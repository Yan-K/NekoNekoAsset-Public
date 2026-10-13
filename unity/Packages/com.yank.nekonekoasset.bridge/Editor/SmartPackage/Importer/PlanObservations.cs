using System;
using System.Collections.Generic;

namespace NekoNekoAsset.Bridge.SmartPackage
{
    public sealed class ObservedAsset
    {
        public string Guid;
        public bool Exists;
        public bool IsFolder;
        public bool ContentKnown;
        public AssetContentFingerprint Content;

        public static ObservedAsset Capture(IAssetProbe probe, string path)
        {
            var result = new ObservedAsset
            { Guid = probe.GuidAt(path) ?? string.Empty, Exists = probe.ExistsAt(path), IsFolder = probe.IsFolderAt(path) };
            result.ContentKnown = probe.TryGetContent(path, out result.Content);
            return result;
        }

        public bool Matches(ObservedAsset other)
        {
            if (other == null || Exists != other.Exists || IsFolder != other.IsFolder
                || !string.Equals(Guid, other.Guid, StringComparison.OrdinalIgnoreCase)) return false;
            if (!Exists) return true;
            return ContentKnown && other.ContentKnown
                && Content.HasAsset == other.Content.HasAsset && Content.HasMeta == other.Content.HasMeta
                && string.Equals(Content.AssetHash, other.Content.AssetHash, StringComparison.Ordinal)
                && string.Equals(Content.MetaHash, other.Content.MetaHash, StringComparison.Ordinal);
        }
    }

    public sealed class PlanObservations
    {
        public readonly Dictionary<string, ObservedAsset> Paths = new Dictionary<string, ObservedAsset>(StringComparer.OrdinalIgnoreCase);
        public readonly Dictionary<string, string> GuidPaths = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
    }

    // Record what the user actually reviewed, including misses and GUID moves. No project writes.
    internal sealed class RecordingAssetProbe : IAssetProbe
    {
        private readonly IAssetProbe inner;
        private readonly PlanObservations observations;
        public RecordingAssetProbe(IAssetProbe inner, PlanObservations observations)
        { this.inner = inner; this.observations = observations; }
        private ObservedAsset Observe(string path)
        {
            if (string.IsNullOrEmpty(path)) return new ObservedAsset();
            if (!observations.Paths.TryGetValue(path, out ObservedAsset state))
            { state = ObservedAsset.Capture(inner, path); observations.Paths.Add(path, state); }
            return state;
        }
        public string GuidAt(string path) => Observe(path).Guid;
        public string PathAt(string guid)
        {
            if (string.IsNullOrEmpty(guid)) return null;
            if (!observations.GuidPaths.TryGetValue(guid, out string path))
            { path = inner.PathAt(guid) ?? string.Empty; observations.GuidPaths.Add(guid, path); }
            if (!string.IsNullOrEmpty(path)) Observe(path);
            return path;
        }
        public bool ExistsAt(string path) => Observe(path).Exists;
        public bool IsFolderAt(string path) => Observe(path).IsFolder;
        public bool TryGetContent(string path, out AssetContentFingerprint content)
        { ObservedAsset state = Observe(path); content = state.Content; return state.ContentKnown; }
    }
}
