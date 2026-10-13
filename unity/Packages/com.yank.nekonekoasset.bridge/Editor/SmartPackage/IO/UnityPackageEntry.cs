// Adapted from Yan-K/AvatarToolbox SmartPackage, commit cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md and SOURCE_PROVENANCE.md.
namespace NekoNekoAsset.Bridge.SmartPackage
{
	public sealed class UnityPackageEntry
	{
		// GUID used for the top-level directory inside the .unitypackage. For normal
		// Unity assets this must match MetaGuid. Path-managed project files may have
		// an archive GUID without having a Unity .meta identity.
		public string Guid;
		public string MetaGuid;
		public string AssetPath;
		public byte[] AssetBytes;
		public byte[] MetaBytes;
		public byte[] PreviewBytes;
		public int EntryOrder;
		public bool HasAssetMember;
		public bool HasMetaMember;
		public bool HasPathnameMember;
		public bool IsFolder;
		public bool IsPathManaged;
		public string ValidationError;
		public string AssetHash;
		public string MetaHash;
		// Loose files use the same planner. These paths point only at verified bridge staging.
		public string SourceAssetPath;
		public string SourceMetaPath;
		public string LooseRelativePath;
		public bool PreserveExistingMeta;

		public string IdentityGuid => IsPathManaged ? null : MetaGuid;

		// Uncompressed asset payload size in bytes, captured during a metadata-only
		// scan when AssetBytes is not loaded into memory.
		public long Size;

		public long TotalSize
		{
			get
			{
				long total = 0;
				if (AssetBytes != null) total += AssetBytes.Length;
				if (MetaBytes != null) total += MetaBytes.Length;
				if (PreviewBytes != null) total += PreviewBytes.Length;
				return total;
			}
		}
	}
}
