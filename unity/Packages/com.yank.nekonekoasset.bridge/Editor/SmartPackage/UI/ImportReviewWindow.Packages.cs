// Importer UI subset adapted from AvatarToolbox cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md / SOURCE_PROVENANCE.md.
using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using NekoNekoAsset.Bridge.SmartPackage;
using UnityEditor;
using UnityEngine;

namespace NekoNekoAsset.Bridge
{
    public sealed partial class ImportReviewWindow
    {
        private PackageAssetNode unifiedLooseTree;
        private PackageAssetNode unifiedLooseLeafParent;
        private bool unifiedLooseExpanded = true;
        private readonly Dictionary<PackageAssetNode, LoadedPackage> looseTreeOwners = new Dictionary<PackageAssetNode, LoadedPackage>();
        private bool previewRequested;
        private bool previewSnapshotDirty;
        private int previewGeneration;
        private int runningPreviewGeneration;
        private double previewDueAt;
        private Task<ImportPlan> importerPreviewTask;
        private Dictionary<LoadedPackage, LoadedPackage> previewOwners;
        private CachedPreviewProbe importerPreviewProbe;
        private bool PreviewPending => previewRequested || importerPreviewTask != null;

        // Only a preview worker reads this cache. Confirmation always captures fresh
        // project facts; cached misses and hashes never authorize project writes.
        private sealed class CachedPreviewProbe : IAssetProbe
        {
            private readonly SnapshotAssetProbe snapshot;
            private readonly Dictionary<string, ObservedAsset> paths;
            private readonly Dictionary<string, string> guids;
            public CachedPreviewProbe(SnapshotAssetProbe snapshot, PlanObservations seed = null)
            {
                this.snapshot = snapshot;
                paths = seed == null ? new Dictionary<string, ObservedAsset>(StringComparer.OrdinalIgnoreCase)
                    : new Dictionary<string, ObservedAsset>(seed.Paths, StringComparer.OrdinalIgnoreCase);
                guids = seed == null ? new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
                    : new Dictionary<string, string>(seed.GuidPaths, StringComparer.OrdinalIgnoreCase);
            }
            private ObservedAsset At(string path)
            {
                if (string.IsNullOrEmpty(path)) return new ObservedAsset();
                if (!paths.TryGetValue(path, out ObservedAsset value))
                {
                    value = ObservedAsset.Capture(snapshot, path);
                    paths.Add(path, value);
                }
                return value;
            }
            public string GuidAt(string path) => At(path).Guid;
            public string PathAt(string guid)
            {
                if (string.IsNullOrEmpty(guid)) return null;
                if (!guids.TryGetValue(guid, out string path))
                {
                    path = snapshot.PathAt(guid);
                    guids.Add(guid, path);
                }
                return path;
            }
            public bool ExistsAt(string path) => At(path).Exists;
            public bool IsFolderAt(string path) => At(path).IsFolder;
            public bool TryGetContent(string path, out AssetContentFingerprint content)
            {
                ObservedAsset value = At(path);
                content = value.Content;
                return value.ContentKnown;
            }
        }

        private void EnsureLooseTree()
        {
            if (unifiedLooseTree != null) return;
            looseTreeOwners.Clear();
            PackageAssetNode parent = null;
            string path = string.Empty;
            foreach (string part in destination.Split('/'))
            {
                path = string.IsNullOrEmpty(path) ? part : path + "/" + part;
                var folder = new PackageAssetNode { Name = part, FullPath = path,
                    IsFolder = true, IsExpanded = parent == null ? unifiedLooseExpanded : true, Parent = parent };
                if (parent == null) unifiedLooseTree = folder;
                else parent.Children.Add(folder);
                parent = folder;
            }
            unifiedLooseLeafParent = parent;
            foreach (LoadedPackage package in loadedPackages)
            {
                if (!package.IsLooseFiles || package.Tree == null) continue;
                AddLooseLeaves(package.Tree, package);
            }
            // Retain every source leaf, including different files with the same basename.
            // Their original parents remain responsible for the planner's selection state.
            unifiedLooseLeafParent.Children.Sort((a, b) =>
            {
                int name = string.Compare(a.Name, b.Name, StringComparison.OrdinalIgnoreCase);
                return name != 0 ? name : looseTreeOwners[a].DropOrder.CompareTo(looseTreeOwners[b].DropOrder);
            });
        }

        private void AddLooseLeaves(PackageAssetNode node, LoadedPackage owner)
        {
            if (!node.IsFolder || (node.Children.Count == 0 && node.HasPackageEntry))
            {
                unifiedLooseLeafParent.Children.Add(node);
                looseTreeOwners[node] = owner;
                return;
            }
            foreach (PackageAssetNode child in node.Children) AddLooseLeaves(child, owner);
        }

        private void DrawLooseFiles()
        {
            if (!hasLooseFiles) return;
            EnsureLooseTree();
            foreach (LoadedPackage package in loadedPackages)
                if (package.IsLooseFiles) EnsurePackageTally(package);
            if (!individualDestinations)
            {
                bool guid = false, path = false, update = false, duplicate = false;
                foreach (PackageAssetNode leaf in unifiedLooseLeafParent.Children)
                {
                    guid |= leaf.HasCheckedGuidConflict;
                    path |= leaf.HasCheckedPathConflict;
                    update |= leaf.HasCheckedUpdate;
                    duplicate |= leaf.HasCheckedDuplicate;
                }
                for (PackageAssetNode folder = unifiedLooseLeafParent; folder != null; folder = folder.Parent)
                {
                    folder.HasCheckedGuidConflict = guid;
                    folder.HasCheckedPathConflict = path;
                    folder.HasCheckedUpdate = update;
                    folder.HasCheckedDuplicate = duplicate;
                }
                DrawNode(unifiedLooseTree, 0, null);
                unifiedLooseExpanded = unifiedLooseTree.IsExpanded;
                return;
            }
            foreach (LoadedPackage package in loadedPackages)
            {
                if (!package.IsLooseFiles) continue;
                // Loose inputs have only basenames. Avoid a card and repeated Assets
                // hierarchy around each file; keep its checkbox and path together.
                DrawLooseLeaves(package.Tree, package.ConflictByPath);
                DrawDestination(package);
            }
        }

        private void DrawLooseLeaves(PackageAssetNode node, Dictionary<string, ImportConflict> conflicts)
        {
            if (!node.IsFolder || (node.Children.Count == 0 && node.HasPackageEntry))
            {
                DrawNode(node, 0, conflicts);
                return;
            }
            foreach (PackageAssetNode child in node.Children) DrawLooseLeaves(child, conflicts);
        }

        private void DrawPackageCard(LoadedPackage package)
        {
            EnsurePackageTally(package);
            using (new EditorGUILayout.VerticalScope(EditorStyles.helpBox))
            {
                using (new EditorGUILayout.HorizontalScope())
                {
                    string name = !string.IsNullOrEmpty(package.DisplayName) ? package.DisplayName
                        : package.IsLooseFiles ? L("looseFiles") : Path.GetFileName(package.FilePath);
                    var label = new GUIContent($"{name}   ({package.SelectedCount} / {package.LeafCount})", name);
                    package.IsExpanded = EditorGUILayout.Foldout(package.IsExpanded, label, true);
                    GUILayout.FlexibleSpace();
                    DrawPackageConflictTallies(package);
                }
                if (package.IsExpanded && package.Tree != null)
                    DrawNode(package.Tree, 0, package.ConflictByPath);
            }
        }

        private void SetAllPackagesExpanded(bool expanded)
        {
            unifiedLooseExpanded = expanded;
            SetNodeExpanded(unifiedLooseTree, expanded);
            foreach (LoadedPackage package in loadedPackages)
            {
                package.IsExpanded = expanded;
                SetNodeExpanded(package.Tree, expanded);
            }
        }

        private static void SetNodeExpanded(PackageAssetNode node, bool expanded)
        {
            if (node == null) return;
            node.IsExpanded = expanded;
            foreach (PackageAssetNode child in node.Children) SetNodeExpanded(child, expanded);
        }

        private void RefreshImporterConflictPreview(bool refreshProjectSnapshot)
        {
            if (loadedPackages.Count == 0) return;
            previewSnapshotDirty |= refreshProjectSnapshot;
            previewRequested = true;
            previewGeneration++;
            previewDueAt = EditorApplication.timeSinceStartup + .08;
            importerCheckVersion++;
            Repaint();
        }

        private void UpdateImporterPreview()
        {
            if (completed || confirming || loadedPackages.Count == 0) return;
            if (importerPreviewTask != null)
            {
                if (!importerPreviewTask.IsCompleted) return;
                Task<ImportPlan> finished = importerPreviewTask;
                importerPreviewTask = null;
                if (finished.IsFaulted) { _ = finished.Exception; }
                if (runningPreviewGeneration == previewGeneration)
                {
                    if (finished.Status == TaskStatus.RanToCompletion)
                    {
                        ImportPlan preview = finished.Result;
                        foreach (ImportPlanItem item in preview.OrderedItems) item.Package = previewOwners[item.Package];
                        foreach (LoadedPackage package in loadedPackages)
                        {
                            package.ConflictByGuid.Clear();
                            package.ConflictByPath.Clear();
                            package.TallyVersion = -1;
                        }
                        ApplyImporterConflictPreview(preview);
                    }
                    else { importBlocked = true; error = L("invalidInput"); Repaint(); }
                }
                previewOwners = null;
            }
            if (!previewRequested || EditorApplication.timeSinceStartup < previewDueAt
                || EditorApplication.isCompiling || EditorApplication.isUpdating) return;
            try
            {
                if (previewSnapshotDirty || importerProjectSnapshot == null)
                {
                    // Unity APIs stay on the main thread. Target reads and hashes run below in one worker.
                    importerProjectSnapshot = SnapshotAssetProbe.Capture(includeContent: true);
                    importerPreviewProbe = new CachedPreviewProbe(importerProjectSnapshot);
                }
                if (importerPreviewProbe == null) importerPreviewProbe = new CachedPreviewProbe(importerProjectSnapshot);
                var inputs = new List<LoadedPackage>(loadedPackages.Count);
                previewOwners = new Dictionary<LoadedPackage, LoadedPackage>();
                foreach (LoadedPackage original in loadedPackages)
                {
                    // Capture mutable paths and checked leaves before yielding to the worker.
                    // Separate packages keep their ownership even when basenames coincide.
                    var copy = new LoadedPackage { DropOrder = original.DropOrder, IsLooseFiles = original.IsLooseFiles,
                        Tree = new PackageAssetNode { IsFolder = true } };
                    foreach (PackageAssetNode leaf in original.Tree.EnumerateCheckedLeaves())
                        copy.Tree.Children.Add(new PackageAssetNode { FullPath = leaf.FullPath, IsFolder = leaf.IsFolder,
                            HasPackageEntry = leaf.HasPackageEntry });
                    foreach (UnityPackageEntry entry in original.Entries)
                    {
                        if (entry == null) continue;
                        copy.Entries.Add(new UnityPackageEntry { Guid = entry.Guid, MetaGuid = entry.MetaGuid,
                            AssetPath = entry.AssetPath, EntryOrder = entry.EntryOrder, IsFolder = entry.IsFolder,
                            IsPathManaged = entry.IsPathManaged, HasAssetMember = entry.HasAssetMember,
                            HasMetaMember = entry.HasMetaMember, PreserveExistingMeta = entry.PreserveExistingMeta,
                            AssetHash = entry.AssetHash, MetaHash = entry.MetaHash });
                    }
                    inputs.Add(copy);
                    previewOwners.Add(copy, original);
                }
                CachedPreviewProbe probe = importerPreviewProbe;
                ConflictPolicy policy = importerPolicy;
                runningPreviewGeneration = previewGeneration;
                previewSnapshotDirty = false;
                previewRequested = false;
                importerPreviewTask = Task.Run(() => ImportPlanBuilder.Build(inputs, probe, policy));
            }
            catch (Exception)
            {
                previewRequested = false;
                importBlocked = true;
                error = L("invalidInput");
                Repaint();
            }
        }

        private void ApplyImporterConflictPreview(ImportPlan preview)
        {
            foreach (ImportPlanItem item in preview.OrderedItems)
            {
                if (item.Package == null || item.Entry == null) continue;
                if (!string.IsNullOrEmpty(item.Entry.Guid)) item.Package.ConflictByGuid[item.Entry.Guid] = item.Conflict;
                item.Package.ConflictByPath[item.IncomingPath] = item.Conflict;
            }
            if (!destinationInvalid)
            {
                importBlocked = !preview.CanApply;
                error = preview.CanApply ? reviewErrorMessage : string.Join("\n", preview.Errors);
            }
            importerCheckVersion++;
            Repaint();
        }

        private void EnsurePackageTally(LoadedPackage package)
        {
            if (package == null || package.Tree == null || package.TallyVersion == importerCheckVersion) return;
            package.TallyVersion = importerCheckVersion;
            int guid = 0, path = 0, update = 0, duplicate = 0, selected = 0;
            AggregateConflicts(package.Tree, package.ConflictByPath, ref guid, ref path, ref update, ref duplicate, ref selected);
            package.GuidConflictCount = guid;
            package.PathConflictCount = path;
            package.UpdateCount = update;
            package.DuplicateCount = duplicate;
            package.SelectedCount = selected;
        }

        private void AggregateConflicts(PackageAssetNode node, Dictionary<string, ImportConflict> map,
            ref int guid, ref int path, ref int update, ref int duplicate, ref int selected)
        {
            if (!node.IsFolder || (node.Children.Count == 0 && node.HasPackageEntry))
            {
                node.HasCheckedGuidConflict = false;
                node.HasCheckedPathConflict = false;
                node.HasCheckedUpdate = false;
                node.HasCheckedDuplicate = false;
                if (node.IsChecked)
                {
                    selected++;
                    if (map != null && map.TryGetValue(node.FullPath, out ImportConflict conflict))
                    {
                        if (conflict.Kind == ImportConflictKind.GuidConflict) { guid++; node.HasCheckedGuidConflict = true; }
                        else if (conflict.Kind == ImportConflictKind.PathConflict) { path++; node.HasCheckedPathConflict = true; }
                        else if (conflict.Kind == ImportConflictKind.Update) { update++; node.HasCheckedUpdate = true; }
                        else if (conflict.Kind == ImportConflictKind.Duplicate && !conflict.ExistingFromProject)
                        { duplicate++; node.HasCheckedDuplicate = true; }
                    }
                }
                return;
            }
            bool folderGuid = false, folderPath = false, folderUpdate = false, folderDuplicate = false;
            foreach (PackageAssetNode child in node.Children)
            {
                AggregateConflicts(child, map, ref guid, ref path, ref update, ref duplicate, ref selected);
                folderGuid |= child.HasCheckedGuidConflict;
                folderPath |= child.HasCheckedPathConflict;
                folderUpdate |= child.HasCheckedUpdate;
                folderDuplicate |= child.HasCheckedDuplicate;
            }
            // A non-empty folder's .meta follows selected descendants without inflating the file count.
            if (node.HasPackageEntry && GetTreeState(node) != PackageAssetNode.ToggleState.Unchecked
                && map != null && map.TryGetValue(node.FullPath, out ImportConflict folderConflict))
            {
                folderGuid |= folderConflict.Kind == ImportConflictKind.GuidConflict;
                folderPath |= folderConflict.Kind == ImportConflictKind.PathConflict;
                folderUpdate |= folderConflict.Kind == ImportConflictKind.Update;
                folderDuplicate |= folderConflict.Kind == ImportConflictKind.Duplicate && !folderConflict.ExistingFromProject;
            }
            node.HasCheckedGuidConflict = folderGuid;
            node.HasCheckedPathConflict = folderPath;
            node.HasCheckedUpdate = folderUpdate;
            node.HasCheckedDuplicate = folderDuplicate;
        }

        private void DrawPackageConflictTallies(LoadedPackage package)
        {
            int conflicts = package.GuidConflictCount + package.PathConflictCount + package.UpdateCount;
            if (conflicts > 0) DrawTallyBadge(localization.Format("conflictCount", conflicts), new Color(.85f, .20f, .20f, .90f));
            if (package.DuplicateCount > 0) DrawTallyBadge(localization.Format("identicalCount", package.DuplicateCount), new Color(.25f, .50f, .90f, .85f));
        }

        private static void DrawTallyBadge(string label, Color background)
        {
            var content = new GUIContent(label);
            Vector2 size = EditorStyles.miniLabel.CalcSize(content);
            Rect rect = GUILayoutUtility.GetRect(size.x + 12f, 16f, GUILayout.Width(size.x + 12f), GUILayout.Height(16f));
            EditorGUI.DrawRect(rect, background);
            GUI.Label(rect, content, EditorStyles.whiteMiniLabel);
        }
    }
}
