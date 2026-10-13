// Adapted from Yan-K/AvatarToolbox SmartPackage, commit cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md and SOURCE_PROVENANCE.md.
using System.Collections.Generic;

namespace NekoNekoAsset.Bridge.SmartPackage
{
	public enum ImportConflictKind
	{
		New,
		Duplicate,
		Update,
		PathConflict,
		GuidConflict
	}

	public struct ImportConflict
	{
		public ImportConflictKind Kind;
		public string ExistingPath;
		public string IncomingPath;
		public string TargetPath;
		public string ExistingGuid;
		public bool ExistingFromProject;
		public bool RequiresDecision;
		public string Message;
	}

	public static class ImportConflictResolver
	{
		public static Dictionary<string, ImportConflict> Resolve(IEnumerable<UnityPackageEntry> entries, IAssetProbe probe)
		{
			var result = new Dictionary<string, ImportConflict>();
			if (entries == null || probe == null)
				return result;

			foreach (UnityPackageEntry entry in entries)
			{
				if (entry == null || string.IsNullOrEmpty(entry.Guid))
					continue;

				string existingPath = probe.PathAt(entry.Guid);
				string pathHolder = probe.GuidAt(entry.AssetPath);

				ImportConflictKind kind;
				if (string.IsNullOrEmpty(existingPath) && string.IsNullOrEmpty(pathHolder))
					kind = ImportConflictKind.New;
				else if (!string.IsNullOrEmpty(existingPath) && existingPath == entry.AssetPath)
					kind = ImportConflictKind.Update;
				else if (string.IsNullOrEmpty(existingPath) && !string.IsNullOrEmpty(pathHolder))
					kind = ImportConflictKind.PathConflict;
				else
					kind = ImportConflictKind.GuidConflict;

				result[entry.Guid] = new ImportConflict
				{
					Kind = kind,
					ExistingPath = existingPath,
					IncomingPath = entry.AssetPath
				};
			}

			return result;
		}
	}
}
