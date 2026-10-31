use super::*;
use std::path::Path;

struct Fixture {
    root: PathBuf,
    host: LibraryHost,
}
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/verification/setup-deferred")
            .join(format!("authored-{}", uuid::Uuid::new_v4()));
        let device_dir = root.join("device");
        std::fs::create_dir_all(&device_dir).unwrap();
        std::fs::write(
            device_dir.join("preserved-device-fixture"),
            b"authored protected device bytes",
        )
        .unwrap();
        let candidate = validate_setup_candidate(root.join("NekoNekoDatabase"), false).unwrap();
        let setup = SetupState::new(&device_dir, candidate, "en").unwrap();
        Self {
            root,
            host: LibraryHost {
                device_dir,
                status: Mutex::new(LibraryStatus {
                    state: "setup".into(),
                    path: None,
                    default_path: None,
                    error_code: None,
                    locale: "en".into(),
                    missing_credentials: vec![],
                }),
                services: Mutex::new(None),
                setup: Mutex::new(Some(setup)),
                opening: AtomicBool::new(false),
                next_catalogue: Mutex::new(None),
                switching: Mutex::new(None),
            },
        }
    }
    fn setup(&self) -> SetupState {
        self.host.active_setup().unwrap().unwrap()
    }
    fn target(&self, name: &str) -> Catalogue {
        let parent = self.root.join(name);
        std::fs::create_dir(&parent).unwrap();
        open_selected_catalogue(parent, self.host.device_dir.clone(), None, true).unwrap()
    }
    fn finish(self) {
        let root = self.root.canonicalize().unwrap();
        let boundary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/verification/setup-deferred")
            .canonicalize()
            .unwrap();
        assert_eq!(root.parent(), Some(boundary.as_path()));
        assert!(uuid::Uuid::parse_str(
            root.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_prefix("authored-")
                .unwrap()
        )
        .is_ok());
        drop(self);
        fn no_links(path: &Path) {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                assert_eq!(metadata.file_attributes() & 0x400, 0);
            }
            if metadata.is_dir() {
                for child in std::fs::read_dir(path).unwrap() {
                    no_links(&child.unwrap().path());
                }
            }
        }
        no_links(&root);
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn account(store: &str, name: &str) -> Value {
    json!({"id":uuid::Uuid::new_v4().to_string(),"store":store,"name":name,"status":"disconnected"})
}
fn finish_args() -> Value {
    json!({"patch":{"locale":"zh-Hant","theme":"light","autoDownloadAll":true,"setupComplete":true}})
}

#[test]
fn partial_setup_and_missing_lock_markers_remain_recoverable() {
    let f = Fixture::new();
    let root = f.root.join("partial-library");
    std::fs::create_dir_all(root.join(".nna/data")).unwrap();
    std::fs::write(root.join(".nna/data/catalogue.sqlite3"), b"").unwrap();
    assert!(Catalogue::probe_setup_preferences(&root).unwrap().is_none());
    let catalogue = Catalogue::open_bound(&root, &f.host.device_dir).unwrap();
    catalogue
        .command("preferences", json!({"patch":{"setupComplete":true}}))
        .unwrap();
    drop(catalogue);
    std::fs::remove_file(root.join(".nna-library.lock")).unwrap();
    assert_eq!(
        Catalogue::probe_setup_preferences(&root).unwrap().unwrap()["setupComplete"],
        true
    );
    f.finish();
}

#[test]
fn library_errors_retain_typed_causes_and_expose_only_actionable_codes() {
    for (kind, expected) in [
        (
            std::io::ErrorKind::PermissionDenied,
            "library_permission_denied",
        ),
        (std::io::ErrorKind::StorageFull, "library_disk_full"),
        (
            std::io::ErrorKind::NotADirectory,
            "library_path_not_directory",
        ),
    ] {
        let error = anyhow::Error::new(std::io::Error::new(kind, "authored private path"))
            .context("storage.create_directory_failed");
        assert_eq!(UiError::from_library_error(&error).code, expected);
    }
    for (sqlite_code, expected) in [
        (rusqlite::ffi::SQLITE_PERM, "library_permission_denied"),
        (rusqlite::ffi::SQLITE_READONLY, "library_permission_denied"),
        (rusqlite::ffi::SQLITE_FULL, "library_disk_full"),
        (
            rusqlite::ffi::SQLITE_CANTOPEN,
            "library_database_unavailable",
        ),
        (rusqlite::ffi::SQLITE_NOTADB, "library_database_unavailable"),
        (
            rusqlite::ffi::SQLITE_CORRUPT,
            "library_database_unavailable",
        ),
    ] {
        let error = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(sqlite_code),
            Some("authored private database path".into()),
        ))
        .context("catalogue initialization");
        assert_eq!(UiError::from_library_error(&error).code, expected);
    }
    for code in [
        "library_in_use",
        "root_offline",
        "storage_link_forbidden",
        "catalogue.newer_schema",
        "library_preferences_write_failed",
    ] {
        assert_eq!(
            UiError::from_library_error(&anyhow::anyhow!(code)).code,
            code
        );
    }
    assert_eq!(
        UiError::from_library_error(&anyhow::anyhow!("storage.create_directory_failed")).code,
        "library_create_failed"
    );
    assert_eq!(
        UiError::from_library_error(&anyhow::anyhow!("authored raw OS error /private/path")).code,
        "library_create_failed"
    );
    let locator_failure = anyhow::Error::new(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "authored device path",
    ))
    .context("library_preferences_write_failed");
    assert_eq!(
        UiError::from_library_error(&locator_failure).code,
        "library_preferences_write_failed"
    );
}

#[test]
fn move_errors_name_the_failed_side_without_raw_messages() {
    let io = |kind: std::io::ErrorKind| {
        anyhow::Error::new(std::io::Error::new(kind, "authored private path"))
    };
    for (error, expected) in [
        (
            io(std::io::ErrorKind::PermissionDenied).context("storage.destination_unavailable"),
            "library_permission_denied",
        ),
        (
            io(std::io::ErrorKind::NotFound).context("storage.destination_unavailable"),
            "library_create_failed",
        ),
        (
            io(std::io::ErrorKind::StorageFull).context("storage.write_failed"),
            "library_disk_full",
        ),
        (
            io(std::io::ErrorKind::PermissionDenied).context("storage.read_failed"),
            "storage.move_source_unreadable",
        ),
        (anyhow::anyhow!("storage.low_space"), "library_disk_full"),
        (
            anyhow::anyhow!("storage.source_hash_mismatch"),
            "storage.move_source_unreadable",
        ),
        (
            anyhow::anyhow!("storage.migration_overlapping_roots"),
            "storage.migration_overlapping_roots",
        ),
        (
            anyhow::anyhow!("storage.destination_not_fresh"),
            "storage.destination_not_fresh",
        ),
        (anyhow::anyhow!("storage_busy"), "storage_busy"),
    ] {
        assert_eq!(UiError::from_move_error(&error).code, expected);
    }
    #[cfg(windows)]
    assert_eq!(
        UiError::from_move_error(
            &anyhow::Error::new(std::io::Error::from_raw_os_error(33))
                .context("storage.read_failed")
        )
        .code,
        "storage.move_file_in_use"
    );
}

#[test]
fn setup_path_file_failure_preserves_the_draft_and_allows_another_location() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let original = setup.candidate().unwrap();
    let path = fixture.root.join("blocked-by-file");
    std::fs::write(&path, b"authored preserved file").unwrap();
    let error = choose_setup_candidate(&setup, path.clone(), false, Some("ja")).unwrap_err();
    assert_eq!(
        UiError::from_library_error(&error).code,
        "library_path_not_directory"
    );
    assert_eq!(setup.candidate().unwrap(), original);
    assert_eq!(
        setup.catalogue.get("preferences", "app").unwrap().unwrap()["locale"],
        "en"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"authored preserved file");
    assert!(choose_setup_candidate(&setup, fixture.root.clone(), true, Some("ja")).unwrap());
    assert!(!original.exists());
    drop(setup);
    fixture.finish();
}

#[cfg(windows)]
#[test]
fn finish_readonly_lock_reports_permission_and_can_retry_without_losing_staged_accounts() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let staged = account("BOOTH", "Authored staged account");
    setup
        .catalogue
        .put("account", staged["id"].as_str().unwrap(), staged.clone())
        .unwrap();
    let candidate = setup.candidate().unwrap();
    std::fs::create_dir(&candidate).unwrap();
    let lock = candidate.join(".nna-library.lock");
    std::fs::write(&lock, b"").unwrap();
    let mut permissions = std::fs::metadata(&lock).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&lock, permissions).unwrap();
    let patch = setup_finish_patch(&setup, &finish_args()).unwrap();
    let error = materialize_setup(&setup, &fixture.host.device_dir, patch.clone())
        .err()
        .expect("readonly lock must reject Finish");
    assert_eq!(
        UiError::from_library_error(&error).code,
        "library_permission_denied"
    );
    assert_eq!(setup.candidate().unwrap(), candidate);
    assert_eq!(
        setup.catalogue.list("account").unwrap(),
        vec![staged.clone()]
    );
    assert!(profile::selected_library(&fixture.host.device_dir)
        .unwrap()
        .is_none());
    assert!(!candidate.join(".nna").exists());
    let mut permissions = std::fs::metadata(&lock).unwrap().permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&lock, permissions).unwrap();
    let (target, _) = materialize_setup(&setup, &fixture.host.device_dir, patch).unwrap();
    assert_eq!(target.list("account").unwrap()[0]["id"], staged["id"]);
    assert_eq!(
        std::fs::read(fixture.host.device_dir.join("preserved-device-fixture")).unwrap(),
        b"authored protected device bytes"
    );
    drop(target);
    drop(setup);
    fixture.finish();
}

#[test]
fn fresh_default_is_confirmed_without_materializing_any_library_or_database() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let candidate = setup.candidate().unwrap();
    assert!(!candidate.exists());
    assert!(profile::selected_library(&fixture.host.device_dir)
        .unwrap()
        .is_none());
    for name in ["catalogue.sqlite3", "ai-chat.sqlite3", "cache"] {
        assert!(!fixture.host.device_dir.join(name).exists());
    }
    let status = active_library_status(&fixture.host).unwrap();
    assert_eq!(status.state, "setup");
    assert_eq!(status.path, Some(candidate.clone()));
    let state = setup
        .catalogue
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    for key in ["roots", "accounts", "jobs", "products"] {
        assert!(state[key].as_array().unwrap().is_empty());
    }
    assert_eq!(state["preferences"]["setupComplete"], false);
    assert_eq!(state["preferences"]["autoDownloadAll"], false);
    assert!(
        fixture.host.services().is_err(),
        "storage/AI/Unity require a materialized library"
    );
    assert!(!candidate.exists());
    assert!(crate::library_lock::LibraryLock::acquire_existing(&fixture.root).is_err());
    assert!(!fixture.root.join(".nna-library.lock").exists());
    drop(setup);
    fixture.finish();
}

#[test]
fn setup_candidate_changes_keep_accounts_drafts_and_device_guards_without_writes() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let original = account("BOOTH", "Authored staged account");
    setup
        .catalogue
        .put(
            "account",
            original["id"].as_str().unwrap(),
            original.clone(),
        )
        .unwrap();
    setup
        .catalogue
        .command(
            "preferences",
            json!({"patch":{"theme":"light","autoDownloadAll":true}}),
        )
        .unwrap();
    for name in ["first", "second", "first"] {
        let parent = fixture.root.join(name);
        if !parent.exists() {
            std::fs::create_dir(&parent).unwrap();
        }
        assert!(choose_setup_candidate(&setup, parent.clone(), true, Some("ja")).unwrap());
        let candidate = setup.candidate().unwrap();
        assert_eq!(
            candidate,
            parent.canonicalize().unwrap().join("NekoNekoDatabase")
        );
        assert!(!candidate.exists());
        assert!(std::fs::read_dir(parent).unwrap().next().is_none());
        assert_eq!(
            setup.catalogue.list("account").unwrap(),
            vec![original.clone()]
        );
        assert_eq!(
            setup.catalogue.get("preferences", "app").unwrap().unwrap()["autoDownloadAll"],
            true
        );
        assert_eq!(
            active_library_status(&fixture.host).unwrap().path,
            Some(candidate)
        );
    }
    assert!(profile::selected_library(&fixture.host.device_dir)
        .unwrap()
        .is_none());
    for name in ["booth.lock", "gumroad.lock"] {
        let duplicate = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.host.device_dir.join(name))
            .unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&duplicate).is_err());
    }
    let other = Catalogue::open_setup(&fixture.root.join("other-device")).unwrap();
    assert_eq!(
        setup
            .stores
            .rebind_catalogue(other)
            .err()
            .unwrap()
            .to_string(),
        "STORE_DEVICE_MISMATCH"
    );
    assert_eq!(
        std::fs::read(fixture.host.device_dir.join("preserved-device-fixture")).unwrap(),
        b"authored protected device bytes"
    );
    drop(setup);
    fixture.finish();
}

#[test]
fn setup_candidate_failure_and_completed_library_probe_preserve_staging_and_database() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let original = setup.candidate().unwrap();
    assert!(
        choose_setup_candidate(&setup, fixture.root.join("missing/parent"), true, None).is_err()
    );
    assert!(!fixture.root.join("missing").exists());
    assert_eq!(setup.candidate().unwrap(), original);
    let completed = fixture.target("completed");
    completed
        .command(
            "preferences",
            json!({"patch":{"setupComplete":true,"theme":"stored","autoDownloadAll":true}}),
        )
        .unwrap();
    let completed_root = completed.library_root().unwrap().to_path_buf();
    let database = completed.data_dir().join("catalogue.sqlite3");
    assert_eq!(
        choose_setup_candidate(&setup, completed_root.clone(), true, None)
            .unwrap_err()
            .to_string(),
        "library_in_use"
    );
    assert_eq!(setup.candidate().unwrap(), original);
    drop(completed);
    let bytes = std::fs::read(&database).unwrap();
    assert!(
        !choose_setup_candidate(&setup, completed_root.clone(), true, Some("ja")).unwrap(),
        "completed candidates keep the immediate Open/restart path"
    );
    assert_eq!(setup.candidate().unwrap(), original);
    assert_eq!(std::fs::read(&database).unwrap(), bytes);
    for suffix in ["-journal", "-wal", "-shm"] {
        assert!(!PathBuf::from(format!("{}{suffix}", database.display())).exists());
    }
    assert!(profile::selected_library(&fixture.host.device_dir)
        .unwrap()
        .is_none());
    let occupied = crate::library_lock::LibraryLock::acquire(&completed_root).unwrap();
    assert!(open_selected_catalogue(
        completed_root.clone(),
        fixture.host.device_dir.clone(),
        None,
        false
    )
    .is_err());
    drop(occupied);
    let normal =
        open_selected_catalogue(completed_root, fixture.host.device_dir.clone(), None, false)
            .unwrap();
    assert_eq!(
        normal.get("preferences", "app").unwrap().unwrap()["theme"],
        "stored"
    );
    drop(normal);
    drop(setup);
    fixture.finish();
}

#[test]
fn finish_creates_once_commits_accounts_choices_and_retains_exclusive_restart_lock() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let staged = account("Gumroad", "Authored staged account");
    setup
        .catalogue
        .put("account", staged["id"].as_str().unwrap(), staged.clone())
        .unwrap();
    let root = setup.candidate().unwrap();
    assert_eq!(setup_finish_patch(&setup, &json!({"patch":{"sensitiveMode":"invalid","autoDownloadAll":true,"setupComplete":true}})).unwrap_err().to_string(), "catalogue.invalid_preferences");
    assert!(!root.exists());
    assert_eq!(
        setup.catalogue.get("preferences", "app").unwrap().unwrap()["autoDownloadAll"],
        false
    );
    let active = setup.catalogue.begin_catalogue_work().unwrap();
    assert!(setup.catalogue.begin_restore_work().is_err());
    drop(active);
    fixture.host.begin_opening().unwrap();
    assert_eq!(
        fixture.host.begin_opening().unwrap_err().code,
        "library_opening"
    );
    let permit = setup.catalogue.begin_restore_work().unwrap();
    let patch = setup_finish_patch(&setup, &finish_args()).unwrap();
    let (target, preferences) = materialize_setup(&setup, &fixture.host.device_dir, patch).unwrap();
    assert!(target.data_dir().join("catalogue.sqlite3").is_file());
    assert_eq!(target.list("account").unwrap(), vec![staged]);
    assert_eq!(preferences["setupComplete"], true);
    assert_eq!(preferences["locale"], "zh-Hant");
    assert_eq!(preferences["theme"], "light");
    assert_eq!(preferences["autoDownloadAll"], true);
    assert_eq!(
        setup.catalogue.get("preferences", "app").unwrap().unwrap()["setupComplete"],
        false
    );
    assert!(
        target.list("job").unwrap().is_empty(),
        "initial sync waits for replacement startup"
    );
    assert!(crate::library_lock::LibraryLock::acquire(target.library_root().unwrap()).is_err());
    assert_eq!(
        profile::selected_library(&fixture.host.device_dir)
            .unwrap()
            .unwrap()
            .path,
        target.library_root().unwrap()
    );
    *fixture.host.next_catalogue.lock().unwrap() = Some(target);
    let reply = retain_setup_finish(&fixture.host, preferences, permit).unwrap();
    assert_eq!(reply["restartRequired"], true);
    assert!(setup.catalogue.begin_catalogue_work().is_err());
    assert_eq!(
        fixture.host.services().err().unwrap().code,
        "library_opening"
    );
    fixture.host.switching.lock().unwrap().take();
    fixture.host.next_catalogue.lock().unwrap().take();
    fixture.host.setup.lock().unwrap().take();
    drop(setup);
    let reopened = Catalogue::open_bound(&root, &fixture.host.device_dir).unwrap();
    let replacement = Services::new(reopened, &fixture.host.device_dir).unwrap();
    assert!(!replacement.setup_only);
    drop(replacement);
    fixture.finish();
}

#[test]
fn finish_locator_and_sql_failures_roll_back_existing_records_and_retry_staged_accounts() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let existing = fixture.target("existing-incomplete");
    let root = existing.library_root().unwrap().to_path_buf();
    existing
        .command(
            "preferences",
            json!({"patch":{"theme":"stored","uiScale":180}}),
        )
        .unwrap();
    let saved_preferences = existing.get("preferences", "app").unwrap().unwrap();
    let saved_account = account("Jinxxy", "Existing authored account");
    existing
        .put(
            "account",
            saved_account["id"].as_str().unwrap(),
            saved_account.clone(),
        )
        .unwrap();
    let database = existing.data_dir().join("catalogue.sqlite3");
    drop(existing);
    profile::select_library(
        &fixture.host.device_dir,
        &profile::LibrarySelection {
            path: root.clone(),
            locale: "ja".into(),
        },
    )
    .unwrap();
    let locator = std::fs::read(fixture.host.device_dir.join("library.json")).unwrap();
    let bytes = std::fs::read(&database).unwrap();
    assert!(choose_setup_candidate(&setup, root.clone(), false, None).unwrap());
    assert_eq!(std::fs::read(&database).unwrap(), bytes);
    let staged = account("BOOTH", "New authored staged account");
    setup
        .catalogue
        .put("account", staged["id"].as_str().unwrap(), staged.clone())
        .unwrap();
    let patch = setup_finish_patch(&setup, &finish_args()).unwrap();
    std::fs::create_dir(fixture.host.device_dir.join("library.json.pending")).unwrap();
    assert!(materialize_setup(&setup, &fixture.host.device_dir, patch.clone()).is_err());
    assert_eq!(
        std::fs::read(fixture.host.device_dir.join("library.json")).unwrap(),
        locator
    );
    std::fs::remove_dir(fixture.host.device_dir.join("library.json.pending")).unwrap();
    let reopened = Catalogue::open_bound(&root, &fixture.host.device_dir).unwrap();
    assert_eq!(
        reopened.get("preferences", "app").unwrap().unwrap(),
        saved_preferences
    );
    assert_eq!(
        reopened.list("account").unwrap(),
        vec![saved_account.clone()]
    );
    drop(reopened);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER fixture_setup_failure BEFORE UPDATE ON entities WHEN NEW.kind='preferences' BEGIN SELECT RAISE(ABORT,'fixture setup failure'); END;").unwrap();
    drop(connection);
    assert!(materialize_setup(&setup, &fixture.host.device_dir, patch.clone()).is_err());
    assert_eq!(
        std::fs::read(fixture.host.device_dir.join("library.json")).unwrap(),
        locator
    );
    let reopened = Catalogue::open_bound(&root, &fixture.host.device_dir).unwrap();
    assert_eq!(
        reopened.get("preferences", "app").unwrap().unwrap(),
        saved_preferences
    );
    assert_eq!(
        reopened.list("account").unwrap(),
        vec![saved_account.clone()]
    );
    drop(reopened);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("DROP TRIGGER fixture_setup_failure")
        .unwrap();
    drop(connection);
    let (target, prefs) = materialize_setup(&setup, &fixture.host.device_dir, patch).unwrap();
    assert_eq!(
        prefs["uiScale"], 180,
        "unrelated target preferences survive setup"
    );
    assert_eq!(
        target
            .get("account", saved_account["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        saved_account
    );
    assert_eq!(
        target
            .get("account", staged["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        staged
    );
    assert_eq!(target.list("account").unwrap().len(), 2);
    assert_eq!(
        std::fs::read(fixture.host.device_dir.join("preserved-device-fixture")).unwrap(),
        b"authored protected device bytes"
    );
    drop(target);
    drop(setup);
    fixture.finish();
}

#[test]
fn open_existing_library_rejects_unrelated_folders_without_writes() {
    let fixture = Fixture::new();
    for populated in [false, true] {
        let path = fixture.root.join(if populated {
            "unrelated-files"
        } else {
            "empty"
        });
        std::fs::create_dir(&path).unwrap();
        let original = path.join("keep.txt");
        if populated {
            std::fs::write(&original, b"unrelated authored bytes").unwrap();
        }
        let error =
            open_selected_catalogue(path.clone(), fixture.host.device_dir.clone(), None, false)
                .err()
                .expect("Open must not initialize a folder without a catalogue");
        assert_eq!(error.to_string(), "library_database_unavailable");
        assert!(!path.join(".nna").exists());
        assert_eq!(
            std::fs::read_dir(&path).unwrap().count(),
            usize::from(populated)
        );
        if populated {
            assert_eq!(
                std::fs::read(&original).unwrap(),
                b"unrelated authored bytes"
            );
        }
    }
    assert!(profile::selected_library(&fixture.host.device_dir)
        .unwrap()
        .is_none());
    fixture.finish();
}

#[test]
fn blocked_parent_selection_retries_the_child_without_adopting_the_parent() {
    let fixture = Fixture::new();
    let occupied = fixture.target("shared-parent");
    let root = occupied.library_root().unwrap().to_path_buf();
    let parent = root.parent().unwrap().to_path_buf();
    let unrelated = parent.join("keep.txt");
    std::fs::write(&unrelated, b"unrelated authored bytes").unwrap();
    let database = occupied.data_dir().join("catalogue.sqlite3");
    let original = std::fs::read(&database).unwrap();
    for new_library in [true, false, true] {
        assert!(open_selected_catalogue(
            parent.clone(),
            fixture.host.device_dir.clone(),
            None,
            new_library,
        )
        .is_err());
        assert!(!parent.join(".nna").exists());
        assert_eq!(std::fs::read(&database).unwrap(), original);
        assert_eq!(
            std::fs::read(&unrelated).unwrap(),
            b"unrelated authored bytes"
        );
        assert!(profile::selected_library(&fixture.host.device_dir)
            .unwrap()
            .is_none());
    }
    drop(occupied);
    let reopened =
        open_selected_catalogue(parent.clone(), fixture.host.device_dir.clone(), None, true)
            .unwrap();
    assert_eq!(reopened.library_root().unwrap(), root);
    assert!(!parent.join(".nna").exists());
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"unrelated authored bytes"
    );
    drop(reopened);
    fixture.finish();
}

#[test]
fn finish_materializes_the_exact_custom_candidate_without_appending_again() {
    let fixture = Fixture::new();
    let setup = fixture.setup();
    let exact = fixture.root.join("exact-empty-root");
    std::fs::create_dir(&exact).unwrap();
    assert!(choose_setup_candidate(&setup, exact.clone(), false, None).unwrap());
    assert!(std::fs::read_dir(&exact).unwrap().next().is_none());
    let patch = setup_finish_patch(&setup, &finish_args()).unwrap();
    let (target, _) = materialize_setup(&setup, &fixture.host.device_dir, patch).unwrap();
    assert_eq!(
        target.library_root().unwrap(),
        exact.canonicalize().unwrap()
    );
    assert!(!exact.join("NekoNekoDatabase").exists());
    drop(target);
    drop(setup);
    fixture.finish();
}

#[test]
fn staged_session_cleanup_rearms_proven_rollback_but_retains_committed_accounts() {
    for fail in [true, false] {
        let fixture = Fixture::new();
        let setup = fixture.setup();
        let staged = account("BOOTH", "Authored Setup ownership fixture");
        let id = staged["id"].as_str().unwrap();
        setup.catalogue.put("account", id, staged.clone()).unwrap();
        setup.sessions.arm(stores::StoreProvider::Booth, id).unwrap();
        let profile = stores::session::profile_path(&fixture.host.device_dir, stores::StoreProvider::Booth, id).unwrap();
        std::fs::create_dir_all(profile.join("WebView/Cache")).unwrap();
        std::fs::write(profile.join("session.bin"), b"authored protected bytes").unwrap();
        std::fs::write(profile.join("WebView/Cache/authored"), b"authored browser bytes").unwrap();
        let patch = setup_finish_patch(&setup, &finish_args()).unwrap();
        if fail {
            std::fs::create_dir(fixture.host.device_dir.join("library.json.pending")).unwrap();
            assert!(materialize_setup(&setup, &fixture.host.device_dir, patch).is_err());
            std::fs::remove_dir(fixture.host.device_dir.join("library.json.pending")).unwrap();
        } else {
            let (target, _) = materialize_setup(&setup, &fixture.host.device_dir, patch).unwrap();
            assert_eq!(target.get("account", id).unwrap(), Some(staged.clone()));
            // Even an explicit rearm attempt cannot claim an account proven
            // present in target SQL after a successful/uncertain commit.
            rearm_unpublished_setup_sessions(&setup, &target, &[staged.clone()]);
            drop(target);
        }
        fixture.host.setup.lock().unwrap().take();
        drop(setup); // Model a closed/crashed Setup: release every lifetime lease.
        setup_sessions::cleanup_abandoned(&fixture.host.device_dir).unwrap();
        assert_eq!(profile.exists(), !fail);
        if !fail { assert_eq!(std::fs::read(profile.join("session.bin")).unwrap(), b"authored protected bytes"); }
        fixture.finish();
    }
}
