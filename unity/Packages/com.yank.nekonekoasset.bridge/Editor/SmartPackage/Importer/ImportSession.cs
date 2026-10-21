// Adapted from Yan-K/AvatarToolbox SmartPackage, commit cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md and SOURCE_PROVENANCE.md.
using System;
using System.Collections.Generic;
using System.IO;

namespace NekoNekoAsset.Bridge.SmartPackage
{
    public enum ImportRunStatus { Succeeded, NothingSelected, PreflightFailed, NeedsReview, PartiallyFailed }
    public enum ImportItemStatus { Applied, IdenticalSkipped, PolicySkipped, Failed, NotAttempted }

    public sealed class ImportItemResult
    {
        public ImportPlanItem Item;
        public ImportItemStatus Status;
        public string TargetPath;
        public string IdentityGuid;
        public string AssetHash;
        public string MetaHash;
        public string Error;
        public bool OwnNewTarget;
        public readonly List<string> CreatedParents = new List<string>();
        public readonly List<string> RecoveryBackups = new List<string>();
    }

    public sealed class ImportRunResult
    {
        public ImportRunStatus Status;
        public int Written;
        public int Updated;
        public int IdenticalSkipped;
        public int PolicySkipped;
        public int Failed;
        public int NotAttempted;
        public string StagingDirectory;
        internal string OwnedStagingDirectory;
        internal string ProjectRoot;
        public readonly List<string> Errors = new List<string>();
        public readonly List<ImportItemResult> Items = new List<ImportItemResult>();
        public int Skipped => IdenticalSkipped + PolicySkipped;
        public bool ShouldClear => Status == ImportRunStatus.Succeeded;
        public bool NeedsReview => Status == ImportRunStatus.NeedsReview;
    }

    public sealed class ImportExecutionOptions
    {
        public string ProjectRoot;
        public string StagingDirectory;
        // Capture on Unity's main thread after the user confirms (and any targeted dirty save).
        public IAssetProbe FreshProbe;
        public Action<ImportPlanItem, int, int> Progress;
        // Bridge persists each applied result here before the next write. A callback failure stops.
        public Action<ImportItemResult> ItemCompleted;
    }

    public static class ImportSession
    {
        public static bool Validate(ImportPlan plan, IAssetProbe freshProbe, out List<string> errors)
        {
            errors = new List<string>();
            if (plan == null) { errors.Add("The import plan is missing."); return false; }
            errors.AddRange(plan.Errors);
            if (freshProbe == null) { errors.Add("A fresh project probe is required."); return false; }
            try
            {
                foreach (KeyValuePair<string, string> pair in plan.Observations.GuidPaths)
                    if (!string.Equals(pair.Value, freshProbe.PathAt(pair.Key) ?? string.Empty, StringComparison.OrdinalIgnoreCase))
                        errors.Add("The reviewed GUID target changed: " + pair.Key);
                foreach (KeyValuePair<string, ObservedAsset> pair in plan.Observations.Paths)
                    if (!pair.Value.Matches(ObservedAsset.Capture(freshProbe, pair.Key)))
                        errors.Add("The reviewed project content changed or could not be compared: " + pair.Key);
                foreach (ImportPlanItem item in plan.OrderedItems)
                {
                    if (item == null || item.Entry == null || item.Package == null)
                    { errors.Add("An import plan entry is missing."); continue; }
                    if (item.Action == ImportPlanAction.Blocked) errors.Add(item.Conflict.Message ?? "Blocked entry.");
                    if (item.Conflict.RequiresDecision && !item.DecisionResolved)
                        errors.Add("An Ask conflict has not been confirmed: " + item.TargetPath);
                    if (!ProjectPackagePath.TryNormalize(item.TargetPath, out _, out string pathError)) errors.Add(pathError);
                    if (item.Package.IsLooseFiles && item.TargetPath != "Assets"
                        && !item.TargetPath.StartsWith("Assets/", StringComparison.Ordinal))
                        errors.Add("Loose file target must stay inside Assets: " + item.TargetPath);
                }
            }
            catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException || ex is ArgumentException)
            { errors.Add("Could not recheck import targets: " + ex.Message); }
            return errors.Count == 0;
        }

        // No dialog, dirty save, Refresh or AssetDatabase import occurs here. The Bridge owns
        // its durable apply-started checkpoint before calling this method and its refresh afterward.
        public static ImportRunResult Apply(ImportPlan plan, ImportExecutionOptions options)
        {
            var result = new ImportRunResult();
            if (plan == null || options == null)
            { result.Status = ImportRunStatus.PreflightFailed; result.Errors.Add("Missing import plan/options."); return result; }
            if (!plan.CanApply)
            { result.Status = ImportRunStatus.PreflightFailed; result.Errors.AddRange(plan.Errors); return result; }
            if (plan.OrderedItems.Count == 0)
            { result.Status = ImportRunStatus.NothingSelected; return result; }
            if (!Validate(plan, options.FreshProbe, out List<string> errors))
            { result.Status = ImportRunStatus.NeedsReview; result.Errors.AddRange(errors); return result; }

            FileAssetProbe live;
            try
            {
                ValidateStagingDirectory(options.ProjectRoot, options.StagingDirectory);
                live = new FileAssetProbe(options.ProjectRoot, options.FreshProbe);
                // Refresh-free byte/GUID checks also catch external filesystem edits since Capture.
                foreach (KeyValuePair<string, ObservedAsset> pair in plan.Observations.Paths)
                    if (!pair.Value.Matches(ObservedAsset.Capture(live, pair.Key)))
                        errors.Add("The reviewed target changed on disk: " + pair.Key);
                if (errors.Count > 0)
                { result.Status = ImportRunStatus.NeedsReview; result.Errors.AddRange(errors); return result; }
                result.StagingDirectory = Path.Combine(options.StagingDirectory, "engine-" + Guid.NewGuid().ToString("N"));
                result.OwnedStagingDirectory = result.StagingDirectory;
                result.ProjectRoot = Path.GetFullPath(options.ProjectRoot);
                Directory.CreateDirectory(result.StagingDirectory);
            }
            catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException || ex is ArgumentException)
            { result.Status = ImportRunStatus.PreflightFailed; result.Errors.Add(ex.Message); return result; }

            var expected = new Dictionary<string, ObservedAsset>(plan.Observations.Paths, StringComparer.OrdinalIgnoreCase);
            var staged = new Dictionary<LoadedPackage, Dictionary<string, StagedAssetPayload>>();
            var packageFailures = new Dictionary<LoadedPackage, string>();
            int packageOrder = 0;
            bool stopped = false;
            bool anyFailure = false;
            int index = 0;
            foreach (ImportPlanItem item in plan.OrderedItems)
            {
                var itemResult = new ImportItemResult
                { Item = item, TargetPath = item.TargetPath, IdentityGuid = item.Entry.IdentityGuid,
                  AssetHash = item.Entry.AssetHash, MetaHash = item.Entry.MetaHash, Status = ImportItemStatus.NotAttempted };
                result.Items.Add(itemResult);
                index++;
                if (stopped)
                {
                    itemResult.Status = ImportItemStatus.NotAttempted;
                    result.NotAttempted++;
                    try { options.ItemCompleted?.Invoke(itemResult); }
                    catch (Exception checkpoint) { result.Errors.Add("Result checkpoint failed: " + checkpoint.Message); }
                    continue;
                }
                if (item.Action == ImportPlanAction.SkipByPolicy || item.Action == ImportPlanAction.SkipIdentical)
                {
                    if (item.Action == ImportPlanAction.SkipIdentical)
                    {
                        try
                        {
                            ObservedAsset current = ObservedAsset.Capture(live, item.TargetPath);
                            if (!MatchesIncoming(current, item.Entry)) throw new IOException("The identical target changed after confirmation.");
                            itemResult.IdentityGuid = current.Guid;
                            itemResult.AssetHash = current.Content.AssetHash;
                            itemResult.MetaHash = current.Content.MetaHash;
                        }
                        catch (Exception ex)
                        {
                            itemResult.Status = ImportItemStatus.Failed;
                            itemResult.Error = ex.Message;
                            result.Failed++;
                            result.Errors.Add(item.TargetPath + ": " + ex.Message);
                            anyFailure = true;
                            try { options.ItemCompleted?.Invoke(itemResult); }
                            catch (Exception checkpoint) { stopped = true; result.Errors.Add(checkpoint.Message); }
                            continue;
                        }
                    }
                    itemResult.Status = item.Action == ImportPlanAction.SkipIdentical
                        ? ImportItemStatus.IdenticalSkipped : ImportItemStatus.PolicySkipped;
                    if (itemResult.Status == ImportItemStatus.IdenticalSkipped) result.IdenticalSkipped++;
                    else result.PolicySkipped++;
                    try { options.ItemCompleted?.Invoke(itemResult); }
                    catch (Exception ex) { stopped = anyFailure = true; result.Errors.Add("Result checkpoint failed: " + ex.Message); }
                    continue;
                }
                try
                {
                    options.Progress?.Invoke(item, index - 1, plan.OrderedItems.Count);
                    if (packageFailures.TryGetValue(item.Package, out string packageError)) throw new IOException(packageError);
                    if (!staged.TryGetValue(item.Package, out Dictionary<string, StagedAssetPayload> payloads))
                    {
                        string packageStaging = Path.Combine(result.StagingDirectory, "package-" + packageOrder++);
                        Directory.CreateDirectory(packageStaging);
                        try { payloads = StagePackage(plan, item.Package, packageStaging); }
                        catch (Exception ex) { packageFailures[item.Package] = ex.Message; throw; }
                        staged.Add(item.Package, payloads);
                    }
                    if (!payloads.TryGetValue(item.Entry.Guid, out StagedAssetPayload payload))
                        throw new InvalidDataException("A selected payload is missing from staging.");
                    ValidatePayload(item.Entry, payload);
                    if (!ProjectPackagePath.TryGetAbsolutePath(options.ProjectRoot, item.TargetPath, out string absolute, out string pathError))
                        throw new IOException(pathError);
                    ObservedAsset current = ObservedAsset.Capture(live, item.TargetPath);
                    ObservedAsset prior = expected.TryGetValue(item.TargetPath, out ObservedAsset known) ? known : new ObservedAsset();
                    if (!prior.Matches(current)) throw new IOException("The target changed after confirmation: " + item.TargetPath);
                    if (MatchesIncoming(current, item.Entry))
                    { itemResult.Status = ImportItemStatus.IdenticalSkipped; result.IdenticalSkipped++; }
                    else
                    {
                        itemResult.OwnNewTarget = !plan.Observations.Paths.TryGetValue(item.TargetPath, out ObservedAsset original)
                            ? !current.Exists : !original.Exists;
                        EnsureParents(item.TargetPath, options.ProjectRoot, live, expected, itemResult.CreatedParents);
                        WriteEntry(absolute, item.Entry, payload, itemResult.RecoveryBackups);
                        itemResult.Status = ImportItemStatus.Applied;
                        if (item.Action == ImportPlanAction.Overwrite) result.Updated++; else result.Written++;
                    }
                    ObservedAsset after = ObservedAsset.Capture(live, item.TargetPath);
                    if (!MatchesIncoming(after, item.Entry)) throw new IOException("The committed target could not be verified.");
                    expected[item.TargetPath] = after;
                    itemResult.IdentityGuid = after.Guid;
                    itemResult.AssetHash = after.Content.AssetHash;
                    itemResult.MetaHash = after.Content.MetaHash;
                    // An applied pair is reported before any subsequent item is attempted.
                    try { options.ItemCompleted?.Invoke(itemResult); }
                    catch { stopped = true; throw; }
                }
                catch (Exception ex)
                {
                    anyFailure = true;
                    itemResult.Error = ex.Message;
                    result.Errors.Add(item.TargetPath + ": " + ex.Message);
                    // A receipt/progress exception cannot relabel already-written bytes as absent.
                    if (itemResult.Status != ImportItemStatus.Applied && itemResult.Status != ImportItemStatus.IdenticalSkipped)
                    { itemResult.Status = ImportItemStatus.Failed; result.Failed++; }
                    try { options.ItemCompleted?.Invoke(itemResult); }
                    catch (Exception checkpoint) { stopped = true; result.Errors.Add("Result checkpoint failed: " + checkpoint.Message); }
                }
            }
            try { options.Progress?.Invoke(plan.OrderedItems[plan.OrderedItems.Count - 1], plan.OrderedItems.Count, plan.OrderedItems.Count); }
            catch (Exception ex) { anyFailure = true; result.Errors.Add("Progress update failed: " + ex.Message); }
            result.Status = anyFailure ? ImportRunStatus.PartiallyFailed : ImportRunStatus.Succeeded;
            return result;
        }

        // Call only after the Bridge has durably completed its import refresh/output/GUID checks.
        // Partial results retain streaming inputs and any rollback backups for recovery.
        public static List<string> CleanupSuccessfulRun(ImportRunResult result)
        {
            var errors = new List<string>();
            if (result == null || result.Status != ImportRunStatus.Succeeded) return errors;
            if (string.IsNullOrEmpty(result.OwnedStagingDirectory)) return errors;
            string owned = Path.GetFullPath(result.OwnedStagingDirectory);
            string project = Path.GetFullPath(result.ProjectRoot).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            string libraryPrefix = Path.Combine(project, "Library") + Path.DirectorySeparatorChar;
            if (!owned.StartsWith(libraryPrefix, StringComparison.OrdinalIgnoreCase)
                || !Path.GetFileName(owned).StartsWith("engine-", StringComparison.Ordinal))
            { errors.Add("Refused cleanup outside the engine's owned staging."); return errors; }
            try { ValidateStagingDirectory(project, owned); }
            catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException || ex is ArgumentException)
            { errors.Add(ex.Message); return errors; }
            foreach (ImportItemResult item in result.Items)
                foreach (string backup in item.RecoveryBackups)
                {
                    string absolute = Path.GetFullPath(backup);
                    if (!ProjectPackagePath.TryGetAbsolutePath(project, item.TargetPath, out string target, out _)
                        || (!absolute.StartsWith(target + ".nna-import-", StringComparison.OrdinalIgnoreCase)
                            && !absolute.StartsWith(target + ".meta.nna-import-", StringComparison.OrdinalIgnoreCase))
                        || !absolute.EndsWith(".bak", StringComparison.Ordinal))
                    { errors.Add("Refused cleanup of an unexpected backup."); continue; }
                    try { if (File.Exists(absolute)) File.Delete(absolute); }
                    catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException) { errors.Add(ex.Message); }
                }
            DeleteOwnedTree(owned, owned, errors);
            return errors;
        }

        private static void DeleteOwnedTree(string path, string ownedRoot, List<string> errors)
        {
            string absolute = Path.GetFullPath(path);
            if (absolute != ownedRoot && !absolute.StartsWith(ownedRoot + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            { errors.Add("Refused staging cleanup outside its owned root."); return; }
            try
            {
                if (!Directory.Exists(absolute)) return;
                if ((File.GetAttributes(absolute) & FileAttributes.ReparsePoint) != 0)
                { errors.Add("Refused staging cleanup through a symbolic link or junction."); return; }
                foreach (string child in Directory.GetFileSystemEntries(absolute))
                {
                    FileAttributes attributes = File.GetAttributes(child);
                    if ((attributes & FileAttributes.ReparsePoint) != 0)
                    { errors.Add("Retained unexpected linked staging entry: " + child); continue; }
                    if ((attributes & FileAttributes.Directory) != 0) DeleteOwnedTree(child, ownedRoot, errors);
                    else File.Delete(child);
                }
                if (Directory.GetFileSystemEntries(absolute).Length == 0) Directory.Delete(absolute, false);
            }
            catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException) { errors.Add(ex.Message); }
        }

        private static Dictionary<string, StagedAssetPayload> StagePackage(ImportPlan plan, LoadedPackage package, string directory)
        {
            var wanted = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (ImportPlanItem item in plan.OrderedItems)
                if (item.Package == package && (item.Action == ImportPlanAction.WriteNew || item.Action == ImportPlanAction.Overwrite))
                    wanted.Add(item.Entry.Guid);
            if (!package.IsLooseFiles)
            {
                var info = new FileInfo(package.FilePath);
                if (!info.Exists || info.Length != package.FileLength || info.LastWriteTimeUtc.Ticks != package.FileLastWriteUtcTicks)
                    throw new IOException("The staged package changed since review.");
                Dictionary<string, StagedAssetPayload> result = UnityPackageReader.StageSelected(package.FilePath, wanted, directory);
                info.Refresh();
                if (info.Length != package.FileLength || info.LastWriteTimeUtc.Ticks != package.FileLastWriteUtcTicks)
                    throw new IOException("The staged package changed while reading payloads.");
                return result;
            }
            var loose = new Dictionary<string, StagedAssetPayload>(StringComparer.OrdinalIgnoreCase);
            foreach (UnityPackageEntry entry in package.Entries)
            {
                if (!wanted.Contains(entry.Guid)) continue;
                var payload = new StagedAssetPayload();
                payload.AssetPath = Path.Combine(directory, entry.Guid + ".asset.payload");
                CopyFile(entry.SourceAssetPath, payload.AssetPath);
                payload.AssetHash = LooseFilePackage.HashFile(payload.AssetPath);
                payload.AssetSize = new FileInfo(payload.AssetPath).Length;
                if (entry.HasMetaMember)
                {
                    payload.MetaPath = Path.Combine(directory, entry.Guid + ".meta.payload");
                    CopyFile(entry.SourceMetaPath, payload.MetaPath);
                    payload.MetaHash = LooseFilePackage.HashFile(payload.MetaPath);
                }
                loose.Add(entry.Guid, payload);
            }
            return loose;
        }

        private static void ValidatePayload(UnityPackageEntry entry, StagedAssetPayload payload)
        {
            if (entry.HasAssetMember && (string.IsNullOrEmpty(payload.AssetPath) || payload.AssetSize != entry.Size
                || !string.Equals(payload.AssetHash, entry.AssetHash, StringComparison.Ordinal)))
                throw new InvalidDataException("The staged asset differs from its reviewed payload.");
            if (!entry.HasAssetMember && !entry.IsFolder) throw new InvalidDataException("An asset payload is required.");
            if (!entry.HasMetaMember) return;
            if (string.IsNullOrEmpty(payload.MetaPath) || !string.Equals(payload.MetaHash, entry.MetaHash, StringComparison.Ordinal))
                throw new InvalidDataException("The staged meta differs from its reviewed payload.");
            byte[] meta = File.ReadAllBytes(payload.MetaPath);
            if (!string.Equals(GuidUtility.ExtractGuidFromMeta(meta), entry.MetaGuid, StringComparison.OrdinalIgnoreCase)
                || GuidUtility.MetaDeclaresFolder(meta) != entry.IsFolder)
                throw new InvalidDataException("The staged meta identity or asset shape changed.");
        }

        private static bool MatchesIncoming(ObservedAsset current, UnityPackageEntry entry)
        {
            return current.Exists && current.ContentKnown && current.IsFolder == entry.IsFolder
                && current.Content.HasAsset == (!entry.IsFolder && entry.HasAssetMember)
                && string.Equals(current.Content.AssetHash, entry.IsFolder ? null : entry.AssetHash, StringComparison.Ordinal)
                && (entry.PreserveExistingMeta || (current.Content.HasMeta == entry.HasMetaMember
                    && string.Equals(current.Content.MetaHash, entry.MetaHash, StringComparison.Ordinal)));
        }

        private sealed class FilePart
        {
            public string Target;
            public string Source;
            public string ExpectedHash;
            public string Staged;
            public string Backup;
            public bool HadOriginal;
            public bool OriginalMoved;
            public bool NewInstalled;
        }

        // Archive order need not put folders before children. Advance only observations
        // for directories created by this writer, retaining metadata/content checks.
        private static void EnsureParents(string target, string root, FileAssetProbe live,
            Dictionary<string, ObservedAsset> expected, List<string> createdParents)
        {
            string[] segments = target.Split('/');
            string path = segments[0];
            for (int i = 1; i < segments.Length; i++)
            {
                ObservedAsset current = ObservedAsset.Capture(live, path);
                if (expected.TryGetValue(path, out ObservedAsset prior) && !prior.Matches(current))
                    throw new IOException("A parent changed after confirmation: " + path);
                if (!current.Exists)
                {
                    if (!ProjectPackagePath.TryGetAbsolutePath(root, path, out string absolute, out string error))
                        throw new IOException(error);
                    Directory.CreateDirectory(absolute);
                    ObservedAsset created = ObservedAsset.Capture(live, path);
                    if (!created.IsFolder || !string.IsNullOrEmpty(created.Guid) || created.Content.HasMeta)
                        throw new IOException("A new parent acquired unexpected metadata: " + path);
                    expected[path] = created;
                    createdParents.Add(path);
                }
                else if (!current.IsFolder) throw new IOException("A file occupies an import parent: " + path);
                path += "/" + segments[i];
            }
        }

        private static void WriteEntry(string absolute, UnityPackageEntry entry, StagedAssetPayload payload, List<string> recoveryBackups)
        {
            bool createdFolder = false;
            if (entry.IsFolder)
            {
                if (File.Exists(absolute)) throw new IOException("A file occupies the incoming folder.");
                createdFolder = !Directory.Exists(absolute);
                Directory.CreateDirectory(absolute);
            }
            else
            {
                if (Directory.Exists(absolute)) throw new IOException("A directory occupies the incoming file.");
                Directory.CreateDirectory(Path.GetDirectoryName(absolute));
            }
            var parts = new List<FilePart>();
            if (!entry.IsFolder) parts.Add(new FilePart { Target = absolute, Source = payload.AssetPath, ExpectedHash = entry.AssetHash });
            if (entry.HasMetaMember)
                parts.Add(new FilePart { Target = absolute + ".meta", Source = payload.MetaPath, ExpectedHash = entry.MetaHash });
            else if (!entry.PreserveExistingMeta)
                parts.Add(new FilePart { Target = absolute + ".meta" }); // project-file placeholder removal only
            try { CommitFileParts(parts, recoveryBackups); }
            catch
            {
                if (createdFolder && Directory.Exists(absolute) && Directory.GetFileSystemEntries(absolute).Length == 0)
                    Directory.Delete(absolute, false);
                throw;
            }
        }

        // Original Smart Package per-pair replacement/rollback, with streamed source files.
        private static void CommitFileParts(List<FilePart> parts, List<string> recoveryBackups)
        {
            string operationId = Guid.NewGuid().ToString("N");
            bool committed = false;
            try
            {
                for (int i = 0; i < parts.Count; i++)
                {
                    FilePart part = parts[i];
                    part.Staged = part.Target + ".nna-import-" + operationId + "-" + i + ".tmp";
                    part.Backup = part.Target + ".nna-import-" + operationId + "-" + i + ".bak";
                    part.HadOriginal = File.Exists(part.Target);
                    if (Directory.Exists(part.Target)) throw new IOException("A directory occupies a file/meta target: " + part.Target);
                    if (part.Source == null) continue;
                    CopyFile(part.Source, part.Staged);
                    if (!string.Equals(LooseFilePackage.HashFile(part.Staged), part.ExpectedHash, StringComparison.Ordinal))
                        throw new IOException("The staged replacement changed before commit.");
                }
                foreach (FilePart part in parts)
                    if (part.HadOriginal) { File.Move(part.Target, part.Backup); part.OriginalMoved = true; }
                foreach (FilePart part in parts)
                    if (part.Source != null) { File.Move(part.Staged, part.Target); part.NewInstalled = true; }
                committed = true;
            }
            catch (Exception original)
            {
                var rollbackErrors = new List<string>();
                for (int i = parts.Count - 1; i >= 0; i--)
                {
                    FilePart part = parts[i];
                    try
                    {
                        if (part.NewInstalled && File.Exists(part.Target)) File.Delete(part.Target);
                        if (part.OriginalMoved && File.Exists(part.Backup)) File.Move(part.Backup, part.Target);
                    }
                    catch (Exception rollback) { rollbackErrors.Add(rollback.Message); }
                    if (!string.IsNullOrEmpty(part.Backup) && File.Exists(part.Backup)) recoveryBackups.Add(part.Backup);
                }
                if (rollbackErrors.Count > 0)
                    throw new IOException(original.Message + " Rollback incomplete; retained backups: "
                        + string.Join(", ", recoveryBackups) + ". " + string.Join("; ", rollbackErrors), original);
                throw;
            }
            finally
            {
                foreach (FilePart part in parts)
                {
                    TryDelete(part.Staged);
                    if (committed)
                    {
                        TryDelete(part.Backup);
                        if (!string.IsNullOrEmpty(part.Backup) && File.Exists(part.Backup)) recoveryBackups.Add(part.Backup);
                    }
                }
            }
        }

        private static void CopyFile(string source, string destination)
        {
            using (var input = new FileStream(source, FileMode.Open, FileAccess.Read, FileShare.Read))
            using (var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            { input.CopyTo(output, 64 * 1024); output.Flush(true); }
        }
        private static void TryDelete(string path)
        {
            if (string.IsNullOrEmpty(path)) return;
            try { if (File.Exists(path)) File.Delete(path); }
            catch (IOException) { } catch (UnauthorizedAccessException) { }
        }
        private static void ValidateStagingDirectory(string projectRoot, string stagingDirectory)
        {
            if (string.IsNullOrEmpty(projectRoot) || string.IsNullOrEmpty(stagingDirectory))
                throw new ArgumentException("Project root and bridge staging are required.");
            string root = Path.GetFullPath(projectRoot).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            string library = Path.Combine(root, "Library");
            string staging = Path.GetFullPath(stagingDirectory);
            if (!staging.StartsWith(library + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
                throw new IOException("Engine staging must stay inside this project's Library.");
            string current = root;
            foreach (string segment in staging.Substring(root.Length + 1).Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar))
            {
                current = Path.Combine(current, segment);
                if ((Directory.Exists(current) || File.Exists(current))
                    && (File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                    throw new IOException("Bridge staging cannot follow a symbolic link or junction.");
            }
            Directory.CreateDirectory(staging);
        }
    }
}
