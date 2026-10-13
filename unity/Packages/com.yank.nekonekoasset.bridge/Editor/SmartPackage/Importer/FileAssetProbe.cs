using System;
using System.IO;

namespace NekoNekoAsset.Bridge.SmartPackage
{
    // Read current bytes for the writer's last check; AssetDatabase is intentionally not refreshed here.
    public sealed class FileAssetProbe : IAssetProbe
    {
        private readonly string projectRoot;
        private readonly IAssetProbe identities;
        public FileAssetProbe(string projectRoot, IAssetProbe identities = null)
        { this.projectRoot = projectRoot; this.identities = identities; }
        public string PathAt(string guid) => identities == null ? null : identities.PathAt(guid);
        public string GuidAt(string path)
        {
            if (ProjectPackagePath.IsPathManaged(path, true)) return string.Empty;
            string absolute = Absolute(path);
            if (absolute == null || !File.Exists(absolute + ".meta")) return string.Empty;
            var info = new FileInfo(absolute + ".meta");
            if (info.Length > 64L * 1024L * 1024L) throw new InvalidDataException("Existing meta is too large.");
            return GuidUtility.ExtractGuidFromMeta(File.ReadAllBytes(info.FullName)) ?? string.Empty;
        }
        public bool ExistsAt(string path)
        {
            string absolute = Absolute(path);
            return absolute != null && (File.Exists(absolute) || Directory.Exists(absolute) || File.Exists(absolute + ".meta"));
        }
        public bool IsFolderAt(string path)
        { string absolute = Absolute(path); return absolute != null && Directory.Exists(absolute); }
        public bool TryGetContent(string path, out AssetContentFingerprint content)
            => SnapshotAssetProbe.TryReadContent(projectRoot, path, out content);
        private string Absolute(string path)
            => ProjectPackagePath.TryGetAbsolutePath(projectRoot, path, out string absolute, out _) ? absolute : null;
    }
}
