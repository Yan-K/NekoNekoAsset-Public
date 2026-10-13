//! A sequentially shared library keeps catalogue facts and relative content together.
use super::*;
use crate::library_lock::LibraryLock;
use sha2::{Digest, Sha256};

pub(super) fn device_id(profile: &Path) -> Result<String> {
    let profile = profile.canonicalize()?;
    let machine = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    Ok(hex::encode(Sha256::digest(
        format!("{machine}\0{}", profile.to_string_lossy()).as_bytes(),
    )))
}

impl Catalogue {
    /// Inspect a current library without initialization, reconciliation or writes.
    /// Existing current libraries already own a stable lock-file identity.
    pub(crate) fn probe_setup_preferences(root: &Path) -> Result<Option<Value>> {
        let root = crate::storage::checked_library_root(root)?;
        let database = root.join(".nna/data/catalogue.sqlite3");
        crate::storage::validate_library_path(&database)?;
        if !database.exists() {
            return Ok(None);
        }
        let _lock = LibraryLock::acquire_existing(&root)?;
        ensure!(
            !root.join(crate::app_reset::DELETE_MARKER).exists(),
            "reset_pending"
        );
        let mut uri = url::Url::from_file_path(&database)
            .map_err(|_| anyhow::anyhow!("root_not_absolute"))?;
        uri.query_pairs_mut().append_pair("immutable", "1");
        let conn = Connection::open_with_flags(
            uri.as_str(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(version == 1, "catalogue.newer_schema");
        conn.prepare("SELECT summary,in_all FROM products LIMIT 0")?;
        conn.prepare("SELECT seq,kind,id,before_json,after_json FROM sync_outbox LIMIT 0")?;
        raw(&conn, "preferences", "app")
    }

    pub fn open_bound(root: &Path, device_dir: &Path) -> Result<Self> {
        let root = crate::storage::checked_library_root(root)?;
        let lock = Arc::new(LibraryLock::acquire(&root)?);
        ensure!(
            !root.join(crate::app_reset::DELETE_MARKER).exists(),
            "reset_pending"
        );
        // Even initialization follows the lock: a rejected opener cannot create
        // schema, reconcile a badge or start a service in the owned library.
        let data = root.join(".nna").join("data");
        crate::storage::validate_library_path(&data.join("catalogue.sqlite3"))?;
        let catalogue = Self::open_at(&data, device_dir, Some(root), Some(lock))?;
        let mut conn = catalogue
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        if rows(&conn, "root")?.is_empty() {
            let id = Uuid::new_v4().to_string();
            let tx = conn.transaction()?;
            set_raw(
                &tx,
                "root",
                &id,
                &json!({"id":id,"path":".","libraryRelative":true,"status":"online","active":true}),
            )?;
            let mut prefs =
                raw(&tx, "preferences", "app")?.context("catalogue.preferences_missing")?;
            prefs["libraryRootId"] = json!(id);
            set_raw(&tx, "preferences", "app", &prefs)?;
            tx.commit()?;
        }
        drop(conn);
        Ok(catalogue)
    }

    pub fn device_dir(&self) -> PathBuf {
        self.device_dir.clone()
    }
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn library_root(&self) -> Option<&Path> {
        self.library_root.as_deref()
    }
    pub fn check_library_access(&self) -> Result<()> {
        if let Some(lock) = &self.library_lock {
            lock.check_available()?;
        }
        Ok(())
    }
    pub fn check_library_available(&self) -> Result<()> {
        self.check_library_access()
    }
    pub(crate) fn retain_move_lock(
        &self,
        lock: Arc<LibraryLock>,
        permit: CatalogueWorkPermit,
    ) -> Result<()> {
        *self
            .pending_move
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))? = Some((lock, permit));
        Ok(())
    }
    pub(crate) fn release_move_hold(&self) -> Result<()> {
        self.pending_move
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?
            .take();
        Ok(())
    }
    pub(crate) fn snapshot_database(&self, path: &Path) -> Result<()> {
        self.check_library_access()?;
        ensure!(!path.exists(), "destination_not_empty");
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])?;
        let copy = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let valid: String = copy.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        ensure!(valid == "ok", "file_integrity_failed");
        Ok(())
    }

    pub(super) fn display_roots(&self, conn: &Connection) -> Result<Vec<Value>> {
        let mut roots = rows(conn, "root")?;
        if let Some(path) = self.library_root() {
            for root in &mut roots {
                if root["libraryRelative"] == true {
                    let relative = text(root, "path");
                    if relative != "." {
                        portable::validate_relative(relative)?;
                    }
                    root["path"] = json!(if relative == "." {
                        path.to_path_buf()
                    } else {
                        path.join(relative)
                    });
                }
            }
        }
        Ok(roots)
    }

    pub(super) fn resolve_previews(&self, result: &mut Value) -> Result<()> {
        let resolve = |product: &mut Value| -> Result<()> {
            if product["thumbnailHidden"] != true {
                if let Some(relative) = product["thumbnailPath"].as_str() {
                    portable::validate_relative(relative)?;
                    product["previewUrl"] = json!(self.dir.join(relative));
                }
            }
            Ok(())
        };
        if let Some(products) = result.get_mut("products").and_then(Value::as_array_mut) {
            for product in products {
                resolve(product)?;
            }
        }
        if result["id"].is_string() {
            resolve(result)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};

    #[test]
    fn crash_transaction_child() {
        let Some(root) = std::env::var_os("NNA_SHARED_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let catalogue =
            Catalogue::open_bound(&root, &root.parent().unwrap().join("child-device")).unwrap();
        catalogue
            .put(
                "product",
                "authored-crash",
                json!({"id":"authored-crash","title":"Committed authored title"}),
            )
            .unwrap();
        let mut conn = catalogue.inner.lock().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute("UPDATE entities SET body=json_set(body,'$.title','Uncommitted authored title') WHERE kind='product' AND id='authored-crash'", []).unwrap();
        tx.execute("UPDATE products SET title='Uncommitted authored title',body=json_set(body,'$.title','Uncommitted authored title'),summary=json_set(summary,'$.title','Uncommitted authored title') WHERE id='authored-crash'", []).unwrap();
        println!("SHARED_TRANSACTION_UNCOMMITTED");
        std::io::stdout().flush().unwrap();
        let mut exit = [0];
        std::io::stdin().read_exact(&mut exit).unwrap();
        drop(tx);
    }

    #[test]
    fn process_crash_rolls_back_shared_transaction_and_releases_library() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("shared-crash-{}", Uuid::new_v4()));
        let root = fixture.join("library");
        std::fs::create_dir_all(&root).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "catalogue::shared::tests::crash_transaction_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("NNA_SHARED_CRASH_ROOT", &root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            assert!(
                output.read_line(&mut line).unwrap() != 0,
                "authored child did not reach open transaction"
            );
            if line.contains("SHARED_TRANSACTION_UNCOMMITTED") {
                break;
            }
            line.clear();
        }
        assert_eq!(
            Catalogue::open_bound(&root, &fixture.join("second-device"))
                .err()
                .unwrap()
                .to_string(),
            "library_in_use"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let catalogue = Catalogue::open_bound(&root, &fixture.join("second-device")).unwrap();
        assert_eq!(
            catalogue.get("product", "authored-crash").unwrap().unwrap()["title"],
            "Committed authored title"
        );
        assert_eq!(
            catalogue
                .command("product_details", json!({"id":"authored-crash"}))
                .unwrap()["title"],
            "Committed authored title"
        );
        let integrity: String = catalogue
            .inner
            .lock()
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        drop(catalogue);
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn shared_catalogue_retains_edits_files_and_thumbnails_at_another_mount() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("shared-catalogue-{}", Uuid::new_v4()));
        let first = fixture.join("desktop-mount");
        let second = fixture.join("laptop-mount");
        std::fs::create_dir_all(&first).unwrap();
        let a = Catalogue::open_bound(&first, &fixture.join("device-a")).unwrap();
        let root = a.list("root").unwrap().remove(0);
        let id = text(&root, "id").to_owned();
        assert_eq!(root["path"], ".");
        let mode: String = a
            .inner
            .lock()
            .unwrap()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        assert_eq!(
            a.get("preferences", "app").unwrap().unwrap()["setupComplete"],
            false
        );
        a.put_many(vec![
            ("product".into(), "p".into(), json!({"id":"p","provider":"local","store":"Local Files","title":"Authored local","localManifest":{"complete":true,"fileIds":["f"],"directories":[]}})),
            ("remote_file".into(), "f".into(), json!({"id":"f","productId":"p","name":"Authored.txt","originKind":"local","revision":"r1"})),
            ("artifact".into(), "a".into(), json!({"id":"a","productId":"p","fileId":"f","rootId":id,"relativePath":"assets/Authored/Loose.txt","revision":"r1","status":"ready","originalAvailable":true})),
        ]).unwrap();
        std::fs::create_dir_all(first.join("assets/Authored")).unwrap();
        std::fs::write(first.join("assets/Authored/Loose.txt"), b"authored bytes").unwrap();
        std::fs::create_dir_all(a.data_dir().join("media")).unwrap();
        std::fs::write(
            a.data_dir().join("media/authored.png"),
            b"authored image bytes",
        )
        .unwrap();
        a.command("overlay", json!({"ids":["p"],"patch":{"title":"Human title","note":"Human note","thumbnailPath":"media/authored.png"}})).unwrap();
        a.command(
            "preferences",
            json!({"patch":{"setupComplete":true,"autoDownloadAll":false}}),
        )
        .unwrap();
        let clone = a.clone();
        drop(a);
        assert_eq!(
            Catalogue::open_bound(&first, &fixture.join("device-b"))
                .err()
                .unwrap()
                .to_string(),
            "library_in_use"
        );
        let device_a = clone.device_id().to_owned();
        drop(clone);
        std::fs::rename(&first, &second).unwrap();
        let b = Catalogue::open_bound(&second, &fixture.join("device-b")).unwrap();
        assert_ne!(device_a, b.device_id());
        assert_eq!(b.list("root").unwrap()[0]["id"], id);
        assert_eq!(b.list("root").unwrap()[0]["path"], ".");
        assert_eq!(
            b.command("state", json!({})).unwrap()["roots"][0]["path"],
            json!(second.canonicalize().unwrap())
        );
        let p = b.command("product_details", json!({"id":"p"})).unwrap();
        assert_eq!(p["title"], "Human title");
        assert_eq!(p["note"], "Human note");
        assert_eq!(p["status"], "ready");
        assert_eq!(p["thumbnailPath"], "media/authored.png");
        assert_eq!(
            p["previewUrl"],
            json!(b.data_dir().join("media/authored.png"))
        );
        assert_eq!(
            std::fs::read(second.join("assets/Authored/Loose.txt")).unwrap(),
            b"authored bytes"
        );
        assert_eq!(
            std::fs::read(b.data_dir().join("media/authored.png")).unwrap(),
            b"authored image bytes"
        );
        assert_eq!(
            b.get("preferences", "app").unwrap().unwrap()["setupComplete"],
            true
        );
        drop(b);
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn shared_open_rejects_newer_schema_without_upgrade_and_releases_lock() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("shared-schema-{}", Uuid::new_v4()));
        let data = fixture.join(".nna/data");
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("catalogue.sqlite3");
        Connection::open(&database)
            .unwrap()
            .execute_batch("PRAGMA user_version=2")
            .unwrap();
        assert_eq!(
            Catalogue::open_bound(&fixture, &fixture.join("device"))
                .err()
                .unwrap()
                .to_string(),
            "catalogue.newer_schema"
        );
        let version: i64 = Connection::open(database)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        drop(LibraryLock::acquire(&fixture).unwrap());
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
