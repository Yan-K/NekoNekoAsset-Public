using System;
using System.Collections.Generic;
using System.IO;

namespace NekoNekoAsset.Bridge.SmartPackage
{
    public sealed class LooseFileInput
    {
        public string SourcePath;
        public string RelativePath;
        public string MetaPath;
        // Only the Bridge preparation path supplies an already verified planner digest.
        internal string VerifiedAssetHash;
    }

    public static class LooseFilePackage
    {
        // Inputs are already copied and verified by the Bridge. Never infer a sidecar from
        // the user's source filesystem: only explicitly supplied meta is incoming meta.
        public static LoadedPackage Create(IEnumerable<LooseFileInput> inputs, long dropOrder)
        {
            var package = new LoadedPackage { IsLooseFiles = true, FilePath = "Loose files", DropOrder = dropOrder };
            if (inputs == null) throw new ArgumentNullException(nameof(inputs));
            foreach (LooseFileInput input in inputs)
            {
                if (input == null || string.IsNullOrEmpty(input.SourcePath))
                    throw new InvalidDataException("A loose file is missing its staged source.");
                if (!ProjectPackagePath.TryNormalize("Assets/" + input.RelativePath, out string path, out string error))
                    throw new InvalidDataException(error);
                // The source hierarchy identifies the selected input, not its Unity destination.
                string filename = Path.GetFileName(path);
                string source = Path.GetFullPath(input.SourcePath);
                if (!File.Exists(source)) throw new FileNotFoundException("The staged loose file is missing.", source);
                var entry = new UnityPackageEntry
                {
                    Guid = System.Guid.NewGuid().ToString("N"),
                    AssetPath = "Assets/" + filename,
                    LooseRelativePath = filename,
                    SourceAssetPath = source,
                    HasAssetMember = true,
                    HasPathnameMember = true,
                    IsPathManaged = true,
                    PreserveExistingMeta = true,
                    EntryOrder = package.Entries.Count,
                    Size = new FileInfo(source).Length,
                    AssetHash = input.VerifiedAssetHash ?? HashFile(source)
                };
                if (!string.IsNullOrEmpty(input.MetaPath))
                {
                    string metaPath = Path.GetFullPath(input.MetaPath);
                    var metaInfo = new FileInfo(metaPath);
                    if (!metaInfo.Exists || metaInfo.Length > 64L * 1024L * 1024L)
                        throw new InvalidDataException("The explicitly supplied meta is missing or too large.");
                    byte[] meta = File.ReadAllBytes(metaPath);
                    entry.MetaGuid = GuidUtility.ExtractGuidFromMeta(meta);
                    if (!GuidUtility.IsValidGuid(entry.MetaGuid) || GuidUtility.MetaDeclaresFolder(meta))
                        throw new InvalidDataException("A loose file's explicit meta needs a valid file GUID.");
                    entry.Guid = entry.MetaGuid;
                    entry.SourceMetaPath = metaPath;
                    entry.HasMetaMember = true;
                    entry.IsPathManaged = false;
                    entry.PreserveExistingMeta = false;
                    entry.MetaHash = GuidUtility.ComputeSha256(meta);
                }
                package.Entries.Add(entry);
            }
            package.Tree = PackageAssetNode.BuildProjectTree(package.Entries);
            package.LeafCount = package.Tree.CountLeaves();
            return package;
        }

        public static void RemapDestination(LoadedPackage package, string destination)
        {
            if (package == null || !package.IsLooseFiles) throw new ArgumentException("Expected loose files.");
            if (!ProjectPackagePath.TryNormalize(destination, out string normalized, out string error)
                || (normalized != "Assets" && !normalized.StartsWith("Assets/", StringComparison.Ordinal)))
                throw new InvalidDataException(error ?? "Loose files must go inside Assets.");
            var checkedPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            if (package.Tree != null)
                foreach (PackageAssetNode leaf in package.Tree.EnumerateCheckedLeaves()) checkedPaths.Add(leaf.FullPath);
            var selection = new Dictionary<UnityPackageEntry, bool>();
            foreach (UnityPackageEntry entry in package.Entries)
            {
                selection[entry] = checkedPaths.Contains(entry.AssetPath);
                entry.AssetPath = normalized + "/" + entry.LooseRelativePath;
            }
            package.LooseDestination = normalized;
            package.Tree = PackageAssetNode.BuildProjectTree(package.Entries);
            var checkedNewPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (KeyValuePair<UnityPackageEntry, bool> pair in selection)
                if (pair.Value) checkedNewPaths.Add(pair.Key.AssetPath);
            foreach (PackageAssetNode leaf in EnumerateAllLeaves(package.Tree))
                leaf.SetChecked(checkedNewPaths.Contains(leaf.FullPath));
            package.TallyVersion = -1;
        }

        private static IEnumerable<PackageAssetNode> EnumerateAllLeaves(PackageAssetNode node)
        {
            if (!node.IsFolder || (node.Children.Count == 0 && node.HasPackageEntry)) { yield return node; yield break; }
            foreach (PackageAssetNode child in node.Children)
                foreach (PackageAssetNode leaf in EnumerateAllLeaves(child)) yield return leaf;
        }

        internal static string HashFile(string path)
        {
            using (var input = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read))
                return GuidUtility.ComputeSha256(input);
        }
    }
}
