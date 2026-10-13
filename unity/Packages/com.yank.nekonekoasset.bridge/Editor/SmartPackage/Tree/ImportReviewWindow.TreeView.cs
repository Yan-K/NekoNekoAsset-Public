// Importer tree subset adapted from AvatarToolbox cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md / SOURCE_PROVENANCE.md.
using System;
using System.Collections.Generic;
using NekoNekoAsset.Bridge.SmartPackage;
using UnityEditor;
using UnityEngine;

namespace NekoNekoAsset.Bridge
{
    public sealed partial class ImportReviewWindow
    {
        private const float RowHeight = 18f;
        private const float TreeIndentWidth = 14f;
        private const float TreeFoldoutWidth = 14f;
        private const float TreeToggleWidth = 16f;
        private const float TreeIconWidth = 18f;
        private static GUIStyle rowName;
        private static Texture folderIcon;
        private readonly Dictionary<string, Texture> iconCache = new Dictionary<string, Texture>(StringComparer.OrdinalIgnoreCase);
        private readonly Dictionary<PackageAssetNode, string> treeControlNames = new Dictionary<PackageAssetNode, string>();
        private readonly List<PackageAssetNode> visibleRows = new List<PackageAssetNode>();
        private readonly Dictionary<PackageAssetNode, PackageAssetNode.ToggleState> treeStates = new Dictionary<PackageAssetNode, PackageAssetNode.ToggleState>();
        private int treeStateVersion = -1;
        private bool treeKeyboardFocus;

        private PackageAssetNode.ToggleState GetTreeState(PackageAssetNode node)
        {
            if (treeStateVersion != importerCheckVersion)
            {
                treeStates.Clear();
                treeStateVersion = importerCheckVersion;
            }
            if (treeStates.TryGetValue(node, out PackageAssetNode.ToggleState state)) return state;
            state = node.IsChecked ? PackageAssetNode.ToggleState.Checked : PackageAssetNode.ToggleState.Unchecked;
            if (node.IsFolder && node.Children.Count > 0)
            {
                state = GetTreeState(node.Children[0]);
                for (int i = 1; i < node.Children.Count && state != PackageAssetNode.ToggleState.Mixed; i++)
                    if (GetTreeState(node.Children[i]) != state) state = PackageAssetNode.ToggleState.Mixed;
            }
            treeStates[node] = state;
            return state;
        }

        private static GUIStyle RowNameStyle => rowName ?? (rowName = new GUIStyle(EditorStyles.label)
            { alignment = TextAnchor.MiddleLeft, clipping = TextClipping.Clip });

        private string TreeControlName(PackageAssetNode node)
        {
            if (!treeControlNames.TryGetValue(node, out string name))
            {
                name = "NNAImportTree:" + treeControlNames.Count;
                treeControlNames.Add(node, name);
            }
            return name;
        }

        private void BeginTreeGUI()
        {
            Event current = Event.current;
            if (treeKeyboardFocus && GUI.GetNameOfFocusedControl().StartsWith("NNAImportTree:", StringComparison.Ordinal)
                && !EditorGUIUtility.editingTextField && importerHighlightNode != null
                && current.type == EventType.KeyDown)
            {
                int index = visibleRows.IndexOf(importerHighlightNode);
                bool handled = true;
                switch (current.keyCode)
                {
                    case KeyCode.UpArrow:
                        if (index > 0) importerHighlightNode = visibleRows[index - 1];
                        break;
                    case KeyCode.DownArrow:
                        if (index >= 0 && index + 1 < visibleRows.Count) importerHighlightNode = visibleRows[index + 1];
                        break;
                    case KeyCode.LeftArrow:
                        if (importerHighlightNode.IsFolder && importerHighlightNode.IsExpanded)
                            importerHighlightNode.IsExpanded = false;
                        else
                        {
                            PackageAssetNode parent = DisplayedTreeParent(importerHighlightNode);
                            if (parent != null && !string.IsNullOrEmpty(parent.FullPath)) importerHighlightNode = parent;
                        }
                        break;
                    case KeyCode.RightArrow:
                        if (importerHighlightNode.IsFolder) importerHighlightNode.IsExpanded = true;
                        break;
                    case KeyCode.Space:
                        ClearReviewMessage();
                        importerHighlightNode.SetChecked(GetTreeState(importerHighlightNode) != PackageAssetNode.ToggleState.Checked, true);
                        RefreshImporterConflictPreview(false);
                        break;
                    default: handled = false; break;
                }
                if (handled) { current.Use(); Repaint(); }
            }
            visibleRows.Clear();
        }

        private PackageAssetNode DisplayedTreeParent(PackageAssetNode node)
            => looseTreeOwners.ContainsKey(node) ? (individualDestinations ? null : unifiedLooseLeafParent) : node.Parent;

        private Rect DrawRowCore(PackageAssetNode node, int depth, float tailWidth)
        {
            Rect row = GUILayoutUtility.GetRect(0f, RowHeight, GUILayout.Height(RowHeight), GUILayout.ExpandWidth(true));
            visibleRows.Add(node);
            if (node == importerHighlightNode && Event.current.type == EventType.Repaint)
                EditorGUI.DrawRect(row, new Color(.24f, .48f, .90f, .20f));
            float middleY = row.y + (RowHeight - 16f) * .5f;
            float controlsWidth = TreeFoldoutWidth + TreeToggleWidth + TreeIconWidth + 4f;
            // Keep essential controls and the conflict tag inside narrow windows, even for deep trees.
            float indent = Mathf.Min(depth * TreeIndentWidth, Mathf.Max(0f, row.width - tailWidth - controlsWidth - 32f));
            float x = row.x + indent;
            var foldoutRect = new Rect(x, middleY, TreeFoldoutWidth, 16f);
            if (node.IsFolder && node.Children.Count > 0)
                node.IsExpanded = EditorGUI.Foldout(foldoutRect, node.IsExpanded, GUIContent.none, true);
            x += TreeFoldoutWidth;

            var toggleRect = new Rect(x, middleY, TreeToggleWidth, 16f);
            PackageAssetNode.ToggleState state = GetTreeState(node);
            bool previousMixed = EditorGUI.showMixedValue;
            EditorGUI.showMixedValue = state == PackageAssetNode.ToggleState.Mixed;
            bool wasChecked = state == PackageAssetNode.ToggleState.Checked;
            bool next;
            try
            {
                GUI.SetNextControlName(TreeControlName(node));
                next = EditorGUI.Toggle(toggleRect, wasChecked);
            }
            finally { EditorGUI.showMixedValue = previousMixed; }
            if (next != wasChecked)
            {
                ClearReviewMessage();
                importerHighlightNode = node;
                treeKeyboardFocus = true;
                node.SetChecked(next, true);
                RefreshImporterConflictPreview(false);
            }
            x += TreeToggleWidth + 2f;
            if (Event.current.type == EventType.Repaint)
            {
                Texture icon = GetCachedIconFor(node);
                if (icon != null) GUI.DrawTexture(new Rect(x, middleY, TreeIconWidth, 16f), icon, ScaleMode.ScaleToFit);
            }
            x += TreeIconWidth + 2f;
            float nameRight = Mathf.Max(x, row.xMax - tailWidth - 2f);
            var nameRect = new Rect(x, row.y, Mathf.Max(0f, nameRight - x), RowHeight);
            string tooltip = node.FullPath;
            if (looseTreeOwners.TryGetValue(node, out LoadedPackage owner) && !string.IsNullOrEmpty(owner.DisplayName))
                tooltip = owner.DisplayName + "\n" + tooltip;
            GUI.Label(nameRect, new GUIContent(node.Name, tooltip), RowNameStyle);
            if (Event.current.type == EventType.MouseDown && Event.current.button == 0 && nameRect.Contains(Event.current.mousePosition))
            {
                importerHighlightNode = node;
                treeKeyboardFocus = true;
                GUI.FocusControl(TreeControlName(node));
                Repaint();
                Event.current.Use();
            }
            return new Rect(row.xMax - tailWidth, row.y, tailWidth, RowHeight);
        }

        private Texture GetCachedIconFor(PackageAssetNode node)
        {
            string path = node.FullPath;
            if (string.IsNullOrEmpty(path)) return ResolveFallbackIcon(node);
            if (iconCache.TryGetValue(path, out Texture cached)) return cached;
            Texture icon = AssetDatabase.GetCachedIcon(path);
            if (icon == null) icon = ResolveFallbackIcon(node);
            iconCache[path] = icon;
            return icon;
        }

        private static Texture ResolveFallbackIcon(PackageAssetNode node)
        {
            if (node.IsFolder)
            {
                if (folderIcon == null)
                {
                    GUIContent content = EditorGUIUtility.IconContent("Folder Icon");
                    folderIcon = content != null ? content.image : null;
                    if (folderIcon == null) folderIcon = AssetDatabase.GetCachedIcon("Assets");
                }
                return folderIcon;
            }
            Texture icon = string.IsNullOrEmpty(node.Name) ? null : UnityEditorInternal.InternalEditorUtility.GetIconForFile(node.Name);
            return icon != null ? icon : AssetPreview.GetMiniTypeThumbnail(typeof(UnityEngine.Object));
        }

        private void DrawNode(PackageAssetNode node, int depth, Dictionary<string, ImportConflict> conflicts)
        {
            if (node == null) return;
            if (looseTreeOwners.TryGetValue(node, out LoadedPackage owner)) conflicts = owner.ConflictByPath;
            // The synthetic Project container is not a selectable package entry.
            if (node.Parent == null && string.IsNullOrEmpty(node.FullPath))
            {
                foreach (PackageAssetNode topLevel in node.Children) DrawNode(topLevel, depth, conflicts);
                return;
            }
            ImportConflictKind badgeKind = default;
            bool hasBadge = false;
            if (node.IsFolder)
            {
                if (node.HasCheckedGuidConflict || node.HasCheckedPathConflict || node.HasCheckedUpdate)
                { badgeKind = ImportConflictKind.GuidConflict; hasBadge = true; }
                else if (node.HasCheckedDuplicate) { badgeKind = ImportConflictKind.Duplicate; hasBadge = true; }
            }
            else if (node.IsChecked && conflicts != null && conflicts.TryGetValue(node.FullPath, out ImportConflict conflict)
                && !(conflict.Kind == ImportConflictKind.Duplicate && conflict.ExistingFromProject))
            {
                badgeKind = VisibleBadgeKind(conflict.Kind);
                hasBadge = true;
            }
            float badgeWidth = hasBadge ? EditorStyles.miniLabel.CalcSize(new GUIContent(BadgeLabel(badgeKind))).x + 12f : 0f;
            Rect tail = DrawRowCore(node, depth, badgeWidth);
            if (hasBadge) DrawConflictBadge(new Rect(tail.x, tail.y + (RowHeight - 16f) * .5f, badgeWidth, 16f), badgeKind);
            if (node.IsFolder && node.IsExpanded)
                foreach (PackageAssetNode child in node.Children) DrawNode(child, depth + 1, conflicts);
        }

        private string BadgeLabel(ImportConflictKind kind)
        {
            if (kind == ImportConflictKind.New) return L("new");
            return kind == ImportConflictKind.Duplicate ? L("identical") : L("guidConflict");
        }

        private static ImportConflictKind VisibleBadgeKind(ImportConflictKind kind)
        {
            return kind == ImportConflictKind.New || kind == ImportConflictKind.Duplicate ? kind : ImportConflictKind.GuidConflict;
        }

        private void DrawConflictBadge(Rect rect, ImportConflictKind kind)
        {
            Color background = kind == ImportConflictKind.New ? new Color(.30f, .70f, .30f, .85f)
                : kind == ImportConflictKind.Duplicate ? new Color(.25f, .50f, .90f, .85f)
                : new Color(.85f, .20f, .20f, .90f);
            EditorGUI.DrawRect(rect, background);
            GUI.Label(rect, BadgeLabel(kind), EditorStyles.whiteMiniLabel);
        }
    }
}
