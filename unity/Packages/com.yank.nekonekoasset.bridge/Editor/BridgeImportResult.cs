using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using NekoNekoAsset.Bridge.SmartPackage;
using UnityEditor;
using UnityEngine;

namespace NekoNekoAsset.Bridge
{
    public static partial class BridgeRuntime
    {
        private static void ShowImportResult(StoredBridgeRequest request)
        {
            if (Application.isBatchMode || stopped || request == null || !Terminal(request.receipt.stage)
                || request.receipt.stage == "cancelled") return;
            EditorApplication.delayCall += () => {
                if (!stopped) BridgeImportResultWindow.Show(request, Locale(request), () => CleanupLastWrites(request));
            };
        }

        // Only new, owned targets are eligible. Overwritten preexisting targets are
        // intentionally retained. Asset/meta hashes guard all subsequent edits.
        internal static int[] CleanupOwnedWrites(string project, StoredBridgeRequest request)
        {
            int removed = 0, retained = 0;
            if (request.cleanupDone || request.ownedWrites == null) return new[] { removed, retained };
            var writes = request.ownedWrites.OrderByDescending(write => write.path.Length).ToList();
            foreach (BridgeOwnedWrite write in writes)
            {
                try
                {
                    if (!ProjectPackagePath.TryGetAbsolutePath(project, write.path, out string target, out _))
                    { retained++; continue; }
                    var current = ObservedAsset.Capture(new FileAssetProbe(project), write.path);
                    if (!current.Exists) continue;
                    if (write.isFolder != current.IsFolder || !current.ContentKnown
                        || (!write.isFolder && current.Content.AssetHash != write.assetHash)
                        || current.Content.MetaHash != write.metaHash
                        || (write.isFolder && Directory.GetFileSystemEntries(target).Length != 0))
                    { retained++; continue; }
                    // Revalidate links and pair immediately before removing the owned bytes.
                    BridgeFiles.RejectLinks(target);
                    BridgeFiles.RejectLinks(target + ".meta");
                    if (write.isFolder) Directory.Delete(target, false);
                    else File.Delete(target);
                    if (File.Exists(target + ".meta")) File.Delete(target + ".meta");
                    removed++;
                }
                catch (IOException) { retained++; }
                catch (UnauthorizedAccessException) { retained++; }
            }
            // Keep implicit parents: empty-directory state alone cannot distinguish an
            // owned directory from one subsequently removed/recreated by another tool.
            request.cleanupDone = true;
            return new[] { removed, retained };
        }

        private static int[] CleanupLastWrites(StoredBridgeRequest request)
        {
            if (active != null || !Terminal(request.receipt.stage)) return new[] { 0, request.ownedWrites.Count };
            int[] counts = CleanupOwnedWrites(registry.projectRoot, request);
            Save(request);
            AssetDatabase.Refresh();
            return counts;
        }
    }

    internal sealed class BridgeImportResultWindow : EditorWindow
    {
        private StoredBridgeRequest request;
        private BridgeLocalization labels;
        private Func<int[]> cleanup;
        private Vector2 scroll;
        private string cleanupSummary;
        internal static void Show(StoredBridgeRequest request, string locale, Func<int[]> cleanup)
        {
            var window = CreateInstance<BridgeImportResultWindow>();
            window.request = request;
            window.labels = new BridgeLocalization(locale);
            window.cleanup = cleanup;
            window.titleContent = new GUIContent(window.labels.Text("resultTitle"));
            window.minSize = new Vector2(440, 280);
            window.ShowUtility();
        }
        private void OnGUI()
        {
            if (request == null || labels == null) return;
            GUILayout.Label(labels.Text(request.receipt.stage == "completed" ? "completed" : "failed"), EditorStyles.boldLabel);
            int applied = request.receipt.results.Count(result => result.state == "applied");
            int failed = request.receipt.results.Count(result => result.state == "failed");
            int skipped = request.receipt.results.Count(result => result.state == "skipped");
            GUILayout.Label(labels.Format("resultCounts", applied, skipped, failed));
            if (!string.IsNullOrEmpty(request.receipt.error)) EditorGUILayout.HelpBox(request.receipt.error, MessageType.Warning);
            scroll = EditorGUILayout.BeginScrollView(scroll);
            foreach (BridgeFileResult result in request.receipt.results)
            {
                using (new EditorGUILayout.HorizontalScope())
                {
                    EditorGUILayout.SelectableLabel(result.path, GUILayout.Height(EditorGUIUtility.singleLineHeight));
                    GUILayout.Label(labels.Text(result.state), GUILayout.Width(65));
                }
                if (!string.IsNullOrEmpty(result.message)) GUILayout.Label(result.message, EditorStyles.wordWrappedMiniLabel);
            }
            foreach (string message in request.receipt.compileErrors) GUILayout.Label(message, EditorStyles.wordWrappedMiniLabel);
            EditorGUILayout.EndScrollView();
            if (!string.IsNullOrEmpty(cleanupSummary)) GUILayout.Label(cleanupSummary, EditorStyles.wordWrappedMiniLabel);
            using (new EditorGUILayout.HorizontalScope())
            {
                if (request.receipt.stage != "completed" && request.ownedWrites != null && request.ownedWrites.Count > 0 && !request.cleanupDone)
                    if (GUILayout.Button(labels.Text("cleanup"))
                        && EditorUtility.DisplayDialog(labels.Text("cleanup"), labels.Text("cleanupConfirm"), labels.Text("cleanup"), labels.Text("cancel")))
                    {
                        int[] counts = cleanup();
                        cleanupSummary = labels.Format("cleanupCounts", counts[0], counts[1]);
                    }
                GUILayout.FlexibleSpace();
                if (GUILayout.Button(labels.Text("close"))) Close();
            }
        }
    }
}
