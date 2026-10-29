//! Explicit current-session reset, applied only after the old process releases its locks.
use crate::{catalogue::Catalogue, library_lock::LibraryLock, profile, storage};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const REQUEST: &str = "app-reset.json";
pub(crate) const DELETE_MARKER: &str = ".nna-reset-delete";
const LOCK: &str = ".nna-library.lock";
pub(crate) fn pending(device: &Path) -> bool {
    device.join(REQUEST).exists()
}
pub(crate) fn pending_library(device: &Path) -> Option<PathBuf> {
    serde_json::from_slice::<Request>(&fs::read(device.join(REQUEST)).ok()?)
        .ok()
        .map(|request| request.library)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Stage {
    Scheduled,
    Deleting,
    FinalCleanup,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    library: PathBuf,
    delete_library: bool,
    operation: String,
    root_id: String,
    stage: Stage,
}
fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (a.canonicalize(), b.canonicalize()) {
        return a == b;
    }
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
            == b.to_string_lossy()
                .trim_start_matches(r"\\?\")
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn checked_root(root: &Path, device: &Path) -> Result<PathBuf> {
    let root = storage::checked_library_root(root)?;
    let device = device.canonicalize()?;
    let executable = tauri::utils::platform::current_exe()?.canonicalize()?;
    ensure!(
        root.parent().is_some()
            && !device.starts_with(&root)
            && !root.starts_with(&device)
            && !executable.starts_with(&root),
        "reset_unsafe_location"
    );
    Ok(root)
}
fn tree(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(path) = pending.pop() {
        ensure!(path.starts_with(root), "reset_unsafe_location");
        storage::validate_library_path(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_dir() || metadata.is_file(),
            "reset_unsafe_location"
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
        paths.push(path);
    }
    Ok(paths)
}
pub(crate) fn schedule(device: &Path, catalogue: &Catalogue, delete_library: bool) -> Result<()> {
    let root = checked_root(
        catalogue.library_root().context("library_not_open")?,
        device,
    )?;
    ensure!(
        root.join(".nna/data/catalogue.sqlite3").is_file(),
        "reset_unsafe_location"
    );
    if delete_library {
        tree(&root)?;
    }
    let request = Request {
        library: root,
        delete_library,
        operation: uuid::Uuid::new_v4().to_string(),
        root_id: catalogue
            .get("preferences", "app")?
            .context("catalogue.preferences_missing")?["libraryRootId"]
            .as_str()
            .context("reset_invalid_request")?
            .to_owned(),
        stage: Stage::Scheduled,
    };
    let destination = device.join(REQUEST);
    storage::validate_library_path(&destination)?;
    ensure!(!destination.exists(), "reset_pending");
    write_request(device, &request)
}
fn write_request(device: &Path, request: &Request) -> Result<()> {
    let destination = device.join(REQUEST);
    storage::validate_library_path(&destination)?;
    let temporary = device.join(format!("app-reset-{}.pending", uuid::Uuid::new_v4()));
    storage::validate_library_path(&temporary)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&serde_json::to_vec(request)?)?;
        file.sync_all()?;
        drop(file);
        crate::ai_provider::replace_file(&temporary, &destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn write_marker(request: &Request) -> Result<()> {
    let path = request.library.join(DELETE_MARKER);
    storage::validate_library_path(&path)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let result = (|| {
        file.write_all(request.operation.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}
fn remove_library(device: &Path, request: &mut Request) -> Result<()> {
    let root = &request.library;
    storage::validate_library_path(&root.join(LOCK))?;
    let lock = LibraryLock::acquire(root)?;
    let marker = root.join(DELETE_MARKER);
    storage::validate_library_path(&marker)?;
    if matches!(request.stage, Stage::Scheduled) {
        // A durable request precedes its root marker. Bind an interrupted first
        // startup to the original database rather than a same-name replacement.
        tree(root)?;
        let database = root.join(".nna/data/catalogue.sqlite3");
        storage::validate_library_path(&database)?;
        let connection = rusqlite::Connection::open_with_flags(
            &database,
            // The prior process may have died with a hot rollback journal.
            // SQLite must recover it before reading the durable library identity.
            // The OS library lock is already held; never create or initialize SQL.
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(version == 1, "catalogue.newer_schema");
        let body: String = connection.query_row(
            "SELECT body FROM entities WHERE kind='preferences' AND id='app'",
            [],
            |row| row.get(0),
        )?;
        let preferences: serde_json::Value = serde_json::from_str(&body)?;
        ensure!(
            preferences["libraryRootId"].as_str() == Some(request.root_id.as_str()),
            "reset_invalid_request"
        );
        drop(connection);
        if !marker.exists() {
            write_marker(request)?;
        }
        ensure!(
            fs::read(&marker)? == request.operation.as_bytes(),
            "reset_invalid_request"
        );
        request.stage = Stage::Deleting;
        write_request(device, request)?;
    }
    ensure!(
        fs::read(&marker)? == request.operation.as_bytes(),
        "reset_invalid_request"
    );
    if matches!(request.stage, Stage::Deleting) {
        let paths = tree(root)?; // Validate the complete tree before the first destructive write.
        for path in paths.into_iter().rev() {
            if path == *root || path == root.join(LOCK) || path == marker {
                continue;
            }
            storage::validate_library_path(&path)?;
            if fs::symlink_metadata(&path)?.is_dir() {
                fs::remove_dir(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        request.stage = Stage::FinalCleanup;
        write_request(device, request)?;
    }
    lock.check_available()?;
    drop(lock);
    // The deletion marker makes other Apps refuse SQL initialization after acquiring
    // the lock. Final removals are nonrecursive: new external writes stop cleanup.
    fs::remove_file(root.join(LOCK))?;
    fs::remove_file(marker)?;
    if let Err(error) = fs::remove_dir(root) {
        let _ = write_marker(request);
        return Err(error.into());
    }
    Ok(())
}
pub(crate) fn apply_pending(device: &Path) -> Result<()> {
    let path = device.join(REQUEST);
    storage::validate_library_path(&path)?;
    if !path.exists() {
        return Ok(());
    }
    let mut request: Request = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|_| anyhow::anyhow!("reset_invalid_request"))?;
    ensure!(
        request.library.is_absolute() && uuid::Uuid::parse_str(&request.operation).is_ok(),
        "reset_invalid_request"
    );
    if let Some(selection) = profile::selected_library(device)? {
        // Missing paths are compared lexically so an interrupted final cleanup can retry.
        ensure!(
            same_path(&selection.path, &request.library),
            "reset_invalid_request"
        );
    } else {
        ensure!(
            matches!(request.stage, Stage::FinalCleanup),
            "reset_invalid_request"
        );
    }
    if !request.delete_library && matches!(request.stage, Stage::FinalCleanup) {
        // Settings are already durable; retry only the profile locator cleanup.
    } else if request.library.exists() {
        let root = checked_root(&request.library, device)?;
        if request.delete_library {
            request.library = root;
            remove_library(device, &mut request)?;
        } else {
            ensure!(
                root.join(".nna/data/catalogue.sqlite3").is_file(),
                "reset_invalid_request"
            );
            let catalogue = Catalogue::open_bound(&root, device)?;
            ensure!(
                catalogue
                    .get("preferences", "app")?
                    .context("catalogue.preferences_missing")?["libraryRootId"]
                    .as_str()
                    == Some(request.root_id.as_str()),
                "reset_invalid_request"
            );
            let mut preferences = crate::catalogue::default_preferences();
            preferences["libraryRootId"] = catalogue
                .get("preferences", "app")?
                .context("catalogue.preferences_missing")?["libraryRootId"]
                .clone();
            catalogue.put("preferences", "app", preferences)?;
            catalogue.prepare_sync_document()?;
            request.stage = Stage::FinalCleanup;
            write_request(device, &request)?;
        }
    } else {
        ensure!(
            request.delete_library && matches!(request.stage, Stage::FinalCleanup),
            "root_offline"
        );
    }
    // Protected sessions, provider credentials and chats are deliberately untouched.
    let locator = device.join("library.json");
    storage::validate_library_path(&locator)?;
    if locator.exists() {
        fs::remove_file(locator)?;
    }
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        device: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/settings-reset")
                .join(format!("authored-{}", uuid::Uuid::new_v4()));
            let root = base.join("library");
            let device = base.join("device");
            fs::create_dir_all(&root).unwrap();
            fs::create_dir(&device).unwrap();
            let catalogue = Catalogue::open_bound(&root, &device).unwrap();
            catalogue.put("product","authored",json!({"id":"authored","provider":"booth","title":"Authored","category":"Tool"})).unwrap();
            catalogue.command("preferences",json!({"patch":{"setupComplete":true,"theme":"light","uiScale":150,"autoDownloadAll":true,"presets":[{"id":"authored","name":"Authored","tokens":{}}]}})).unwrap();
            drop(catalogue);
            fs::create_dir(root.join("Assets")).unwrap();
            fs::write(root.join("Assets/authored.bin"), b"Authored asset").unwrap();
            for name in ["protected-session", "ai-settings.json", "chat-history"] {
                fs::write(device.join(name), b"Authored retained device data").unwrap();
            }
            profile::select_library(
                &device,
                &profile::LibrarySelection {
                    path: root.clone(),
                    locale: "zh-Hant".into(),
                },
            )
            .unwrap();
            Self { base, root, device }
        }
        fn schedule(&self, delete: bool) {
            let catalogue = Catalogue::open_bound(&self.root, &self.device).unwrap();
            let _permit = catalogue.begin_restore_work().unwrap();
            schedule(&self.device, &catalogue, delete).unwrap();
        }
        fn retained(&self) {
            for name in ["protected-session", "ai-settings.json", "chat-history"] {
                assert_eq!(
                    fs::read(self.device.join(name)).unwrap(),
                    b"Authored retained device data"
                );
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let boundary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/settings-reset")
                .canonicalize()
                .unwrap();
            let base = self.base.canonicalize().unwrap();
            assert!(
                base.starts_with(&boundary)
                    && base
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("authored-")
            );
            tree(&base).unwrap();
            fs::remove_dir_all(base).unwrap();
        }
    }
    #[test]
    fn settings_reset_preserves_products_assets_credentials_and_current_defaults() {
        let f = Fixture::new();
        f.schedule(false);
        apply_pending(&f.device).unwrap();
        assert!(!pending(&f.device));
        assert!(profile::selected_library(&f.device).unwrap().is_none());
        f.retained();
        assert_eq!(
            fs::read(f.root.join("Assets/authored.bin")).unwrap(),
            b"Authored asset"
        );
        let catalogue = Catalogue::open_bound(&f.root, &f.device).unwrap();
        assert!(catalogue.get("product", "authored").unwrap().is_some());
        let mut preferences = catalogue.get("preferences", "app").unwrap().unwrap();
        assert!(preferences["libraryRootId"].is_string());
        preferences.as_object_mut().unwrap().remove("libraryRootId");
        assert_eq!(preferences["avatarSourceAliases"], json!({}));
        preferences
            .as_object_mut()
            .unwrap()
            .remove("avatarSourceAliases");
        assert_eq!(preferences, crate::catalogue::default_preferences());
        catalogue.prepare_sync_document().unwrap();
        drop(catalogue);
        assert_eq!(
            Catalogue::open_bound(&f.root, &f.device)
                .unwrap()
                .get("preferences", "app")
                .unwrap()
                .unwrap()["setupComplete"],
            false
        );
    }
    #[test]
    fn deletion_recovers_a_crash_left_hot_journal_before_checking_library_identity() {
        const CHILD: &str = "NNA_AUTHORED_RESET_HOT_JOURNAL";
        if let Some(database) = std::env::var_os(CHILD) {
            let database = PathBuf::from(database).canonicalize().unwrap();
            let boundary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/settings-reset")
                .canonicalize()
                .unwrap();
            assert!(database.starts_with(boundary));
            assert_eq!(database.file_name().unwrap(), "catalogue.sqlite3");
            storage::validate_library_path(&database).unwrap();
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA cache_size=5; BEGIN IMMEDIATE; CREATE TABLE authored_crash_spill(payload BLOB);").unwrap();
            for _ in 0..64 {
                connection
                    .execute(
                        "INSERT INTO authored_crash_spill VALUES(zeroblob(4096))",
                        [],
                    )
                    .unwrap();
            }
            // Deliberately bypass Drop/rollback in this isolated child only.
            std::process::exit(0);
        }
        let f = Fixture::new();
        f.schedule(true);
        let database = f.root.join(".nna/data/catalogue.sqlite3");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "app_reset::tests::deletion_recovers_a_crash_left_hot_journal_before_checking_library_identity"])
            .env(CHILD, &database)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            child.creation_flags(0x08000000);
        }
        assert!(child.output().unwrap().status.success());
        let journal = database.with_file_name("catalogue.sqlite3-journal");
        assert!(fs::metadata(&journal).unwrap().len() > 512);
        let readonly = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let error = readonly
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
        drop(readonly);
        apply_pending(&f.device).unwrap();
        assert!(!f.root.exists());
        assert!(!pending(&f.device));
        f.retained();
    }

    #[test]
    fn reset_delete_removes_entire_selected_library_and_keeps_device_and_siblings() {
        let f = Fixture::new();
        fs::write(f.base.join("outside.bin"), b"Authored outside").unwrap();
        f.schedule(true);
        apply_pending(&f.device).unwrap();
        assert!(!f.root.exists());
        assert!(!pending(&f.device));
        assert!(profile::selected_library(&f.device).unwrap().is_none());
        f.retained();
        assert_eq!(
            fs::read(f.base.join("outside.bin")).unwrap(),
            b"Authored outside"
        );
        apply_pending(&f.device).unwrap();
    }
    #[test]
    fn reset_waits_for_library_owner_and_retains_request_for_retry() {
        let f = Fixture::new();
        f.schedule(true);
        let lock = LibraryLock::acquire(&f.root).unwrap();
        assert!(apply_pending(&f.device).is_err());
        assert!(pending(&f.device));
        assert!(profile::selected_library(&f.device).unwrap().is_some());
        assert!(f.root.join("Assets/authored.bin").exists());
        drop(lock);
        apply_pending(&f.device).unwrap();
        assert!(!f.root.exists());
    }
    #[test]
    fn deletion_marker_blocks_catalogue_initialization_and_retry_handles_missing_root() {
        let f = Fixture::new();
        f.schedule(true);
        let request: Request =
            serde_json::from_slice(&fs::read(f.device.join(REQUEST)).unwrap()).unwrap();
        write_marker(&request).unwrap();
        assert!(Catalogue::open_bound(&f.root, &f.device).is_err());
        apply_pending(&f.device).unwrap();
        assert!(!f.root.exists());
        fs::write(
            f.device.join(REQUEST),
            serde_json::to_vec(&Request {
                library: f.root.clone(),
                delete_library: true,
                operation: uuid::Uuid::new_v4().to_string(),
                root_id: "authored-removed-root".into(),
                stage: Stage::FinalCleanup,
            })
            .unwrap(),
        )
        .unwrap();
        profile::select_library(
            &f.device,
            &profile::LibrarySelection {
                path: f.root.clone(),
                locale: "en".into(),
            },
        )
        .unwrap();
        apply_pending(&f.device).unwrap();
        assert!(!pending(&f.device));
    }
    #[test]
    fn reset_rejects_device_overlap_and_busy_work_without_destroying_data() {
        let f = Fixture::new();
        assert!(checked_root(&f.base, &f.device).is_err());
        assert!(checked_root(&f.device, &f.device).is_err());
        let catalogue = Catalogue::open_bound(&f.root, &f.device).unwrap();
        let _work = catalogue.begin_catalogue_work().unwrap();
        assert!(catalogue.begin_restore_work().is_err());
        assert!(!pending(&f.device));
        assert!(f.root.join("Assets/authored.bin").exists());
    }
    #[cfg(windows)]
    #[test]
    fn partial_deletion_keeps_pending_locator_and_marker_until_retry_succeeds() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        f.schedule(true);
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(f.root.join("Assets/authored.bin"))
            .unwrap();
        assert!(apply_pending(&f.device).is_err());
        assert!(pending(&f.device));
        assert!(profile::selected_library(&f.device).unwrap().is_some());
        assert!(f.root.join(DELETE_MARKER).exists());
        assert!(Catalogue::open_bound(&f.root, &f.device).is_err());
        drop(file);
        apply_pending(&f.device).unwrap();
        assert!(!f.root.exists());
        f.retained();
    }
    #[cfg(windows)]
    #[test]
    fn deletion_rejects_junction_before_any_mutation() {
        use std::os::windows::process::CommandExt;
        let f = Fixture::new();
        let outside = f.base.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.bin"), b"Authored protected outside").unwrap();
        let link = f.root.join("linked");
        let link_normal = link
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(link.file_name().unwrap());
        let outside_normal = outside.canonicalize().unwrap();
        let output = std::process::Command::new("cmd")
            .args(["/d", "/c"])
            .raw_arg(format!(
                "mklink /J \"{}\" \"{}\"",
                link_normal.to_string_lossy().trim_start_matches(r"\\?\"),
                outside_normal.to_string_lossy().trim_start_matches(r"\\?\")
            ))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let catalogue = Catalogue::open_bound(&f.root, &f.device).unwrap();
        assert!(schedule(&f.device, &catalogue, true).is_err());
        assert!(!pending(&f.device));
        assert!(f.root.join("Assets/authored.bin").exists());
        assert_eq!(
            fs::read(outside.join("keep.bin")).unwrap(),
            b"Authored protected outside"
        );
        drop(catalogue);
        fs::remove_dir(link).unwrap();
    }
    #[test]
    fn final_cleanup_never_deletes_external_new_files_and_preserves_marker_on_failure() {
        let f = Fixture::new();
        f.schedule(true);
        let mut request: Request =
            serde_json::from_slice(&fs::read(f.device.join(REQUEST)).unwrap()).unwrap();
        request.stage = Stage::FinalCleanup;
        write_marker(&request).unwrap();
        write_request(&f.device, &request).unwrap();
        fs::write(
            f.root.join("new-external.bin"),
            b"Authored new external file",
        )
        .unwrap();
        for _ in 0..2 {
            assert!(apply_pending(&f.device).is_err());
            assert!(pending(&f.device));
            assert!(f.root.join(DELETE_MARKER).exists());
            assert_eq!(
                fs::read(f.root.join("new-external.bin")).unwrap(),
                b"Authored new external file"
            );
            assert_eq!(
                fs::read(f.root.join("Assets/authored.bin")).unwrap(),
                b"Authored asset"
            );
            assert!(Catalogue::open_bound(&f.root, &f.device).is_err());
        }
    }
    #[test]
    fn mismatched_marker_or_locator_never_deletes_a_replacement_library() {
        let f = Fixture::new();
        f.schedule(true);
        fs::write(f.root.join(DELETE_MARKER), b"Authored different operation").unwrap();
        assert!(apply_pending(&f.device).is_err());
        assert!(f.root.join("Assets/authored.bin").exists());
        fs::remove_file(f.device.join("library.json")).unwrap();
        assert!(apply_pending(&f.device).is_err());
        assert!(f.root.join("Assets/authored.bin").exists());
        assert!(pending(&f.device));
    }
    #[test]
    fn scheduled_request_recovers_before_and_after_marker_creation_without_guessing() {
        for created in [false, true] {
            let f = Fixture::new();
            f.schedule(true);
            let request: Request =
                serde_json::from_slice(&fs::read(f.device.join(REQUEST)).unwrap()).unwrap();
            assert!(matches!(request.stage, Stage::Scheduled));
            assert!(!f.root.join(DELETE_MARKER).exists());
            if created {
                write_marker(&request).unwrap();
            }
            apply_pending(&f.device).unwrap();
            assert!(!f.root.exists());
            assert!(!pending(&f.device));
            f.retained();
        }
    }
    #[test]
    fn scheduled_request_rejects_a_same_path_replacement_database() {
        let f = Fixture::new();
        f.schedule(true);
        let catalogue = Catalogue::open_bound(&f.root, &f.device).unwrap();
        catalogue
            .command(
                "preferences",
                json!({"patch":{"libraryRootId":"authored-replacement-root"}}),
            )
            .unwrap();
        drop(catalogue);
        assert!(apply_pending(&f.device).is_err());
        assert!(!f.root.join(DELETE_MARKER).exists());
        assert!(pending(&f.device));
        assert_eq!(
            fs::read(f.root.join("Assets/authored.bin")).unwrap(),
            b"Authored asset"
        );
    }
}
