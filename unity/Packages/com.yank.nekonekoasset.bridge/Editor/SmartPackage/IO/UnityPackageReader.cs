// Adapted from Yan-K/AvatarToolbox SmartPackage, commit cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md and SOURCE_PROVENANCE.md.
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Security.Cryptography;

namespace NekoNekoAsset.Bridge.SmartPackage
{
	public static class UnityPackageReader
	{
		private const long MaxPathnameBytes = 1024L * 1024L;
		private const long MaxMetaBytes = 64L * 1024L * 1024L;

		// Read path/GUID metadata and stream asset fingerprints. For received packages,
		// verify the compressed bytes during this same scan rather than reading them twice.
		public static List<UnityPackageEntry> ReadMetadata(string filePath, string expectedArchiveHash = null,
			Action<long> progress = null, Func<bool> cancelled = null)
		{
			var map = new Dictionary<string, UnityPackageEntry>(StringComparer.OrdinalIgnoreCase);
			var ordered = new List<UnityPackageEntry>();
			try
			{
				using (var fs = new FileStream(filePath, FileMode.Open, FileAccess.Read, FileShare.Read, 64 * 1024, FileOptions.SequentialScan))
				using (var hash = BridgeSha256.Create())
				using (var verified = new CryptoStream(fs, hash, CryptoStreamMode.Read))
				using (var observed = new PreparationReadStream(expectedArchiveHash == null ? (Stream)fs : verified, progress, cancelled))
				using (var gz = new GZipStream(observed, CompressionMode.Decompress, true))
				using (var buffered = new BufferedStream(gz, 64 * 1024))
				{
					var reader = new TarReader(buffered, cancelled);
					while (reader.MoveNext(out var entry))
					{
						string name = entry.Name ?? string.Empty;
						int slash = name.IndexOf('/');
						if (slash <= 0)
						{
							reader.SkipEntry();
							continue;
						}

						string guid = name.Substring(0, slash).ToLowerInvariant();
						string kind = name.Substring(slash + 1);
						bool knownKind = kind == "asset" || kind == "asset.meta"
							|| kind == "pathname" || kind == "preview.png";
						if (!GuidUtility.IsValidGuid(guid) || !knownKind)
						{
							reader.SkipEntry();
							continue;
						}
						if (!map.TryGetValue(guid, out var record))
						{
							record = new UnityPackageEntry { Guid = guid, EntryOrder = ordered.Count };
							map[guid] = record;
							ordered.Add(record);
						}

						if (kind == "pathname")
						{
							if (record.HasPathnameMember)
								throw new InvalidDataException("Duplicate pathname member for GUID " + guid);
							record.AssetPath = GuidUtility.NormalizePathname(reader.ReadEntryBytes(MaxPathnameBytes));
							record.HasPathnameMember = true;
						}
						else if (kind == "asset.meta")
						{
							if (record.HasMetaMember)
								throw new InvalidDataException("Duplicate asset.meta member for GUID " + guid);
							byte[] meta = reader.ReadEntryBytes(MaxMetaBytes);
							record.HasMetaMember = true;
							record.MetaGuid = GuidUtility.ExtractGuidFromMeta(meta);
							record.IsFolder = GuidUtility.MetaDeclaresFolder(meta);
							record.MetaHash = GuidUtility.ComputeSha256(meta);
						}
						else if (kind == "asset")
						{
							if (record.HasAssetMember)
								throw new InvalidDataException("Duplicate asset member for GUID " + guid);
							record.HasAssetMember = true;
							record.Size = entry.Size;
							record.AssetHash = reader.ReadEntrySha256();
						}
						else
						{
							reader.SkipEntry();
						}
					}
					// TAR's zero block can precede the gzip trailer or trailing compressed bytes.
					// Consume the complete stream before accepting its declared transport digest.
					if (expectedArchiveHash != null)
					{
						byte[] tail = new byte[64 * 1024];
						while (buffered.Read(tail, 0, tail.Length) > 0)
							if (cancelled != null && cancelled()) throw new OperationCanceledException();
						while (observed.Read(tail, 0, tail.Length) > 0) { }
						string actual = BitConverter.ToString(hash.Hash).Replace("-", "").ToLowerInvariant();
						if (!string.Equals(actual, expectedArchiveHash, StringComparison.Ordinal))
							throw new IOException("Verified staged package changed during its metadata scan.");
					}
				}
			}
			catch (InvalidDataException ex)
			{
				throw new InvalidDataException("Not a valid .unitypackage: " + filePath + "\n" + ex.Message, ex);
			}
			catch (EndOfStreamException ex)
			{
				throw new InvalidDataException("Not a valid .unitypackage: " + filePath, ex);
			}

			var result = new List<UnityPackageEntry>(ordered.Count);
			foreach (UnityPackageEntry record in ordered)
			{
				ValidateMetadataRecord(record, filePath);
				result.Add(record);
			}
			return result;
		}

		private sealed class PreparationReadStream : Stream
		{
			private readonly Stream source;
			private readonly Action<long> progress;
			private readonly Func<bool> cancelled;
			private long bytesRead;
			public PreparationReadStream(Stream source, Action<long> progress, Func<bool> cancelled)
			{ this.source = source; this.progress = progress; this.cancelled = cancelled; }
			public override int Read(byte[] buffer, int offset, int count)
			{
				if (cancelled != null && cancelled()) throw new OperationCanceledException();
				int read = source.Read(buffer, offset, count);
				bytesRead += read;
				progress?.Invoke(bytesRead);
				return read;
			}
			public override bool CanRead => true;
			public override bool CanSeek => false;
			public override bool CanWrite => false;
			public override long Length => throw new NotSupportedException();
			public override long Position { get => bytesRead; set => throw new NotSupportedException(); }
			public override void Flush() { }
			public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException();
			public override void SetLength(long value) => throw new NotSupportedException();
			public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException();
		}

        // Re-read only selected members into verified bridge-owned staging using 64 KiB buffers.
        public static Dictionary<string, StagedAssetPayload> StageSelected(
            string filePath, ICollection<string> guids, string stagingDirectory)
        {
            var result = new Dictionary<string, StagedAssetPayload>(StringComparer.OrdinalIgnoreCase);
            if (guids == null || guids.Count == 0) return result;
            Directory.CreateDirectory(stagingDirectory);
            var wanted = new HashSet<string>(guids, StringComparer.OrdinalIgnoreCase);
            foreach (string guid in wanted)
                if (!GuidUtility.IsValidGuid(guid)) throw new InvalidDataException("Invalid selected archive GUID.");
            using (var fs = new FileStream(filePath, FileMode.Open, FileAccess.Read, FileShare.Read))
            using (var gz = new GZipStream(fs, CompressionMode.Decompress))
            using (var buffered = new BufferedStream(gz, 64 * 1024))
            {
                var reader = new TarReader(buffered);
                while (reader.MoveNext(out TarEntry entry))
                {
                    string name = entry.Name ?? string.Empty;
                    int slash = name.IndexOf('/');
                    if (slash <= 0) { reader.SkipEntry(); continue; }
                    string guid = name.Substring(0, slash).ToLowerInvariant();
                    string kind = name.Substring(slash + 1);
                    if (!wanted.Contains(guid) || (kind != "asset" && kind != "asset.meta"))
                    { reader.SkipEntry(); continue; }
                    if (!result.TryGetValue(guid, out StagedAssetPayload payload))
                    { payload = new StagedAssetPayload(); result.Add(guid, payload); }
                    bool isMeta = kind == "asset.meta";
                    if ((isMeta ? payload.MetaPath : payload.AssetPath) != null)
                        throw new InvalidDataException("Duplicate selected TAR member for " + guid);
                    string path = Path.Combine(stagingDirectory, guid + (isMeta ? ".meta.payload" : ".asset.payload"));
                    string hash;
                    using (var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                    {
                        hash = reader.CopyEntryTo(output, isMeta ? MaxMetaBytes : long.MaxValue);
                        output.Flush(true);
                    }
                    if (isMeta) { payload.MetaPath = path; payload.MetaHash = hash; }
                    else { payload.AssetPath = path; payload.AssetHash = hash; payload.AssetSize = entry.Size; }
                }
            }
            return result;
        }
		private static void ValidateMetadataRecord(UnityPackageEntry record, string filePath)
		{
			if (record == null || !record.HasPathnameMember || string.IsNullOrEmpty(record.AssetPath))
				throw new InvalidDataException("Package entry is missing a pathname in " + filePath);

			if (!ProjectPackagePath.TryNormalize(record.AssetPath, out string normalized, out string pathError))
				throw new InvalidDataException(pathError);
			record.AssetPath = normalized;

			bool validMetaGuid = GuidUtility.IsValidGuid(record.MetaGuid);
			record.IsPathManaged = ProjectPackagePath.IsPathManaged(normalized, validMetaGuid);

			if (record.IsPathManaged)
			{
				if (!record.HasAssetMember && !record.IsFolder)
					throw new InvalidDataException("Path-managed project entry has no payload and is not a folder: " + normalized);

				// Some package writers preserve the unitypackage member shape by emitting an
				// asset.meta for project files outside AssetDatabase. These entries have path
				// identity, not Unity GUID identity, so do not write that placeholder beside
				// the destination file.
				record.HasMetaMember = false;
				record.MetaGuid = null;
				record.MetaHash = null;
				return;
			}

			if (record.HasMetaMember)
			{
				if (!validMetaGuid)
					throw new InvalidDataException("Entry has an asset.meta without a valid GUID: " + normalized);
				if (!string.Equals(record.Guid, record.MetaGuid, StringComparison.OrdinalIgnoreCase))
					throw new InvalidDataException("Archive GUID does not match asset.meta GUID for " + normalized);
			}

			if (!record.HasMetaMember)
				throw new InvalidDataException("Unity asset entry is missing asset.meta: " + normalized);
			if (!record.HasAssetMember && !record.IsFolder)
				throw new InvalidDataException("Unity asset entry has no payload and is not a folder: " + normalized);
		}
	}

    public sealed class StagedAssetPayload
    {
        public string AssetPath;
        public string MetaPath;
        public string AssetHash;
        public string MetaHash;
        public long AssetSize;
    }
}
