using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Diagnostics;
using System.Threading.Tasks;
using NekoNekoAsset.Bridge.SmartPackage;
using UnityEditor;
using UnityEditor.Compilation;
using UnityEngine;

namespace NekoNekoAsset.Bridge
{
    public static partial class BridgeRuntime
    {
        private static double refreshNotBefore;
        private static double refreshStableSince = -1;

        private static List<LoadedPackage> LoadInputs(StoredBridgeRequest request)
        {
            var manifest = JsonUtility.FromJson<BridgeManifest>(request.manifestJson);
            return PrepareInputs(manifest, InputsDirectory(request), request);
        }

        // Plain filesystem/metadata work; no Unity API or project writes run here.
        private static List<LoadedPackage> PrepareInputs(BridgeManifest manifest, string inputsDirectory, StoredBridgeRequest request)
        {
            var packages = new List<LoadedPackage>();
            long totalBytes = manifest.entries.Sum(entry => entry.bytes), completedBytes = 0;
            var progressClock = Stopwatch.StartNew();
            long nextProgress = 0;
            var files = manifest.entries.Where(e => e.kind == "file").ToList();
            var keys = new Dictionary<string, BridgeEntry>(StringComparer.OrdinalIgnoreCase);
            foreach (BridgeEntry entry in files)
            {
                string key = entry.productId + "\0" + entry.relativePath;
                if (keys.ContainsKey(key)) throw new InvalidDataException("Selected loose paths overlap; choose one version.");
                keys.Add(key, entry);
            }
            foreach (BridgeEntry meta in files.Where(e => e.relativePath.EndsWith(".meta", StringComparison.OrdinalIgnoreCase)))
                if (!keys.ContainsKey(meta.productId + "\0" + meta.relativePath.Substring(0, meta.relativePath.Length - 5)))
                    throw new InvalidDataException("An explicit meta file needs its selected asset.");
            for (int order = 0; order < manifest.entries.Count; ++order)
            {
                BridgeEntry entry = manifest.entries[order];
                if (request.cancelTransfer) throw new OperationCanceledException();
                string staged = BridgeFiles.Under(inputsDirectory, entry.entryId);
                var before = new FileInfo(staged);
                if (!before.Exists || before.Length != entry.bytes)
                    throw new IOException("Verified staged input changed or is missing.");
                long lastWrite = before.LastWriteTimeUtc.Ticks;
                string phase = entry.kind == "unitypackage" ? "readPackage" : "verify";
                RecordPreparationProgress(request, phase, completedBytes, totalBytes, order, manifest.entries.Count);
                Action<long> progress = bytes => {
                    if (bytes < 0 || bytes > entry.bytes) throw new IOException("Verified staged input size changed during preparation.");
                    if (progressClock.ElapsedMilliseconds < nextProgress && bytes != entry.bytes) return;
                    nextProgress = progressClock.ElapsedMilliseconds + 100;
                    RecordPreparationProgress(request, phase, completedBytes + bytes, totalBytes, order, manifest.entries.Count);
                };
                if (entry.kind == "unitypackage")
                {
                    LoadedPackage package = LoadedPackage.LoadPackage(staged, order, entry.sha256, progress, () => request.cancelTransfer);
                    package.DisplayName = entry.displayName;
                    packages.Add(package);
                }
                else
                {
                    string verifiedHash = BridgeFiles.HashFile(staged, progress, () => request.cancelTransfer);
                    if (verifiedHash != entry.sha256)
                        throw new IOException("Verified staged input changed or is missing.");
                    if (entry.relativePath.EndsWith(".meta", StringComparison.OrdinalIgnoreCase))
                    {
                        before.Refresh();
                        if (!before.Exists || before.Length != entry.bytes || before.LastWriteTimeUtc.Ticks != lastWrite)
                            throw new IOException("Verified staged input changed during preparation.");
                        completedBytes += entry.bytes;
                        RecordPreparationProgress(request, phase, completedBytes, totalBytes, order + 1, manifest.entries.Count);
                        continue;
                    }
                    if (request.cancelTransfer) throw new OperationCanceledException();
                    keys.TryGetValue(entry.productId + "\0" + entry.relativePath + ".meta", out BridgeEntry meta);
                    var digest = new byte[32];
                    for (int index = 0; index < digest.Length; ++index)
                        digest[index] = Convert.ToByte(verifiedHash.Substring(index * 2, 2), 16);
                    LoadedPackage package = LooseFilePackage.Create(new[] { new LooseFileInput {
                        SourcePath = staged, RelativePath = entry.relativePath,
                        MetaPath = meta == null ? null : BridgeFiles.Under(inputsDirectory, meta.entryId),
                        VerifiedAssetHash = Convert.ToBase64String(digest)
                    } }, order);
                    package.DisplayName = entry.relativePath;
                    packages.Add(package);
                }
                before.Refresh();
                if (!before.Exists || before.Length != entry.bytes || before.LastWriteTimeUtc.Ticks != lastWrite)
                    throw new IOException("Verified staged input changed during preparation.");
                completedBytes += entry.bytes;
                RecordPreparationProgress(request, phase, completedBytes, totalBytes, order + 1, manifest.entries.Count);
            }
            return packages;
        }

        private static void RecordPreparationProgress(StoredBridgeRequest request, string phase,
            long bytes, long totalBytes, int entries, int totalEntries, bool waitingForEditor = false)
        {
            request.preparationProgress = new BridgePreparationProgress {
                phase = phase, waitingForEditor = waitingForEditor,
                bytes = bytes, totalBytes = totalBytes, entries = entries, totalEntries = totalEntries
            };
        }

        private static string Locale(StoredBridgeRequest request = null)
        {
            if (request != null)
            {
                string locale = JsonUtility.FromJson<BridgeManifest>(request.manifestJson)?.locale;
                if (!string.IsNullOrEmpty(locale)) return locale;
            }
            switch (Application.systemLanguage)
            {
                case SystemLanguage.Japanese: return "ja";
                case SystemLanguage.Korean: return "ko";
                case SystemLanguage.ChineseTraditional: return "zh-Hant";
                case SystemLanguage.Chinese:
                case SystemLanguage.ChineseSimplified: return "zh-Hans";
                default: return "en";
            }
        }

        private static void ShowReview(StoredBridgeRequest request)
        {
            if (stopped || request == null || active != request || !registry.enabled
                || (request.receipt.stage != "preparing" && request.receipt.stage != "waitingForUser")) return;
            if (request.preparingReview) return;
            try
            {
                if (request.receipt.stage != "preparing")
                {
                    request.receipt.stage = "preparing";
                    Save(request);
                }
                if (EditorApplication.isCompiling || EditorApplication.isUpdating)
                {
                    RecordPreparationProgress(request, null, 0,
                        request.receipt.preparationTotalBytes, 0, 0, waitingForEditor: true);
                    PublishPreparationProgress(request, true);
                    return;
                }
                if (request.preparedPackages == null)
                {
                    var manifest = JsonUtility.FromJson<BridgeManifest>(request.manifestJson);
                    string inputsDirectory = InputsDirectory(request);
                    request.preparingReview = true;
                    Task.Run(() => PrepareInputs(manifest, inputsDirectory, request)).ContinueWith(prepared => MainActions.Enqueue(() => {
                        if (!AcceptPreparationResult(request, prepared)) return;
                        request.preparedPackages = prepared.Result;
                        PublishPreparationProgress(request, true);
                        ShowReview(request);
                    }));
                    return;
                }
                if (request.preparedPreview == null)
                {
                    RecordPreparationProgress(request, "checkTargets", request.receipt.preparationTotalBytes,
                        request.receipt.preparationTotalBytes, 0, request.preparedPackages.Sum(package => package.Entries.Count));
                    PublishPreparationProgress(request, true);
                    // AssetDatabase stays on the main thread. Target hashing/planning can use its read-only snapshot in the worker.
                    request.preparedSnapshot = SnapshotAssetProbe.Capture(true, cancelled: () => request.cancelTransfer);
                    request.preparedPolicy = ImportReviewWindow.SavedPolicy();
                    request.preparingReview = true;
                    List<LoadedPackage> preparedPackages = request.preparedPackages;
                    SnapshotAssetProbe preparedSnapshot = request.preparedSnapshot;
                    ConflictPolicy preparedPolicy = request.preparedPolicy;
                    var planClock = Stopwatch.StartNew();
                    long nextProgress = 0;
                    Task.Run(() => ImportPlanBuilder.Build(preparedPackages, preparedSnapshot, preparedPolicy,
                        progress: (entries, total) => {
                            if (request.cancelTransfer) throw new OperationCanceledException();
                            if (planClock.ElapsedMilliseconds < nextProgress && entries != total) return;
                            nextProgress = planClock.ElapsedMilliseconds + 100;
                            var current = request.preparationProgress;
                            RecordPreparationProgress(request, "checkTargets", current.bytes, current.totalBytes, entries, total);
                        })).ContinueWith(prepared => MainActions.Enqueue(() => {
                            if (!AcceptPreparationResult(request, prepared)) return;
                            request.preparedPreview = prepared.Result;
                            ShowReview(request);
                        }));
                    return;
                }
                // Batch checks observe actual preparation readiness, never claim a rendered native window.
                if (!Application.isBatchMode)
                    review = ImportReviewWindow.Show(request.preparedPackages, Locale(request),
                        selection => { review = null; if (!stopped) ApplySelection(request, selection); },
                        () => { review = null; if (!stopped) CancelRequest(request); },
                        errorMessage: request.receipt.error,
                        preparedSnapshot: request.preparedSnapshot, preparedPreview: request.preparedPreview,
                        preparedPolicy: request.preparedPolicy);
                PublishPreparationProgress(request, true);
                request.receipt.preparationWaitingForEditor = false;
                request.receipt.stage = "waitingForUser";
                Save(request);
            }
            catch (Exception) { Fail(request, "Could not prepare verified package/file review."); }
        }

        private static bool AcceptPreparationResult<T>(StoredBridgeRequest request, Task<T> prepared)
        {
            if (prepared.IsFaulted) _ = prepared.Exception; // No incoming content/paths enter logs.
            request.preparingReview = false;
            if (Terminal(request.receipt.stage) && !request.receipt.writesStarted) TryCleanupInputs(request);
            if (stopped || active != request || !registry.enabled || Terminal(request.receipt.stage)) return false;
            if (prepared.IsFaulted || prepared.IsCanceled)
            { Fail(request, "Could not prepare verified package/file review."); return false; }
            return true;
        }

        private static void PublishPreparationProgress(StoredBridgeRequest request, bool force = false)
        {
            BridgePreparationProgress progress = request.preparationProgress;
            if (progress == null || (!force && EditorApplication.timeSinceStartup < request.nextPreparationPublish)) return;
            request.nextPreparationPublish = EditorApplication.timeSinceStartup + .5;
            BridgeReceipt receipt = request.receipt;
            if (receipt.preparationPhase == progress.phase && receipt.preparationWaitingForEditor == progress.waitingForEditor
                && receipt.preparationBytes == progress.bytes
                && receipt.preparationTotalBytes == progress.totalBytes && receipt.preparationEntries == progress.entries
                && receipt.preparationTotalEntries == progress.totalEntries) return;
            receipt.preparationPhase = progress.phase;
            receipt.preparationWaitingForEditor = progress.waitingForEditor;
            receipt.preparationBytes = progress.bytes;
            receipt.preparationTotalBytes = progress.totalBytes;
            receipt.preparationEntries = progress.entries;
            receipt.preparationTotalEntries = progress.totalEntries;
            Save(request);
        }

        private static void ApplySelection(StoredBridgeRequest request, ImportReviewSelection selection)
        {
            if (active != request || stopped || !registry.enabled || Terminal(request.receipt.stage)) return;
            if (EditorApplication.isCompiling || EditorApplication.isUpdating)
            { EditorApplication.delayCall += () => ApplySelection(request, selection); return; }
            bool reloadLocked = false;
            bool editing = false;
            try
            {
                if (!FitsReceipt(request, selection.Plan))
                {
                    ReopenReview(request, selection, new BridgeLocalization(Locale(request)).Text("receiptTooLarge"));
                    return;
                }
                SnapshotAssetProbe.SaveDirtyTargets(selection.Packages, out HashSet<string> unknown);
                SnapshotAssetProbe fresh = SnapshotAssetProbe.Capture(true, unknown);
                if (!ImportSession.Validate(selection.Plan, fresh, out List<string> errors))
                {
                    ReopenReview(request, selection, new BridgeLocalization(Locale(request)).Text("planChanged"));
                    return;
                }
                // Write intent is durable before the first asset/meta replacement.
                request.receipt.writesStarted = selection.Plan.WriteCount > 0;
                request.receipt.stage = "importing";
                request.receipt.error = null;
                request.applyReloadGeneration = reloadGeneration;
                Save(request);
                request.preparedPackages = null;
                request.preparedSnapshot = null;
                request.preparedPreview = null;
                EditorApplication.LockReloadAssemblies();
                reloadLocked = true;
                AssetDatabase.StartAssetEditing();
                editing = true;
                ImportRunResult result = ImportSession.Apply(selection.Plan, new ImportExecutionOptions {
                    ProjectRoot = registry.projectRoot,
                    StagingDirectory = RequestDirectory(request.receipt.requestId),
                    FreshProbe = fresh,
                    Progress = (item, index, total) => {
                        if (!Application.isBatchMode) EditorUtility.DisplayProgressBar(new BridgeLocalization(Locale(request)).Text("importing"), index + " / " + total + "  " + item.TargetPath,
                            total == 0 ? 1 : (float)index / total);
                    },
                    ItemCompleted = item => RecordItem(request, item, fresh)
                });
                request.engineDirectory = result.OwnedStagingDirectory;
                foreach (ImportItemResult item in result.Items)
                    foreach (string backup in item.RecoveryBackups)
                        request.engineBackups.Add(new BridgeBackup { targetPath = item.TargetPath, backupPath = backup });
                if (result.NeedsReview)
                {
                    request.receipt.writesStarted = false;
                    ReopenReview(request, selection, new BridgeLocalization(Locale(request)).Text("planChanged"));
                    return;
                }
                request.appliedFailed = result.Status == ImportRunStatus.PartiallyFailed || result.Status == ImportRunStatus.PreflightFailed;
                if (request.appliedFailed) request.receipt.error = "Some files were not imported; inspect the per-file results.";
                request.receipt.stage = "refreshing";
                Save(request); // Checkpoint precedes refresh/reload and survives process exit.
                refreshNotBefore = EditorApplication.timeSinceStartup + 1;
                refreshStableSince = -1;
            }
            catch (Exception)
            {
                request.appliedFailed = true;
                request.receipt.error = "Import interrupted; applied entries remain and backups are retained.";
                request.receipt.stage = "interrupted";
                Save(request);
                if (active == request) active = null;
            }
            finally
            {
                try { if (editing) AssetDatabase.StopAssetEditing(); }
                finally
                {
                    if (reloadLocked) EditorApplication.UnlockReloadAssemblies();
                    if (!Application.isBatchMode) EditorUtility.ClearProgressBar();
                }
            }
            if (active == request && request.receipt.stage == "refreshing")
                AssetDatabase.Refresh(ImportAssetOptions.ForceSynchronousImport);
            if (Terminal(request.receipt.stage)) ShowImportResult(request);
            Publish();
        }

        private static void RecordItem(StoredBridgeRequest request, ImportItemResult item, IAssetProbe before)
        {
            string entryId = Path.GetFileName(string.IsNullOrEmpty(item.Item.Entry.SourceAssetPath)
                ? item.Item.Package.FilePath : item.Item.Entry.SourceAssetPath);
            string state = item.Status == ImportItemStatus.Applied ? "applied"
                : item.Status == ImportItemStatus.IdenticalSkipped || item.Status == ImportItemStatus.PolicySkipped ? "skipped" : "failed";
            string message = item.Status == ImportItemStatus.NotAttempted ? "Not attempted after an earlier failure." : item.Error;
            message = BridgeFiles.Diagnostic(message, 256);
            PutResult(request, new BridgeFileResult { entryId = entryId, path = item.TargetPath, state = state, message = message });
            if (!string.IsNullOrEmpty(item.Item.Entry.SourceMetaPath))
            {
                string metaId = Path.GetFileName(item.Item.Entry.SourceMetaPath);
                if (BridgeFiles.IsUuid(metaId))
                    PutResult(request, new BridgeFileResult { entryId = metaId, path = item.TargetPath + ".meta", state = state, message = message });
            }
            if (item.Status == ImportItemStatus.Applied || item.Status == ImportItemStatus.IdenticalSkipped)
            {
                request.outputs.RemoveAll(output => string.Equals(output.path, item.TargetPath, StringComparison.OrdinalIgnoreCase));
                request.outputs.Add(new BridgeOutputCheck { path = item.TargetPath, assetHash = item.AssetHash,
                    guid = string.IsNullOrEmpty(item.IdentityGuid) ? null : item.IdentityGuid,
                    isFolder = item.Item.Entry.IsFolder });
            }
            if (item.Status == ImportItemStatus.Applied
                && (item.TargetPath.StartsWith("Assets/", StringComparison.Ordinal) || item.TargetPath.StartsWith("Packages/", StringComparison.Ordinal)))
            {
                string extension = Path.GetExtension(item.TargetPath).ToLowerInvariant();
                if (extension == ".cs" || extension == ".dll")
                {
                    bool changed = !before.TryGetContent(item.TargetPath, out AssetContentFingerprint old)
                        || !old.HasAsset || old.AssetHash != item.AssetHash;
                    if (changed)
                    {
                        List<string> paths = extension == ".cs" ? request.changedScripts : request.changedPlugins;
                        if (!paths.Contains(item.TargetPath)) paths.Add(item.TargetPath);
                    }
                    else (extension == ".cs" ? request.changedScripts : request.changedPlugins).Remove(item.TargetPath);
                }
            }
            foreach (string parent in item.CreatedParents)
                if (!request.createdParents.Contains(parent)) request.createdParents.Add(parent);
            if (item.Status == ImportItemStatus.Applied)
            {
                request.ownedWrites.RemoveAll(write => string.Equals(write.path, item.TargetPath, StringComparison.OrdinalIgnoreCase));
                if (item.OwnNewTarget)
                    request.ownedWrites.Add(new BridgeOwnedWrite { path = item.TargetPath, assetHash = item.AssetHash,
                        metaHash = item.MetaHash, isFolder = item.Item.Entry.IsFolder,
                        generatedMeta = item.Item.Package.IsLooseFiles && !item.Item.Entry.HasMetaMember });
            }
            Save(request); // Each completed pair is independently durable; never promise whole-request rollback.
        }

        private static void CompilationStarted(object context)
        {
            if (active == null || (active.receipt.stage != "refreshing" && active.receipt.stage != "compiling")) return;
            active.compileObserved = true;
            active.compileFinished = false;
            active.receipt.stage = "compiling";
            Save(active);
        }

        private static void AssemblyCompiled(string assemblyPath, CompilerMessage[] messages)
        {
            if (active == null || !active.compileObserved || active.receipt.stage != "compiling") return;
            active.compiledAssemblyCount++;
            foreach (CompilerMessage message in messages)
                if (message.type == CompilerMessageType.Error && active.receipt.compileErrors.Count < 50)
                {
                    string text = message.message ?? "Unity compilation error.";
                    active.receipt.compileErrors.Add(BridgeFiles.Diagnostic(text, 1024));
                }
            Save(active);
        }

        private static void CompilationFinished(object context)
        {
            if (active == null || active.receipt.stage != "compiling") return;
            active.compileFinished = true;
            Save(active);
            refreshNotBefore = EditorApplication.timeSinceStartup + 1;
            refreshStableSince = -1; // Compilation event alone never declares completion.
        }

        private static void ObserveRefresh()
        {
            if (active == null || (active.receipt.stage != "refreshing" && active.receipt.stage != "compiling")) return;
            if (EditorApplication.isCompiling || EditorApplication.isUpdating || EditorApplication.timeSinceStartup < refreshNotBefore)
            { refreshStableSince = -1; return; }
            if (refreshStableSince < 0) { refreshStableSince = EditorApplication.timeSinceStartup; return; }
            if (EditorApplication.timeSinceStartup - refreshStableSince < 1) return;
            StoredBridgeRequest request = active;
            try
            {
                bool compileUnconfirmed = request.compileObserved && !request.compileFinished;
                bool reloadUnconfirmed = request.compileObserved && request.compiledAssemblyCount > 0
                    && request.receipt.compileErrors.Count == 0 && reloadGeneration <= request.applyReloadGeneration;
                if (!request.compileObserved && request.changedScripts.Count > 0)
                {
                    // An extension alone does not prove Unity compiles an ignored/excluded file.
                    var sources = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
                    foreach (UnityEditor.Compilation.Assembly assembly in CompilationPipeline.GetAssemblies(AssembliesType.Editor))
                        foreach (string source in assembly.sourceFiles)
                            sources.Add(source.Replace('\\', '/'));
                    foreach (string path in request.changedScripts)
                        if (sources.Contains(path) || sources.Contains(BridgeFiles.Under(registry.projectRoot, path).Replace('\\', '/')))
                            compileUnconfirmed = true;
                }
                if (request.receipt.compileErrors.Count == 0 && reloadGeneration <= request.applyReloadGeneration)
                    foreach (string path in request.changedPlugins)
                        if (AssetImporter.GetAtPath(path) is PluginImporter plugin && !plugin.isNativePlugin && plugin.GetCompatibleWithEditor())
                            reloadUnconfirmed = true;
                if (compileUnconfirmed || reloadUnconfirmed)
                {
                    if (EditorApplication.timeSinceStartup - refreshStableSince < 30) return;
                    request.receipt.stage = "interrupted";
                    request.receipt.error = "Files imported; Unity compilation/reload could not be confirmed. Applied files and backups remain.";
                    Save(request);
                    active = null;
                    ShowImportResult(request);
                    Publish();
                    return;
                }
                foreach (BridgeOutputCheck output in request.outputs)
                {
                    string path = BridgeFiles.Under(registry.projectRoot, output.path);
                    if (output.isFolder ? !Directory.Exists(path) : !File.Exists(path)) throw new IOException("Imported output missing.");
                    // Smart Package fingerprints are Base64; transport hashes are hexadecimal.
                    if (!output.isFolder && !string.IsNullOrEmpty(output.assetHash)
                        && BridgeFiles.HashFile(path) != BridgeFiles.Hex(Convert.FromBase64String(output.assetHash)))
                        throw new IOException("Imported output changed during refresh.");
                    if (!string.IsNullOrEmpty(output.guid) && !string.Equals(GuidUtility.ExtractGuidFromMeta(File.ReadAllBytes(path + ".meta")), output.guid, StringComparison.OrdinalIgnoreCase))
                        throw new IOException("Imported GUID changed during refresh.");
                }
                foreach (BridgeOwnedWrite write in request.ownedWrites)
                    if (write.generatedMeta)
                    {
                        string meta = BridgeFiles.Under(registry.projectRoot, write.path) + ".meta";
                        if (File.Exists(meta))
                            using (var hash = BridgeSha256.Create())
                                write.metaHash = Convert.ToBase64String(hash.ComputeHash(File.ReadAllBytes(meta)));
                    }
                request.receipt.stage = request.appliedFailed ? "failed" : "completed";
                if (!request.appliedFailed && request.receipt.compileErrors.Count > 0)
                    request.receipt.error = "Files imported; Unity compilation reported errors.";
                Save(request);
                active = null;
            }
            catch (Exception)
            {
                Fail(request, "Could not verify imported outputs after Unity refresh; applied files and backups remain.");
                return;
            }
            // Temp cleanup failures cannot change an already durable verified terminal result.
            if (!request.appliedFailed && request.receipt.compileErrors.Count == 0)
            {
                try { CleanupSuccessfulEngine(request); CleanupInputs(request); }
                catch (IOException) { }
                catch (UnauthorizedAccessException) { }
            }
            ShowImportResult(request);
            Publish();
        }

        private static void PutResult(StoredBridgeRequest request, BridgeFileResult result)
        {
            request.receipt.results.RemoveAll(previous => previous.entryId == result.entryId
                && string.Equals(previous.path, result.path, StringComparison.OrdinalIgnoreCase));
            request.receipt.results.Add(result);
        }

        private static void ReopenReview(StoredBridgeRequest request, ImportReviewSelection selection, string error)
        {
            request.receipt.stage = "preparing";
            request.receipt.error = error;
            request.preparedPackages = selection.Packages.ToList();
            request.preparedSnapshot = null;
            request.preparedPreview = null;
            Save(request);
        }

        private static bool FitsReceipt(StoredBridgeRequest request, ImportPlan plan)
        {
            var worst = new BridgeReceipt { requestId = request.receipt.requestId, fingerprint = request.receipt.fingerprint,
                instanceId = registry.instanceId, sessionId = registry.sessionId, stage = "interrupted", staged = true, writesStarted = true,
                updatedUtc = BridgeFiles.Now(), error = new string('"', 1024) };
            for (int i = 0; i < 50; ++i) worst.compileErrors.Add(new string('"', 1024));
            foreach (ImportPlanItem item in plan.OrderedItems)
            {
                worst.results.Add(new BridgeFileResult { entryId = new string('f', 36), path = item.TargetPath,
                    state = "applied", message = new string('"', 256) });
                if (!string.IsNullOrEmpty(item.Entry.SourceMetaPath))
                    worst.results.Add(new BridgeFileResult { entryId = new string('f', 36), path = item.TargetPath + ".meta",
                        state = "applied", message = new string('"', 256) });
            }
            BridgeResponse response = Response("status", true, request.receipt.requestId);
            response.receipt = worst;
            return BridgeFiles.Utf8.GetByteCount(SerializeResponse(response)) < BridgeFiles.MaximumControlBytes - 4096;
        }

        private static void CleanupSuccessfulEngine(StoredBridgeRequest request)
        {
            if (string.IsNullOrEmpty(request.engineDirectory)) return;
            string name = Path.GetFileName(request.engineDirectory);
            if (!name.StartsWith("engine-", StringComparison.Ordinal)
                || !Guid.TryParseExact(name.Substring(7), "N", out Guid _)
                || Path.GetFullPath(request.engineDirectory) != BridgeFiles.Under(RequestDirectory(request.receipt.requestId), name)) return;
            var run = new ImportRunResult { Status = ImportRunStatus.Succeeded,
                OwnedStagingDirectory = request.engineDirectory, ProjectRoot = registry.projectRoot };
            foreach (BridgeBackup backup in request.engineBackups)
            {
                var item = new ImportItemResult { TargetPath = backup.targetPath };
                item.RecoveryBackups.Add(backup.backupPath);
                run.Items.Add(item);
            }
            ImportSession.CleanupSuccessfulRun(run);
        }

        private static void TryCleanupInputs(StoredBridgeRequest request)
        {
            try { CleanupInputs(request); }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }

        private static void CleanupInputs(StoredBridgeRequest request)
        {
            string root = InputsDirectory(request);
            if (!Directory.Exists(root)) return;
            BridgeFiles.RejectLinks(root);
            foreach (string file in Directory.GetFiles(root))
            {
                if (!BridgeFiles.IsUuid(Path.GetFileName(file))) continue;
                BridgeFiles.RejectLinks(file);
                File.Delete(file);
            }
            if (Directory.GetFileSystemEntries(root).Length == 0) Directory.Delete(root);
        }
    }
}
