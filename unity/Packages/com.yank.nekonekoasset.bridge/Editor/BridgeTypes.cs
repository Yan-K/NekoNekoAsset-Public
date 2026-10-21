using System;
using System.Collections.Generic;

namespace NekoNekoAsset.Bridge
{
    [Serializable]
    public sealed class BridgeRegistry
    {
        public int version = 1;
        public string instanceId;
        public string sessionId;
        public int processId;
        public string projectName;
        public string projectRoot;
        public string unityVersion;
        public string bridgeVersion;
        public string host = "127.0.0.1";
        public int port;
        public string capability;
        public string heartbeatUtc;
        public bool enabled;
        public bool busy;
    }

    [Serializable]
    public sealed class BridgeCommand
    {
        public int version;
        public string command;
        public string capability;
        public string instanceId;
        public string sessionId;
        public string requestId;
        public string fingerprint;
        public string manifestJson;
    }

    [Serializable]
    public sealed class BridgeManifest
    {
        public int version;
        public string locale;
        public string instanceId;
        public string sessionId;
        public List<BridgeEntry> entries;
    }

    [Serializable]
    public sealed class BridgeEntry
    {
        public string entryId;
        public string kind;
        public string displayName;
        public string relativePath;
        public long bytes;
        public string sha256;
        public string productId;
        public string fileId;
        public string revision;
        public string memberPath;
    }

    [Serializable]
    public sealed class BridgeResponse
    {
        public int version = 1;
        public bool ok;
        public string code;
        public string message;
        public string instanceId;
        public string sessionId;
        public string requestId;
        // JsonUtility expands null inline classes into empty objects. The wire
        // serializer appends only real receipts so no-request responses omit it.
        [NonSerialized] public BridgeReceipt receipt;
        public string projectName;
        public string projectRoot;
        public string unityVersion;
        public string bridgeVersion;
        public bool enabled;
        public bool busy;
    }

    [Serializable]
    public sealed class BridgeReceipt
    {
        public string requestId;
        public string fingerprint;
        public string instanceId;
        public string sessionId;
        public string stage;
        public bool staged;
        public bool writesStarted;
        public string updatedUtc;
        public string preparationPhase;
        public bool preparationWaitingForEditor;
        public long preparationBytes;
        public long preparationTotalBytes;
        public int preparationEntries;
        public int preparationTotalEntries;
        public string error;
        // Retained for v1 compatibility; new receipts leave this empty and outgoing
        // receipts never include historical compiler diagnostics.
        public List<string> compileErrors = new List<string>();
        public List<BridgeFileResult> results = new List<BridgeFileResult>();
    }

    [Serializable]
    public sealed class BridgeFileResult
    {
        public string entryId;
        public string path;
        public string state;
        public string message;
    }

    // Stored only in Unity Library, never exposed as a catalogue or sync format.
    [Serializable]
    internal sealed class StoredBridgeRequest
    {
        [NonSerialized] internal string lastDurableJson;
        [NonSerialized] internal volatile bool cancelTransfer;
        [NonSerialized] internal bool preparingReview;
        [NonSerialized] internal List<SmartPackage.LoadedPackage> preparedPackages;
        [NonSerialized] internal SmartPackage.SnapshotAssetProbe preparedSnapshot;
        [NonSerialized] internal SmartPackage.ImportPlan preparedPreview;
        [NonSerialized] internal SmartPackage.ConflictPolicy preparedPolicy;
        [NonSerialized] internal volatile BridgePreparationProgress preparationProgress;
        [NonSerialized] internal double nextPreparationPublish;
        public int version = 1;
        public string manifestJson;
        public BridgeReceipt receipt;
        public List<BridgeOutputCheck> outputs = new List<BridgeOutputCheck>();
        public List<BridgeOwnedWrite> ownedWrites = new List<BridgeOwnedWrite>();
        public List<string> createdParents = new List<string>();
        public bool cleanupDone;
        public bool appliedFailed;
        public string engineDirectory;
        public List<BridgeBackup> engineBackups = new List<BridgeBackup>();
    }

    internal sealed class BridgePreparationProgress
    {
        internal string phase;
        internal bool waitingForEditor;
        internal long bytes;
        internal long totalBytes;
        internal int entries;
        internal int totalEntries;
    }

    [Serializable]
    internal sealed class BridgeOwnedWrite
    {
        public string path;
        public string assetHash;
        public string metaHash;
        public bool isFolder;
        public bool generatedMeta;
    }

    [Serializable]
    internal sealed class BridgeBackup
    {
        public string targetPath;
        public string backupPath;
    }

    [Serializable]
    internal sealed class BridgeOutputCheck
    {
        public string path;
        public string assetHash;
        public string guid;
        public bool isFolder;
    }
}
