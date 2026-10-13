// Adapted from Yan-K/AvatarToolbox SmartPackage, commit cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md and SOURCE_PROVENANCE.md.
using System.Collections.Generic;

namespace NekoNekoAsset.Bridge.SmartPackage
{
	public sealed class LoadedPackage
	{
		public string FilePath;
		public string DisplayName;
		public long DropOrder;
		public int LoadGeneration;
		public long FileLength;
		public long FileLastWriteUtcTicks;
		public bool IsLooseFiles;
		public string LooseDestination = "Assets";
		public List<UnityPackageEntry> Entries = new List<UnityPackageEntry>();
		public PackageAssetNode Tree;
		public bool IsExpanded = true;
		public Dictionary<string, ImportConflict> ConflictByGuid =
			new Dictionary<string, ImportConflict>(System.StringComparer.OrdinalIgnoreCase);

		// Conflict lookup keyed by asset path, built once on load so the UI never
		// rebuilds it per repaint.
		public Dictionary<string, ImportConflict> ConflictByPath =
			new Dictionary<string, ImportConflict>(System.StringComparer.OrdinalIgnoreCase);

		// Tallies reflecting only CHECKED leaves; recomputed lazily when the checked
		// state changes (tracked via TallyVersion) rather than every frame.
		public int GuidConflictCount;
		public int PathConflictCount;
		public int UpdateCount;
		public int DuplicateCount;
		public int SelectedCount;
		public int TallyVersion = -1;

		// Total number of importable leaf items in the tree (files + empty folders),
		// computed once after the tree is built. Used as the denominator on the package
		// card so "select all" reads N / N instead of counting structural folder entries.
		public int LeafCount;

		public static LoadedPackage LoadPackage(string filePath, long dropOrder, string expectedArchiveHash = null,
			System.Action<long> progress = null, System.Func<bool> cancelled = null)
		{
			var info = new System.IO.FileInfo(filePath);
			var package = new LoadedPackage
			{
				FilePath = info.FullName,
				DisplayName = info.Name,
				DropOrder = dropOrder,
				FileLength = info.Length,
				FileLastWriteUtcTicks = info.LastWriteTimeUtc.Ticks,
				Entries = UnityPackageReader.ReadMetadata(info.FullName, expectedArchiveHash, progress, cancelled)
			};
			info.Refresh();
			if (info.Length != package.FileLength || info.LastWriteTimeUtc.Ticks != package.FileLastWriteUtcTicks)
				throw new System.IO.IOException("The staged package changed during its metadata scan.");
			package.Tree = PackageAssetNode.BuildProjectTree(package.Entries);
			package.LeafCount = package.Tree.CountLeaves();
			return package;
		}
	}
}
