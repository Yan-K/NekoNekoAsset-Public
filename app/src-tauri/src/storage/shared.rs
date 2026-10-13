//! Explicit movement of a current shared library, without upgrading old data.
use super::*;
use crate::library_lock::LibraryLock;

impl StorageService {
    pub fn move_library(&self, args: &Value, cancel: &AtomicBool) -> Result<Value> {
        let source = self
            .catalogue
            .library_root()
            .ok_or_else(|| anyhow::anyhow!("root_unconfigured"))?
            .to_path_buf();
        let permit = self.catalogue.begin_restore_work()?;
        io::check_cancel(cancel)?;
        let requested = Path::new(io::required_string(args, "path")?);
        ensure!(requested.is_absolute(), "root_not_absolute");
        archive::validate_no_links(requested)?;
        std::fs::create_dir_all(requested)?;
        let destination = checked_library_root(requested)?;
        ensure!(
            !source.starts_with(&destination) && !destination.starts_with(&source),
            "storage.migration_overlapping_roots"
        );
        let destination_lock = Arc::new(LibraryLock::acquire(&destination)?);
        let mut payload = self.payload(None, None, false)?;
        for item in &mut payload {
            item.relative_path = item
                .source_path
                .strip_prefix(&source)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid_path"))?
                .replace('\\', "/");
        }
        for directory in ["media", "cache", "recovery", "unity"] {
            self.shared_directory_payload(directory, &source, &mut payload)?;
        }
        let snapshot = self
            .catalogue
            .data_dir()
            .join(format!(".nna-move-db-{}.sqlite3", uuid::Uuid::new_v4()));
        let _snapshot = io::PartialGuard::new(snapshot.clone());
        self.catalogue.snapshot_database(&snapshot)?;
        let (hash, _) = io::hash_file(&snapshot)?;
        // Source snapshot is immutable while copying. It is installed last at
        // the ordinary current-format path, never by copying a live SQLite file.
        payload.push(portable::PayloadSource {
            root_id: "__catalogue".into(),
            relative_path: ".nna/data/catalogue.sqlite3".into(),
            source_path: snapshot,
            expected_hash: Some(hash),
            is_directory: false,
        });
        let result = portable::copy_into_locked_root(&source, &destination, &payload, cancel)?;
        io::check_cancel(cancel)?;
        self.catalogue.retain_move_lock(destination_lock, permit)?;
        Ok(
            json!({"libraryPath":destination,"restartRequired":true,"fileCount":result.file_count,"totalBytes":result.total_bytes}),
        )
    }

    fn shared_directory_payload(
        &self,
        name: &str,
        library: &Path,
        payload: &mut Vec<portable::PayloadSource>,
    ) -> Result<()> {
        let directory = self.catalogue.data_dir().join(name);
        if !directory.exists() {
            return Ok(());
        }
        let mut pending = vec![directory];
        while let Some(path) = pending.pop() {
            io::reject_link(&path)?;
            let relative = path
                .strip_prefix(library)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid_path"))?
                .replace('\\', "/");
            let is_directory = path.is_dir();
            payload.push(portable::PayloadSource {
                root_id: "__catalogue".into(),
                relative_path: relative,
                source_path: path.clone(),
                expected_hash: None,
                is_directory,
            });
            if is_directory {
                for entry in std::fs::read_dir(path)? {
                    pending.push(entry?.path());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_device_without_session_keeps_facts_and_does_not_start_downloads() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("shared-no-session-{}", uuid::Uuid::new_v4()));
        let root = fixture.join("library");
        std::fs::create_dir_all(&root).unwrap();
        let catalogue = Catalogue::open_bound(&root, &fixture.join("new-device")).unwrap();
        catalogue.put_many(vec![
            ("account".into(), "authored-account".into(), json!({"id":"authored-account","store":"BOOTH","status":"connected"})),
            ("product".into(), "p".into(), json!({"id":"p","provider":"booth","title":"Authored remote product","category":"Clothes"})),
            ("remote_file".into(), "f".into(), json!({"id":"f","productId":"p","accountId":"authored-account","sourceFileId":"authored-file","name":"Authored.unitypackage","revision":"r1"})),
            ("entitlement".into(), "e".into(), json!({"id":"e","productId":"p","accountId":"authored-account","fileIds":["f"],"manifestComplete":true})),
        ]).unwrap();
        catalogue
            .command("preferences", json!({"patch":{"autoDownloadAll":true}}))
            .unwrap();
        let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
        let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
        let service = StorageService::new(catalogue.clone(), stores).unwrap();
        service.reevaluate_automatic().await.unwrap();
        assert!(catalogue.list("job").unwrap().is_empty());
        assert!(service
            .enabled_files(service.remote_files("p").unwrap().0)
            .unwrap()
            .is_empty());
        let job = json!({"id":"queued-other-device","type":"download","state":"queued","productId":"p","fileIds":["f"]});
        catalogue
            .put("job", "queued-other-device", job.clone())
            .unwrap();
        assert!(!service
            .download_has_local_session(&job, &service.available_download_accounts().unwrap())
            .unwrap());
        let (next, changed) = service.queued_download().unwrap();
        assert!(next.is_none());
        assert!(changed);
        assert!(!service.queued_download().unwrap().1);
        assert_eq!(
            catalogue
                .get("job", "queued-other-device")
                .unwrap()
                .unwrap()["state"],
            "queued"
        );
        assert_eq!(
            catalogue
                .get("job", "queued-other-device")
                .unwrap()
                .unwrap()["stage"],
            "authRequired"
        );
        assert_eq!(
            catalogue
                .get("job", "queued-other-device")
                .unwrap()
                .unwrap()["errorCode"],
            "auth_required"
        );
        assert_eq!(
            catalogue
                .get("account", "authored-account")
                .unwrap()
                .unwrap()["status"],
            "connected"
        );
        assert_eq!(
            catalogue
                .command("product_details", json!({"id":"p"}))
                .unwrap()["status"],
            "notDownloaded"
        );
        catalogue.put("job", "foreign-local", json!({"id":"foreign-local","type":"storage:import_local","state":"failed","originDeviceId":"other-device","localPlan":{}})).unwrap();
        assert_eq!(
            service
                .retry_local_import("foreign-local")
                .await
                .unwrap_err()
                .to_string(),
            "local_source_required"
        );
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn shared_move_preserves_current_sql_relative_artifacts_and_cached_media() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("shared-move-{}", uuid::Uuid::new_v4()));
        let source = fixture.join("source");
        let destination = fixture.join("destination");
        std::fs::create_dir_all(&source).unwrap();
        let catalogue = Catalogue::open_bound(&source, &fixture.join("desktop")).unwrap();
        let root = catalogue.list("root").unwrap().remove(0);
        let root_id = root["id"].as_str().unwrap();
        std::fs::create_dir_all(source.join("assets/Authored")).unwrap();
        let file = source.join("assets/Authored/Input.txt");
        std::fs::write(&file, b"independent authored bytes").unwrap();
        let (hash, size) = io::hash_file(&file).unwrap();
        catalogue.put_many(vec![
            ("product".into(),"p".into(),json!({"id":"p","provider":"local","store":"Local Files","title":"Authored","localManifest":{"complete":true,"fileIds":["f"],"directories":[]}})),
            ("remote_file".into(),"f".into(),json!({"id":"f","productId":"p","originKind":"local","name":"Input.txt","revision":"r1"})),
            ("artifact".into(),"a".into(),json!({"id":"a","productId":"p","fileId":"f","revision":"r1","rootId":root_id,"relativePath":"assets/Authored/Input.txt","productDirectory":"assets/Authored","status":"ready","originalAvailable":true,"hash":hash,"sizeBytes":size})),
        ]).unwrap();
        for directory in ["media", "cache/previews"] {
            std::fs::create_dir_all(catalogue.data_dir().join(directory)).unwrap();
            std::fs::write(
                catalogue.data_dir().join(directory).join("Authored.png"),
                b"authored preview bytes",
            )
            .unwrap();
        }
        catalogue.command("overlay",json!({"ids":["p"],"patch":{"note":"keep this note","thumbnailPath":"media/Authored.png"}})).unwrap();
        let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
        let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
        let service = StorageService::new(catalogue.clone(), stores).unwrap();
        let result = service
            .move_library(&json!({"path":destination}), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(result["restartRequired"], true);
        assert_eq!(
            catalogue.begin_catalogue_work().err().unwrap().to_string(),
            "storage_busy"
        );
        assert_eq!(
            Catalogue::open_bound(&destination, &fixture.join("laptop"))
                .err()
                .unwrap()
                .to_string(),
            "library_in_use"
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"independent authored bytes");
        catalogue.release_move_hold().unwrap();
        drop(catalogue.begin_catalogue_work().unwrap());
        drop(LibraryLock::acquire(&destination).unwrap());
        drop(service);
        drop(catalogue);
        let reopened = Catalogue::open_bound(&destination, &fixture.join("laptop")).unwrap();
        let product = reopened
            .command("product_details", json!({"id":"p"}))
            .unwrap();
        assert_eq!(product["status"], "ready");
        assert_eq!(product["note"], "keep this note");
        assert_eq!(
            product["previewUrl"],
            json!(reopened.data_dir().join("media/Authored.png"))
        );
        assert_eq!(
            std::fs::read(destination.join("assets/Authored/Input.txt")).unwrap(),
            b"independent authored bytes"
        );
        assert!(reopened
            .data_dir()
            .join("cache/previews/Authored.png")
            .is_file());
        assert!(source.join(".nna/data/catalogue.sqlite3").is_file());
        drop(reopened);
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
