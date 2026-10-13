using System;
using System.Collections.Generic;
using System.IO;
using NekoNekoAsset.Bridge.SmartPackage;
using UnityEditor;
using UnityEngine;

namespace NekoNekoAsset.Bridge
{
    public sealed class ImportReviewSelection
    {
        public ImportPlan Plan;
        public IReadOnlyList<LoadedPackage> Packages;
        public ConflictPolicy Policy;
        public string AssetsDestination;
    }

    /// <summary>Review only. The receiver applies the returned plan after this window closes.</summary>
    public sealed partial class ImportReviewWindow : EditorWindow
    {
        private const string ConflictPolicyPreference = "NekoNekoAsset.Bridge.Import.ConflictPolicy";
        private readonly List<LoadedPackage> loadedPackages = new List<LoadedPackage>();
        private Action<ImportReviewSelection> onConfirm;
        private Action onCancel;
        private BridgeLocalization localization;
        private ConflictPolicy importerPolicy = ConflictPolicy.Ask;
        private SnapshotAssetProbe importerProjectSnapshot;
        private Vector2 importerScroll;
        private Vector2 looseScroll;
        private bool individualDestinations;
        private string destination = "Assets";
        private string destinationDraft = "Assets";
        private bool destinationEditing;
        private LoadedPackage editingDestinationPackage;
        private string editingDestinationControl;
        private Rect editingDestinationRect;
        private readonly Dictionary<LoadedPackage, string> individualDestinationDrafts = new Dictionary<LoadedPackage, string>();
        private string error;
        private string reviewErrorMessage;
        private bool importBlocked;
        private bool destinationInvalid;
        private bool completed;
        private bool confirming;
        private bool hasLooseFiles;
        private int importerCheckVersion;
        private PackageAssetNode importerHighlightNode;

        public static ImportReviewWindow Show(
            IReadOnlyList<LoadedPackage> packages,
            string locale,
            Action<ImportReviewSelection> onConfirm,
            Action onCancel,
            string errorMessage = null,
            SnapshotAssetProbe preparedSnapshot = null,
            ImportPlan preparedPreview = null,
            ConflictPolicy? preparedPolicy = null)
        {
            if (packages == null) throw new ArgumentNullException(nameof(packages));
            if (onConfirm == null) throw new ArgumentNullException(nameof(onConfirm));
            var window = CreateInstance<ImportReviewWindow>();
            window.localization = new BridgeLocalization(locale);
            window.onConfirm = onConfirm;
            window.onCancel = onCancel;
            window.error = errorMessage;
            window.reviewErrorMessage = errorMessage;
            window.titleContent = new GUIContent(window.L("title"));
            window.minSize = new Vector2(640f, 300f);
            window.position = CenteredPosition(760f, 540f);
            window.importerPolicy = preparedPolicy ?? SavedPolicy();
            foreach (LoadedPackage package in packages)
            {
                if (package == null) continue;
                if (package.Tree == null) package.Tree = PackageAssetNode.BuildProjectTree(package.Entries);
                package.Tree.SortChildrenRecursive();
                package.LeafCount = package.Tree.CountLeaves();
                window.loadedPackages.Add(package);
                if (package.IsLooseFiles)
                {
                    window.hasLooseFiles = true;
                    window.individualDestinationDrafts[package] = package.LooseDestination ?? "Assets";
                    if (!string.IsNullOrEmpty(package.LooseDestination))
                        window.destination = package.LooseDestination;
                }
            }
            window.loadedPackages.Sort((a, b) => a.DropOrder.CompareTo(b.DropOrder));
            window.destinationDraft = window.destination;
            foreach (LoadedPackage package in window.loadedPackages)
                if (package.IsLooseFiles && (package.LooseDestination ?? "Assets") != window.destination)
                    window.individualDestinations = true;
            if (preparedSnapshot != null && preparedPreview != null)
            {
                window.importerProjectSnapshot = preparedSnapshot;
                window.importerPreviewProbe = new CachedPreviewProbe(preparedSnapshot, preparedPreview.Observations);
                window.ApplyImporterConflictPreview(preparedPreview);
            }
            else window.RefreshImporterConflictPreview(true);
            // A utility review window keeps cancellation/status available to the receiver.
            // The nested conflict window is modal only while a choice is being reviewed.
            window.ShowUtility();
            return window;
        }

        internal static ConflictPolicy SavedPolicy()
        {
            int saved = EditorPrefs.GetInt(ConflictPolicyPreference, (int)ConflictPolicy.Ask);
            return Enum.IsDefined(typeof(ConflictPolicy), saved) ? (ConflictPolicy)saved : ConflictPolicy.Ask;
        }

        private static Rect CenteredPosition(float width, float height)
        {
            Rect main = EditorGUIUtility.GetMainWindowPosition();
            return new Rect(main.x + (main.width - width) * .5f,
                main.y + (main.height - height) * .5f, width, height);
        }

        public void CancelPending()
        {
            if (completed) return;
            completed = true;
            ImportConflictWindow.CancelActive();
            Action callback = onCancel;
            onCancel = null;
            onConfirm = null;
            Close();
            callback?.Invoke();
        }

        private string L(string key) => (localization ?? (localization = new BridgeLocalization("en"))).Text(key);

        private void OnGUI()
        {
            bool commitDestinationOnReturn = Event.current.type == EventType.KeyDown
                && (Event.current.keyCode == KeyCode.Return || Event.current.keyCode == KeyCode.KeypadEnter);
            string focusedControl = GUI.GetNameOfFocusedControl();
            bool destinationFocused = focusedControl == "NNA.UnifiedDestination"
                || focusedControl.StartsWith("NNA.IndividualDestination:", StringComparison.Ordinal);
            bool commitDestinationOnClick = (destinationEditing || destinationFocused) && Event.current.type == EventType.MouseDown
                && !editingDestinationRect.Contains(GUIUtility.GUIToScreenPoint(Event.current.mousePosition));
            if (Event.current.type == EventType.KeyDown && Event.current.keyCode == KeyCode.Escape)
            {
                Event.current.Use();
                CancelPending();
                GUIUtility.ExitGUI();
                return;
            }
            // Loose rows keep the same shape when their destination changes. Commit
            // before an outside click so that click focuses the current source nodes.
            if (destinationEditing && (commitDestinationOnReturn || commitDestinationOnClick)) CommitDestinationEdit();
            if (commitDestinationOnClick)
            {
                GUI.FocusControl(null);
                EditorGUIUtility.editingTextField = false;
            }
            DrawImporterToolbar();
            BeginTreeGUI();
            using (new EditorGUILayout.HorizontalScope())
            {
                using (new EditorGUILayout.VerticalScope(GUILayout.Width((position.width - 14f) * .5f)))
                {
                    GUILayout.Label(".unitypackage", EditorStyles.boldLabel);
                    importerScroll = EditorGUILayout.BeginScrollView(importerScroll);
                    foreach (LoadedPackage package in loadedPackages)
                        if (!package.IsLooseFiles) DrawPackageCard(package);
                    EditorGUILayout.EndScrollView();
                }
                using (new EditorGUILayout.VerticalScope())
                {
                    GUILayout.Label(L("looseFiles"), EditorStyles.boldLabel);
                    if (hasLooseFiles)
                    {
                        using (new EditorGUILayout.HorizontalScope())
                        {
                            bool unified = GUILayout.Toggle(!individualDestinations, L("unified"), EditorStyles.radioButton);
                            bool individual = GUILayout.Toggle(individualDestinations, L("individual"), EditorStyles.radioButton);
                            if (individual && !individualDestinations) { SetDestinationMode(true); GUIUtility.ExitGUI(); }
                            else if (unified && individualDestinations) { SetDestinationMode(false); GUIUtility.ExitGUI(); }
                        }
                        if (!individualDestinations) DrawDestination();
                    }
                    looseScroll = EditorGUILayout.BeginScrollView(looseScroll);
                    DrawLooseFiles();
                    EditorGUILayout.EndScrollView();
                }
            }
            // Keyboard focus changes also commit the window-owned text draft.
            if (destinationEditing && GUI.GetNameOfFocusedControl() != editingDestinationControl)
                CommitDestinationEdit();
            if (!string.IsNullOrEmpty(error)) EditorGUILayout.HelpBox(error, MessageType.Error);
            DrawFooter();
        }

        private void DrawImporterToolbar()
        {
            using (new EditorGUILayout.HorizontalScope(EditorStyles.toolbar))
            {
                if (GUILayout.Button(L("selectAll"), EditorStyles.toolbarButton)) SetAllChecked(true);
                if (GUILayout.Button(L("selectNone"), EditorStyles.toolbarButton)) SetAllChecked(false);
                GUILayout.FlexibleSpace();
                GUILayout.Label(L("conflictPolicy"), EditorStyles.miniLabel);
                ConflictPolicy[] policies = { ConflictPolicy.Ask, ConflictPolicy.Overwrite, ConflictPolicy.Skip };
                int selected = Array.IndexOf(policies, importerPolicy);
                int next = EditorGUILayout.Popup(selected, new[] { L("ask"), L("overwrite"), L("skip") },
                    EditorStyles.toolbarPopup, GUILayout.Width(110f));
                if (next != selected && next >= 0)
                {
                    ClearReviewMessage();
                    importerPolicy = policies[next];
                    EditorPrefs.SetInt(ConflictPolicyPreference, (int)importerPolicy);
                    RefreshImporterConflictPreview(false);
                }
                GUILayout.FlexibleSpace();
                if (GUILayout.Button(L("expandAll"), EditorStyles.toolbarButton)) SetAllPackagesExpanded(true);
                if (GUILayout.Button(L("foldAll"), EditorStyles.toolbarButton)) SetAllPackagesExpanded(false);
            }
        }

        private void DrawDestination(LoadedPackage single = null)
        {
            using (new EditorGUILayout.HorizontalScope())
            {
                GUILayout.Label(L("destination"), GUILayout.Width(65f));
                string draft = single == null ? destinationDraft : individualDestinationDrafts[single];
                Rect field = EditorGUILayout.GetControlRect(false, EditorGUIUtility.singleLineHeight);
                string controlName = single == null ? "NNA.UnifiedDestination"
                    : "NNA.IndividualDestination:" + loadedPackages.IndexOf(single);
                GUI.SetNextControlName(controlName);
                EditorGUI.BeginChangeCheck();
                string next = EditorGUI.TextField(field, draft);
                if (EditorGUI.EndChangeCheck() && next != draft)
                {
                    SetDestinationDraft(next, single);
                    destinationEditing = true;
                    editingDestinationPackage = single;
                    editingDestinationControl = controlName;
                }
                if (Event.current.type == EventType.Repaint && GUI.GetNameOfFocusedControl() == controlName)
                    editingDestinationRect = new Rect(GUIUtility.GUIToScreenPoint(field.position), field.size);
                if (GUILayout.Button(L("choose"), GUILayout.Width(65f))) ChooseDestination(single);
            }
        }

        private void SetDestinationDraft(string candidate, LoadedPackage single = null)
        {
            if (single == null) destinationDraft = candidate;
            else individualDestinationDrafts[single] = candidate;
            bool wasInvalid = destinationInvalid;
            destinationInvalid = HasInvalidDestinationDraft();
            if (destinationInvalid)
            {
                importBlocked = true;
                error = L("invalidDestination");
            }
            else if (wasInvalid) RefreshImporterConflictPreview(false);
        }

        private void CommitDestinationEdit()
        {
            if (!destinationEditing) return;
            LoadedPackage single = editingDestinationPackage;
            destinationEditing = false;
            editingDestinationPackage = null;
            editingDestinationControl = null;
            ApplyDestination(single == null ? destinationDraft : individualDestinationDrafts[single], single);
        }

        private void ChooseDestination(LoadedPackage single = null)
        {
            CommitDestinationEdit();
            GUI.FocusControl(null);
            EditorGUIUtility.editingTextField = false;
            string projectRoot = Path.GetDirectoryName(Application.dataPath);
            string initial = Path.Combine(projectRoot, single == null ? destination : single.LooseDestination ?? "Assets");
            if (!Directory.Exists(initial)) initial = Application.dataPath;
            string picked = EditorUtility.OpenFolderPanel(L("chooseDestination"), initial, string.Empty);
            if (string.IsNullOrEmpty(picked)) return;
            string assets = Path.GetFullPath(Application.dataPath).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            string chosen = Path.GetFullPath(picked).TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            StringComparison comparison = Application.platform == RuntimePlatform.WindowsEditor
                ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
            string candidate;
            if (string.Equals(chosen, assets, comparison)) candidate = "Assets";
            else if (chosen.StartsWith(assets + Path.DirectorySeparatorChar, comparison))
                candidate = "Assets/" + chosen.Substring(assets.Length + 1).Replace('\\', '/');
            else
            {
                destinationInvalid = true;
                importBlocked = true;
                error = L("invalidDestination");
                return;
            }
            ApplyDestination(candidate, single);
        }

        private bool ApplyDestination(string candidate, LoadedPackage single = null)
        {
            if (single == null) destinationDraft = candidate;
            else individualDestinationDrafts[single] = candidate;
            if (!ProjectPackagePath.TryNormalize(candidate, out string normalized, out _)
                || !(normalized == "Assets" || normalized.StartsWith("Assets/", StringComparison.Ordinal)))
            {
                destinationInvalid = true;
                importBlocked = true;
                error = L("invalidDestination");
                return false;
            }
            try
            {
                if (normalized != destination) ClearReviewMessage();
                bool remapped = false;
                foreach (LoadedPackage package in loadedPackages)
                    if (package.IsLooseFiles && (single == null || single == package) && package.LooseDestination != normalized)
                    {
                        LooseFilePackage.RemapDestination(package, normalized);
                        package.Tree.SortChildrenRecursive();
                        remapped = true;
                    }
                if (remapped)
                {
                    importerHighlightNode = null;
                    treeKeyboardFocus = false;
                    treeControlNames.Clear();
                    unifiedLooseTree = null;
                }
                if (single == null)
                {
                    if (destination != normalized) unifiedLooseTree = null;
                    destination = normalized;
                    destinationDraft = normalized;
                }
                else individualDestinationDrafts[single] = normalized;
                destinationInvalid = HasInvalidDestinationDraft();
                if (destinationInvalid) importBlocked = true;
                error = destinationInvalid ? L("invalidDestination") : null;
                RefreshImporterConflictPreview(false);
                return true;
            }
            catch (Exception)
            {
                destinationInvalid = true;
                importBlocked = true;
                error = L("invalidDestination");
                return false;
            }
        }

        private void SetDestinationMode(bool individual)
        {
            CommitDestinationEdit();
            if (Event.current != null)
            {
                GUI.FocusControl(null);
                EditorGUIUtility.editingTextField = false;
            }
            individualDestinations = individual;
            importerHighlightNode = null;
            treeKeyboardFocus = false;
            treeControlNames.Clear();
            if (!individual) { ApplyDestination(destinationDraft); return; }
            foreach (LoadedPackage package in loadedPackages)
            {
                if (!package.IsLooseFiles || !ValidDestination(individualDestinationDrafts[package])) continue;
                ProjectPackagePath.TryNormalize(individualDestinationDrafts[package], out string normalized, out _);
                if (package.LooseDestination == normalized) continue;
                LooseFilePackage.RemapDestination(package, normalized);
                package.Tree.SortChildrenRecursive();
                unifiedLooseTree = null;
            }
            destinationInvalid = HasInvalidDestinationDraft();
            if (destinationInvalid) importBlocked = true;
            error = destinationInvalid ? L("invalidDestination") : null;
            RefreshImporterConflictPreview(false);
        }

        private bool HasInvalidDestinationDraft()
        {
            if (!individualDestinations) return !ValidDestination(destinationDraft);
            foreach (LoadedPackage package in loadedPackages)
                if (package.IsLooseFiles && !ValidDestination(individualDestinationDrafts[package])) return true;
            return false;
        }

        private static bool ValidDestination(string candidate)
            => ProjectPackagePath.TryNormalize(candidate, out string normalized, out _)
                && (normalized == "Assets" || normalized.StartsWith("Assets/", StringComparison.Ordinal));

        private void DrawFooter()
        {
            int selected = 0, total = 0;
            foreach (LoadedPackage package in loadedPackages)
            {
                EnsurePackageTally(package);
                selected += package.SelectedCount;
                total += package.LeafCount;
            }
            using (new EditorGUILayout.HorizontalScope())
            {
                GUILayout.Label(localization.Format("selectedCount", selected, total), EditorStyles.miniLabel);
                if (PreviewPending) GUILayout.Label(L("checkingTargets"), EditorStyles.miniLabel);
                GUILayout.FlexibleSpace();
                if (GUILayout.Button(L("cancel"), GUILayout.MinWidth(80f)))
                {
                    CancelPending();
                    GUIUtility.ExitGUI();
                }
                using (new EditorGUI.DisabledScope(selected == 0 || confirming || importBlocked || PreviewPending))
                    if (GUILayout.Button(L("import"), GUILayout.MinWidth(90f)))
                    {
                        ConfirmSelection();
                        if (completed) GUIUtility.ExitGUI();
                    }
            }
        }

        private void SetAllChecked(bool value)
        {
            ClearReviewMessage();
            foreach (LoadedPackage package in loadedPackages) package.Tree?.SetChecked(value, true);
            RefreshImporterConflictPreview(false);
        }

        private void ClearReviewMessage() => reviewErrorMessage = null;

        private void OnEnable()
        {
            EditorApplication.update += UpdateImporterPreview;
            EditorApplication.projectChanged += OnReviewProjectChanged;
        }

        private void OnReviewProjectChanged()
        {
            if (!completed && loadedPackages.Count > 0) RefreshImporterConflictPreview(true);
        }

        private void OnFocus()
        {
            if (localization != null && !completed && !confirming && loadedPackages.Count > 0)
                Repaint();
        }

        private void ConfirmSelection()
        {
            if (completed || confirming || PreviewPending) return;
            confirming = true;
            try
            {
                if (hasLooseFiles && !individualDestinations && !ApplyDestination(destinationDraft)) return;
                if (hasLooseFiles && individualDestinations)
                    foreach (LoadedPackage package in loadedPackages)
                        if (package.IsLooseFiles && !ApplyDestination(individualDestinationDrafts[package], package)) return;
                // No project writes or dirty-target save during review; the writer revalidates later.
                SnapshotAssetProbe snapshot = SnapshotAssetProbe.Capture(includeContent: true);
                bool overwriteAll = false, skipAll = false;
                ImportPlan plan = ImportPlanBuilder.Build(loadedPackages, snapshot, importerPolicy, item =>
                {
                    if (completed) return false;
                    if (overwriteAll) return true;
                    if (skipAll) return false;
                    ImportConflictDecision decision = ImportConflictWindow.ShowDialog(item, localization);
                    if (decision == ImportConflictDecision.OverwriteAll) overwriteAll = true;
                    if (decision == ImportConflictDecision.SkipAll) skipAll = true;
                    return decision == ImportConflictDecision.Overwrite || decision == ImportConflictDecision.OverwriteAll;
                });
                if (completed) return;
                if (!plan.CanApply)
                {
                    importBlocked = true;
                    error = string.Join("\n", plan.Errors);
                    return;
                }
                if (plan.OrderedItems.Count == 0)
                {
                    importBlocked = true;
                    error = L("empty");
                    return;
                }
                var selection = new ImportReviewSelection
                {
                    Plan = plan,
                    Packages = new List<LoadedPackage>(loadedPackages).AsReadOnly(),
                    Policy = importerPolicy,
                    AssetsDestination = hasLooseFiles ? destination : null
                };
                Action<ImportReviewSelection> callback = onConfirm;
                completed = true;
                onConfirm = null;
                onCancel = null;
                Close();
                // The review and nested modal have both closed before any writer/refresh starts.
                EditorApplication.delayCall += () => callback?.Invoke(selection);
            }
            catch (ExitGUIException) { throw; }
            catch (Exception)
            {
                importBlocked = true;
                error = L("invalidInput");
            }
            finally { confirming = false; }
        }

        private void OnDisable()
        {
            EditorApplication.update -= UpdateImporterPreview;
            EditorApplication.projectChanged -= OnReviewProjectChanged;
            if (completed)
            {
                ImportConflictWindow.ResetPosition();
                return;
            }
            completed = true;
            ImportConflictWindow.CancelActive();
            ImportConflictWindow.ResetPosition();
            Action callback = onCancel;
            onCancel = null;
            onConfirm = null;
            callback?.Invoke();
        }
    }
}
