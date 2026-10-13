// ConflictDecisionWindow subset adapted from AvatarToolbox cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md / SOURCE_PROVENANCE.md.
using NekoNekoAsset.Bridge.SmartPackage;
using UnityEditor;
using UnityEngine;

namespace NekoNekoAsset.Bridge
{
    internal enum ImportConflictDecision { Skip, Overwrite, SkipAll, OverwriteAll }

    internal sealed class ImportConflictWindow : EditorWindow
    {
        private static bool hasPosition;
        private static Rect lastPosition;
        private static ImportConflictWindow active;
        private BridgeLocalization localization;
        private string message;
        private ImportConflictDecision decision = ImportConflictDecision.Skip;
        private Vector2 scroll;

        public static ImportConflictDecision ShowDialog(ImportPlanItem item, BridgeLocalization localization)
        {
            var window = CreateInstance<ImportConflictWindow>();
            window.localization = localization;
            window.titleContent = new GUIContent(localization.Text("conflictTitle"));
            ImportConflict conflict = item.Conflict;
            string incomingGuid = string.IsNullOrEmpty(item.Entry.IdentityGuid)
                ? localization.Text("pathManaged") : item.Entry.IdentityGuid;
            string existingGuid = string.IsNullOrEmpty(conflict.ExistingGuid)
                ? localization.Text("none") : conflict.ExistingGuid;
            window.message = localization.Format("conflictDetails", localization.Text("guidConflict"),
                item.IncomingPath, incomingGuid, item.TargetPath, existingGuid);
            window.minSize = new Vector2(520f, 270f);
            window.maxSize = new Vector2(760f, 420f);
            if (hasPosition) window.position = lastPosition;
            else
            {
                Rect main = EditorGUIUtility.GetMainWindowPosition();
                window.position = new Rect(main.x + (main.width - 640f) * .5f,
                    main.y + (main.height - 330f) * .5f, 640f, 330f);
            }
            active = window;
            try
            {
                window.ShowModalUtility();
                return window.decision;
            }
            finally { active = null; }
        }

        public static void CancelActive()
        {
            if (active != null) active.Choose(ImportConflictDecision.Skip);
        }

        public static void ResetPosition()
        {
            hasPosition = false;
            lastPosition = default;
        }

        private void OnGUI()
        {
            if (Event.current.type == EventType.KeyDown && Event.current.keyCode == KeyCode.Escape)
            {
                Event.current.Use();
                Choose(ImportConflictDecision.Skip);
                GUIUtility.ExitGUI();
                return;
            }
            ImportConflictDecision? selected = null;
            GUILayout.Space(8f);
            EditorGUILayout.LabelField(localization.Text("conflictHeading"), EditorStyles.boldLabel);
            GUILayout.Space(4f);
            scroll = EditorGUILayout.BeginScrollView(scroll, EditorStyles.helpBox);
            EditorGUILayout.LabelField(message ?? string.Empty, EditorStyles.wordWrappedLabel);
            EditorGUILayout.EndScrollView();
            GUILayout.Space(8f);
            EditorGUILayout.LabelField(localization.Text("thisConflict"), EditorStyles.miniBoldLabel);
            using (new EditorGUILayout.HorizontalScope())
            {
                if (GUILayout.Button(localization.Text("overwrite"), GUILayout.Height(28f))) selected = ImportConflictDecision.Overwrite;
                if (GUILayout.Button(localization.Text("skip"), GUILayout.Height(28f))) selected = ImportConflictDecision.Skip;
            }
            EditorGUILayout.LabelField(localization.Text("remainingConflicts"), EditorStyles.miniBoldLabel);
            using (new EditorGUILayout.HorizontalScope())
            {
                if (GUILayout.Button(localization.Text("overwriteAll"), GUILayout.Height(28f))) selected = ImportConflictDecision.OverwriteAll;
                if (GUILayout.Button(localization.Text("skipAll"), GUILayout.Height(28f))) selected = ImportConflictDecision.SkipAll;
            }
            GUILayout.Space(8f);
            if (selected.HasValue)
            {
                Choose(selected.Value);
                GUIUtility.ExitGUI();
            }
        }

        private void Choose(ImportConflictDecision selected)
        {
            decision = selected;
            RememberPosition();
            Close();
        }

        private void OnDisable() => RememberPosition();

        private void RememberPosition()
        {
            if (position.width <= 0f || position.height <= 0f) return;
            lastPosition = position;
            hasPosition = true;
        }
    }
}
