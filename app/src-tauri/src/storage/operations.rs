use super::*;
use portable::PayloadSource;

fn restored_artifact_state(artifact: &Value) -> Option<&str> {
    if artifact["status"] == "unknown" && artifact["originalAvailable"] != true {
        artifact["restoreStatus"]
            .as_str()
            .filter(|state| matches!(*state, "ready" | "downloaded" | "extractionFailed"))
    } else {
        None
    }
}
fn recover_artifact_state(artifact: &mut Value, original_available: bool) {
    artifact["status"] = artifact["restoreStatus"].clone();
    artifact["originalAvailable"] = json!(original_available);
    artifact.as_object_mut().unwrap().remove("restoreStatus");
}

impl StorageService {
    pub(super) async fn launch_operation(
        &self,
        app: &tauri::AppHandle,
        action: &str,
        args: Value,
    ) -> Result<Value> {
        let work = if action == "restore_backup" {
            self.catalogue.begin_restore_work()?
        } else {
            self.catalogue.begin_catalogue_work()?
        };
        let id = uuid::Uuid::new_v4().to_string();
        let mut job = json!({"id":id,"type":format!("storage:{action}"),"state":"queued","createdAt":now(),"updatedAt":now()});
        self.catalogue.put("job", &id, job.clone())?;
        let service = self.clone();
        let app = app.clone();
        let action = action.to_owned();
        let cancel = self.cancellation(&id);
        let task_id = id.clone();
        tauri::async_runtime::spawn(async move {
            let _work = work;
            let _lock = service.operations.write().await;
            let result = if cancel.load(Ordering::Acquire) {
                Err(anyhow::anyhow!("cancelled"))
            } else {
                let _ = service.put_job(&mut job, "running", None);
                service.jobs_changed(&app, &task_id);
                let worker = service.clone();
                let stop = cancel.clone();
                let active_restore_job_id = task_id.clone();
                tokio::task::spawn_blocking(move || {
                    if action == "restore_backup" {
                        worker.restore(&args, &stop, Some(&active_restore_job_id))
                    } else {
                        worker.disk_operation(&action, &args, &stop)
                    }
                })
                .await
                .map_err(|_| anyhow::anyhow!("storage_failed"))
                .and_then(|r| r)
            };
            match result {
                Ok(value) => {
                    job["result"] = value;
                    let _ = service.put_job(&mut job, "completed", None);
                }
                Err(e) => {
                    let code = io::stable_error(&e);
                    let _ = service.put_job(
                        &mut job,
                        if code == "cancelled" {
                            "cancelled"
                        } else {
                            "needsAction"
                        },
                        Some(code),
                    );
                }
            }
            service.cancellations.lock().unwrap().remove(&task_id);
            service.changed(&app, Some(&task_id));
        });
        Ok(json!({"jobId":id}))
    }
    pub(super) fn payload(
        &self,
        root_filter: Option<&str>,
        products: Option<&HashSet<String>>,
        usable_only: bool,
    ) -> Result<Vec<PayloadSource>> {
        self.payload_records(
            &self.catalogue.list("artifact")?,
            &self.catalogue.list("product")?,
            root_filter,
            products,
            usable_only,
        )
    }
    fn payload_records(
        &self,
        artifacts: &[Value],
        product_records: &[Value],
        root_filter: Option<&str>,
        products: Option<&HashSet<String>>,
        usable_only: bool,
    ) -> Result<Vec<PayloadSource>> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        // Store products' current download lists; local products have none.
        let mut offered: HashMap<String, Option<HashSet<String>>> = HashMap::new();
        for a in artifacts {
            let root_id = io::required_string(&a, "rootId")?;
            if root_filter.is_some_and(|id| id != root_id) {
                continue;
            }
            if products
                .is_some_and(|ids| !a["productId"].as_str().is_some_and(|id| ids.contains(id)))
            {
                continue;
            }
            let root = self.root(Some(root_id))?;
            let root_path = self.root_path(&root)?;
            if usable_only {
                let Some(remote) = a["fileId"]
                    .as_str()
                    .map(|id| self.catalogue.get("remote_file", id))
                    .transpose()?
                    .flatten()
                else {
                    continue;
                };
                if artifact_id(&remote) != a["id"].as_str().unwrap_or("") {
                    continue;
                }
                // A kept store file exports its downloaded original even where
                // extraction failed; the store's own files must be ready.
                let product = io::required_string(&a, "productId")?;
                if a["status"] != "ready" && !offered.contains_key(product) {
                    let ids = if self
                        .catalogue
                        .get("product", product)?
                        .is_some_and(|p| p["provider"] != "local")
                    {
                        Some(
                            self.remote_files(product)?
                                .0
                                .iter()
                                .filter_map(|f| f["id"].as_str().map(str::to_owned))
                                .collect(),
                        )
                    } else {
                        None
                    };
                    offered.insert(product.to_owned(), ids);
                }
                let kept = offered.get(product).is_some_and(|ids| {
                    ids.as_ref()
                        .is_some_and(|ids| !ids.contains(a["fileId"].as_str().unwrap_or("")))
                });
                ensure!(
                    a["status"] == "ready"
                        || kept && matches!(a["status"].as_str(), Some("downloaded" | "extractionFailed")),
                    "file_not_usable"
                );
            }
            if a["originalAvailable"] == true && (!usable_only || a["extractedPath"].is_null()) {
                let rel = io::required_string(&a, "relativePath")?;
                if seen.insert((root_id.to_owned(), rel.to_owned())) {
                    result.push(PayloadSource {
                        root_id: root_id.to_owned(),
                        relative_path: rel.to_owned(),
                        source_path: io::safe_join(&root_path, rel)?,
                        expected_hash: Some(io::required_string(&a, "hash")?.to_owned()),
                        is_directory: false,
                    });
                }
            }
            if let Some(extracted) = a["extractedPath"].as_str() {
                for entry in a["extractedInventory"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?
                {
                    let rel = format!(
                        "{}/{}",
                        extracted,
                        io::required_string(entry, "relativePath")?
                    );
                    if seen.insert((root_id.to_owned(), rel.clone())) {
                        result.push(PayloadSource {
                            root_id: root_id.to_owned(),
                            relative_path: rel.clone(),
                            source_path: io::safe_join(&root_path, &rel)?,
                            expected_hash: if entry["isDirectory"] == true {
                                None
                            } else {
                                entry["hash"].as_str().map(str::to_owned)
                            },
                            is_directory: entry["isDirectory"] == true,
                        });
                    }
                }
            }
        }
        // Imported folder trees include empty directories independently of archive extraction.
        for product in product_records {
            if product["provider"] != "local"
                || products
                    .is_some_and(|ids| !product["id"].as_str().is_some_and(|id| ids.contains(id)))
            {
                continue;
            }
            let id = io::required_string(&product, "id")?;
            let Some(artifact) = artifacts.iter().find(|a| a["productId"] == id) else {
                continue;
            };
            let root_id = io::required_string(artifact, "rootId")?;
            if root_filter.is_some_and(|filter| filter != root_id) {
                continue;
            }
            let root = self.root_path(&self.root(Some(root_id))?)?;
            for relative in strings(&product["localManifest"]["directories"]) {
                if seen.insert((root_id.to_owned(), relative.clone())) {
                    let source_path = io::safe_join(&root, &relative)?;
                    ensure!(source_path.is_dir(), "file_missing");
                    result.push(PayloadSource {
                        root_id: root_id.to_owned(),
                        relative_path: relative,
                        source_path,
                        expected_hash: None,
                        is_directory: true,
                    });
                }
            }
        }
        Ok(result)
    }
    pub(super) fn backup_payload(&self, manifest: &Value) -> Result<Vec<PayloadSource>> {
        let artifacts = manifest["records"]["artifact"].as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?;
        let products = manifest["records"]["product"].as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?;
        for artifact in artifacts {
            ensure!(artifact["status"] != "ready" || artifact["originalAvailable"] == true
                || artifact["extractedPath"].is_string(), "backup_payload_missing");
        }
        // Derive bytes from the exact captured records, rather than reading a
        // newer artifact list after an account or product was removed.
        let mut payload = self.payload_records(artifacts, products, None, None, false)?;
        let by_path: HashMap<_, _> = payload.iter()
            .map(|source| ((source.root_id.as_str(), source.relative_path.as_str()), source)).collect();
        let roots: HashSet<_> = manifest["records"]["root"].as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?
            .iter().filter_map(|root| root["id"].as_str()).collect();
        let require = |root: &str, relative: &str, hash: &str, size: u64, directory: bool| -> Result<()> {
            let source = by_path.get(&(root, relative))
                .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?;
            ensure!(source.is_directory == directory
                && if directory { hash.is_empty() && size == 0 }
                   else { source.expected_hash.as_deref() == Some(hash)
                       && std::fs::metadata(&source.source_path)?.len() == size },
                "backup_payload_missing");
            Ok(())
        };
        for artifact in artifacts {
            let root = io::required_string(artifact, "rootId")?;
            ensure!(roots.contains(root), "backup_payload_missing");
            if artifact["originalAvailable"] == true {
                require(root, io::required_string(artifact, "relativePath")?,
                    io::required_string(artifact, "hash")?,
                    artifact["sizeBytes"].as_u64().ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?, false)?;
            }
            if let Some(base) = artifact["extractedPath"].as_str() {
                let entries = artifact["extractedInventory"].as_array()
                    .filter(|entries| !entries.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?;
                for entry in entries {
                    require(root, &format!("{base}/{}", io::required_string(entry, "relativePath")?),
                        entry["hash"].as_str().ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?,
                        entry["sizeBytes"].as_u64().ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?,
                        entry["isDirectory"] == true)?;
                }
            } else if matches!(artifact["status"].as_str(), Some("ready" | "downloaded" | "extractionFailed")) {
                ensure!(artifact["originalAvailable"] == true, "backup_payload_missing");
            }
        }
        let media = self.media_payload()?;
        let media_paths: HashSet<_> = media.iter().map(|source| source.relative_path.as_str()).collect();
        for relative in manifest["media"].as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))? {
            let relative = relative.as_str().ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?;
            ensure!(media_paths.contains(relative), "backup_payload_missing");
        }
        payload.extend(media);
        Ok(payload)
    }
    fn media_payload(&self) -> Result<Vec<PayloadSource>> {
        let mut result = Vec::new();
        let data = self.catalogue.data_dir();
        let media = data.join("media");
        if !media.exists() {
            return Ok(result);
        }
        let mut pending = vec![media];
        while let Some(dir) = pending.pop() {
            io::reject_link(&dir)?;
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                io::reject_link(&path)?;
                if path.is_dir() {
                    pending.push(path)
                } else {
                    let rel = path
                        .strip_prefix(&data)?
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid_path"))?
                        .replace('\\', "/");
                    result.push(PayloadSource {
                        root_id: "__media".into(),
                        relative_path: rel,
                        source_path: path,
                        expected_hash: None,
                        is_directory: false,
                    });
                }
            }
        }
        Ok(result)
    }
    pub(super) fn disk_operation(
        &self,
        action: &str,
        args: &Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        io::check_cancel(cancel)?;
        match action {
            "export_backup" => {
                let path = Path::new(io::required_string(args, "path")?);
                let catalogue = self.catalogue.manifest()?;
                let payload = self.backup_payload(&catalogue)?;
                let result = portable::create_backup(path, catalogue, &payload, cancel)?;
                Ok(json!({"fileCount":result.file_count,"totalBytes":result.total_bytes}))
            }
            "export_zip" => {
                let ids: HashSet<_> = strings(&args["productIds"]).into_iter().collect();
                ensure!(!ids.is_empty(), "invalid_arguments");
                for product in &ids {
                    let (files, _) = self.remote_files(product)?;
                    ensure!(!files.is_empty(), "manifest_incomplete");
                    for file in files {
                        let a = self
                            .catalogue
                            .get("artifact", &artifact_id(&file))?
                            .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
                        ensure!(a["status"] == "ready", "file_not_usable");
                    }
                }
                let payload = self.payload(None, Some(&ids), true)?;
                ensure!(!payload.is_empty(), "file_missing");
                let result = portable::export_zip(
                    Path::new(io::required_string(args, "path")?),
                    &payload,
                    cancel,
                )?;
                Ok(json!({"fileCount":result.file_count,"totalBytes":result.total_bytes}))
            }
            "migrate_root" => {
                ensure!(
                    self.catalogue.library_root().is_none(),
                    "library_move_restart_required"
                );
                let mut root = self.root(args["rootId"].as_str())?;
                let id = io::required_string(&root, "id")?.to_owned();
                let source = self.root_path(&root)?;
                let destination = Path::new(io::required_string(args, "path")?);
                let payload = self.payload(Some(&id), None, false)?;
                let receipt = portable::copy_verified_root(&source, destination, &payload, cancel)?;
                io::check_cancel(cancel)?;
                root["path"] = json!(receipt.destination_root);
                root["freeBytes"] = json!(fs2::available_space(&receipt.destination_root)?);
                root["usedBytes"] =
                    json!(receipt.inventory.iter().map(|e| e.size_bytes).sum::<u64>());
                root["previousMigration"] = serde_json::to_value(receipt)?;
                root["status"] = json!("online");
                self.catalogue.put("root", &id, root.clone())?;
                Ok(root)
            }
            "cleanup_old_root" => {
                let mut root = self.root(args["rootId"].as_str())?;
                let receipt: portable::MigrationReceipt =
                    serde_json::from_value(root["previousMigration"].clone())
                        .map_err(|_| anyhow::anyhow!("migration_receipt_missing"))?;
                ensure!(
                    Path::new(io::required_string(&root, "path")?) == receipt.destination_root,
                    "root_mapping_changed"
                );
                let removed = portable::cleanup_old_root(&receipt, cancel)?;
                root.as_object_mut().unwrap().remove("previousMigration");
                self.catalogue
                    .put("root", io::required_string(&root, "id")?, root.clone())?;
                Ok(json!({"removedFiles":removed}))
            }
            "remap_root" => {
                if self.catalogue.library_root().is_some() {
                    let root = self.root(args["rootId"].as_str())?;
                    let selected =
                        checked_library_root(Path::new(io::required_string(args, "path")?))?;
                    ensure!(selected == self.root_path(&root)?, "use_remap_or_migrate");
                    return Ok(root);
                }
                let mut root = self.root(args["rootId"].as_str())?;
                let id = io::required_string(&root, "id")?.to_owned();
                let destination =
                    io::absolute_dir(Path::new(io::required_string(args, "path")?), false)?;
                let mut recovered = Vec::new();
                for mut artifact in self
                    .catalogue
                    .list("artifact")?
                    .into_iter()
                    .filter(|a| a["rootId"] == id)
                {
                    if restored_artifact_state(&artifact).is_some() {
                        let original_available =
                            self.verify_restored_artifact_at(&artifact, &destination, cancel)?;
                        recover_artifact_state(&mut artifact, original_available);
                        recovered.push((
                            "artifact".into(),
                            io::required_string(&artifact, "id")?.to_owned(),
                            artifact,
                        ));
                    } else if artifact["originalAvailable"] == true
                        || artifact["extractedPath"].is_string()
                    {
                        self.verify_artifact_at(&artifact, &destination, cancel)?;
                    }
                }
                root["path"] = json!(destination);
                root["status"] = json!("online");
                // Failed validation above changes neither mapping nor availability.
                recovered.push(("root".into(), id, root.clone()));
                self.catalogue.put_many(recovered)?;
                Ok(root)
            }
            "verify_root" => {
                let mut root = self.root(args["rootId"].as_str())?;
                let id = io::required_string(&root, "id")?.to_owned();
                let path = match self.root_path(&root) {
                    Ok(path) => path,
                    Err(_) => {
                        root["status"] = json!("offline");
                        self.catalogue.put("root", &id, root)?;
                        return Ok(json!({"status":"offline"}));
                    }
                };
                let mut verified = 0;
                let mut missing = 0;
                for mut artifact in self
                    .catalogue
                    .list("artifact")?
                    .into_iter()
                    .filter(|a| a["rootId"] == id)
                {
                    io::check_cancel(cancel)?;
                    if restored_artifact_state(&artifact).is_some() {
                        match self.verify_restored_artifact_at(&artifact, &path, cancel) {
                            Ok(original_available) => {
                                recover_artifact_state(&mut artifact, original_available);
                                self.catalogue.put(
                                    "artifact",
                                    io::required_string(&artifact, "id")?,
                                    artifact.clone(),
                                )?;
                                verified += 1;
                            }
                            Err(error) => {
                                io::check_cancel(cancel)?;
                                if !path.is_dir() {
                                    root["status"] = json!("offline");
                                    self.catalogue.put("root", &id, root)?;
                                    return Ok(json!({"status":"offline"}));
                                }
                                if io::stable_error(&error) == "permission_denied" {
                                    return Err(error);
                                }
                                // Metadata-only claims are still unknown when bytes
                                // are absent/different; no completed local fact existed.
                                missing += 1;
                            }
                        }
                        continue;
                    }
                    if !matches!(
                        artifact["status"].as_str(),
                        Some("ready" | "downloaded" | "extractionFailed" | "missing")
                    ) {
                        continue;
                    }
                    match self.verify_artifact_at(&artifact, &path, cancel) {
                        Ok(()) => {
                            verified += 1;
                            if artifact["status"] == "missing" {
                                artifact["status"] = artifact["previousStatus"].clone();
                            }
                        }
                        Err(e) => {
                            io::check_cancel(cancel)?;
                            if !path.is_dir() {
                                root["status"] = json!("offline");
                                self.catalogue.put("root", &id, root)?;
                                return Ok(json!({"status":"offline"}));
                            }
                            if io::stable_error(&e) == "permission_denied" {
                                return Err(e);
                            }
                            missing += 1;
                            if artifact["status"] != "missing" {
                                artifact["previousStatus"] = artifact["status"].clone();
                            }
                            artifact["status"] = json!("missing");
                            artifact["errorCode"] = json!("file_integrity_failed");
                        }
                    }
                    self.catalogue.put(
                        "artifact",
                        io::required_string(&artifact, "id")?,
                        artifact.clone(),
                    )?;
                }
                root["status"] = json!("online");
                root["freeBytes"] = json!(fs2::available_space(&path)?);
                root["totalBytes"] = json!(fs2::total_space(&path)?);
                self.catalogue.put("root", &id, root)?;
                Ok(json!({"status":"online","verified":verified,"missing":missing}))
            }
            "restore_backup" => self.restore(args, cancel, None),
            _ => bail!("unsupported_storage_action"),
        }
    }
    fn verify_artifact_at(&self, a: &Value, root: &Path, cancel: &AtomicBool) -> Result<()> {
        if a["originalAvailable"] == true {
            io::check_cancel(cancel)?;
            io::verify(
                &io::safe_join(root, io::required_string(a, "relativePath")?)?,
                io::required_string(a, "hash")?,
                a["sizeBytes"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?,
            )?;
        }
        if a["extractedPath"].is_string() {
            self.verify_extracted_at(a, root, cancel)?;
        } else {
            ensure!(a["originalAvailable"] == true, "file_missing");
        }
        Ok(())
    }
    fn verify_restored_artifact_at(
        &self,
        artifact: &Value,
        root: &Path,
        cancel: &AtomicBool,
    ) -> Result<bool> {
        ensure!(
            restored_artifact_state(artifact).is_some(),
            "file_not_usable"
        );
        io::check_cancel(cancel)?;
        let extracted = artifact["extractedPath"].is_string();
        if extracted {
            // Metadata restoration is a claim until every recorded member's
            // path, kind, size and checksum have been verified on this device.
            self.verify_extracted_at(artifact, root, cancel)?;
        }
        let original = io::safe_join(root, io::required_string(artifact, "relativePath")?)?;
        let original_available = original.try_exists()?;
        if original_available {
            io::verify(
                &original,
                io::required_string(artifact, "hash")?,
                artifact["sizeBytes"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?,
            )?;
        } else {
            // A verified extraction remains usable after its original archive
            // was intentionally removed; availability must not be invented.
            ensure!(extracted, "file_missing");
        }
        io::check_cancel(cancel)?;
        Ok(original_available)
    }
    pub(super) fn verify_extracted_at(
        &self,
        a: &Value,
        root: &Path,
        cancel: &AtomicBool,
    ) -> Result<PathBuf> {
        let base = io::safe_join(root, io::required_string(a, "extractedPath")?)?;
        ensure!(base.is_dir(), "file_missing");
        let inventory = a["extractedInventory"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?;
        ensure!(!inventory.is_empty(), "file_integrity_failed");
        for entry in inventory {
            io::check_cancel(cancel)?;
            let path = io::safe_join(&base, io::required_string(entry, "relativePath")?)?;
            if entry["isDirectory"] == true {
                ensure!(path.is_dir(), "file_missing");
            } else {
                io::verify(
                    &path,
                    io::required_string(entry, "hash")?,
                    entry["sizeBytes"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?,
                )?;
            }
        }
        Ok(base)
    }
    pub(super) fn reconcile_archive_cleanup(&self) -> Result<()> {
        for root in self.catalogue.list("root")? {
            if let Ok(path) = self.root_path(&root) {
                if let Ok(staging) = io::safe_join(&path, ".nna-staging") {
                    // Optional leftover cleanup cannot block ordinary library
                    // use; unavailable/locked workspaces retain their marker.
                    let _ = archive::recover_workspaces(&staging);
                }
            }
        }
        let mut changes = Vec::new();
        for mut artifact in self.catalogue.list("artifact")? {
            if artifact["status"] == "downloading" && artifact["originalAvailable"] != true {
                let root = self.root(Some(io::required_string(&artifact, "rootId")?))?;
                if let Ok(path) = self.root_path(&root) {
                    let partial = io::safe_join(
                        &path,
                        &format!(
                            ".nna-partials/{}.part",
                            io::required_string(&artifact, "id")?
                        ),
                    )?;
                    let _empty_partials =
                        io::EmptyDirectoryGuard(partial.parent().unwrap().to_path_buf());
                    if partial.exists() {
                        io::ensure_regular(&partial)?;
                        std::fs::remove_file(partial)?;
                    }
                    artifact["status"] = json!("notDownloaded");
                    artifact["errorCode"] = json!("interrupted");
                    changes.push((
                        "artifact".into(),
                        io::required_string(&artifact, "id")?.to_owned(),
                        artifact,
                    ));
                }
                continue;
            }
            if artifact["originalDeletionPending"] != true {
                continue;
            }
            let root = self.root(Some(io::required_string(&artifact, "rootId")?))?;
            let Ok(path) = self.root_path(&root) else {
                continue;
            };
            let original = io::safe_join(&path, io::required_string(&artifact, "relativePath")?)?;
            if !original.exists() {
                let mut candidate = artifact.clone();
                candidate["originalAvailable"] = json!(false);
                if self
                    .verify_artifact_at(&candidate, &path, &AtomicBool::new(false))
                    .is_err()
                {
                    continue;
                }
                artifact["originalAvailable"] = json!(false);
            }
            artifact
                .as_object_mut()
                .unwrap()
                .remove("originalDeletionPending");
            changes.push((
                "artifact".into(),
                io::required_string(&artifact, "id")?.to_owned(),
                artifact,
            ));
        }
        if !changes.is_empty() {
            self.catalogue.put_many(changes)?;
        }
        Ok(())
    }
    pub(super) fn open_artifact(&self, action: &str, args: &Value) -> Result<Value> {
        let path = self.resolve_open_target(action, args)?;
        io::open_path(&path)?;
        Ok(json!({"opened":true}))
    }
    /// Resolve a completed target without launching a shell; regression tests are inert.
    pub(super) fn resolve_open_target(&self, action: &str, args: &Value) -> Result<PathBuf> {
        if action == "open_root_folder" {
            let root = self.root(args["rootId"].as_str())?;
            let path = self.root_path(&root)?;
            archive::validate_no_links(&path)?;
            return Ok(path);
        }
        if action == "open_product_folder" {
            let product = io::required_string(args, "productId")?;
            for artifact in self.catalogue.related("artifact", product)? {
                if !matches!(
                    artifact["status"].as_str(),
                    Some("ready" | "downloaded" | "extractionFailed")
                ) {
                    continue;
                }
                let root = self.root(Some(io::required_string(&artifact, "rootId")?))?;
                let Ok(root) = self.root_path(&root) else {
                    continue;
                };
                if self.available_file_target(&artifact, &root).is_err() {
                    continue;
                }
                if let Ok(path) = self.product_directory(&artifact, &root) {
                    if path.is_dir() {
                        return Ok(path);
                    }
                }
            }
            bail!("file_missing")
        }
        ensure!(action == "open_file", "unsupported_storage_action");
        let id = io::required_string(args, "fileId")?;
        let remote = self
            .catalogue
            .get("remote_file", id)?
            .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
        ensure!(
            remote["revision"]
                .as_str()
                .is_some_and(|r| !r.is_empty() && r != "unknown"),
            "revision_unknown"
        );
        let artifact = self
            .catalogue
            .get("artifact", &artifact_id(&remote))?
            .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
        let root = self.root(Some(io::required_string(&artifact, "rootId")?))?;
        let root = self.root_path(&root)?;
        self.available_file_target(&artifact, &root)
    }
    /// Opening is a path/existence check of a durably completed artifact.
    /// Full content verification belongs to verify_root, migration and restore.
    fn available_file_target(&self, artifact: &Value, root: &Path) -> Result<PathBuf> {
        ensure!(
            matches!(
                artifact["status"].as_str(),
                Some("ready" | "downloaded" | "extractionFailed")
            ),
            "file_not_usable"
        );
        let path = io::safe_join(&root, io::required_string(&artifact, "relativePath")?)?;
        // Archive filename actions prefer completed extraction, independently of
        // whether an original was retained or has since become damaged.
        if artifact["status"] == "ready" && archive::is_supported_archive(&path) {
            match self.extracted_target(artifact, root) {
                Ok(extracted) => return Ok(extracted),
                Err(error) if artifact["originalAvailable"] != true || !path.exists() => {
                    return Err(error);
                }
                Err(_) => {} // A retained original can still provide the open target.
            }
        }
        if artifact["originalAvailable"] == true && path.exists() {
            io::ensure_regular(&path)?;
            ensure!(
                Some(std::fs::metadata(&path)?.len()) == artifact["sizeBytes"].as_u64()
                    && artifact["hash"].as_str().is_some_and(|h| h.len() == 64),
                "file_integrity_failed"
            );
            return Ok(path);
        }
        bail!("original_unavailable")
    }
    fn extracted_target(&self, artifact: &Value, root: &Path) -> Result<PathBuf> {
        ensure!(
            artifact["extractedInventory"]
                .as_array()
                .is_some_and(|i| !i.is_empty()),
            "file_integrity_failed"
        );
        let base = io::safe_join(root, io::required_string(artifact, "extractedPath")?)?;
        ensure!(base.is_dir(), "file_missing");
        Ok(base)
    }
    fn product_directory(&self, artifact: &Value, root: &Path) -> Result<PathBuf> {
        let directory = artifact["productDirectory"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("product_folder_unavailable"))?;
        let base = io::safe_join(root, directory)?;
        ensure!(directory.starts_with("assets/"), "archive_unsafe_path");
        ensure!(
            ["relativePath", "extractedPath"].iter().any(|field| {
                artifact[*field].as_str().is_some_and(|relative| {
                    relative
                        .replace('\\', "/")
                        .starts_with(&format!("{}/", directory.replace('\\', "/")))
                })
            }),
            "product_folder_unavailable"
        );
        Ok(base)
    }
    pub(super) fn product_availability(&self, args: &Value) -> Result<Value> {
        let product = io::required_string(args, "productId")?;
        let mut folder = false;
        let mut openable = Vec::new();
        let mut online = false;
        let mut offline = false;
        let remote: HashMap<_, _> = self
            .catalogue
            .related("remote_file", product)?
            .into_iter()
            .map(|f| (f["id"].as_str().unwrap_or("").to_owned(), f))
            .collect();
        for a in self.catalogue.related("artifact", product)? {
            if !matches!(
                a["status"].as_str(),
                Some("ready" | "downloaded" | "extractionFailed")
            ) {
                continue;
            }
            let root = self.root(Some(io::required_string(&a, "rootId")?))?;
            let root = match self.root_path(&root) {
                Ok(p) => {
                    online = true;
                    p
                }
                Err(_) => {
                    offline = true;
                    continue;
                }
            };
            let original_path = a["relativePath"]
                .as_str()
                .and_then(|p| io::safe_join(&root, p).ok());
            let original = a["originalAvailable"] == true
                && original_path
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .is_some_and(|m| m.is_file() && Some(m.len()) == a["sizeBytes"].as_u64())
                && a["hash"].as_str().is_some_and(|h| h.len() == 64);
            let extracted = a["status"] == "ready"
                && original_path
                    .as_ref()
                    .is_some_and(|p| archive::is_supported_archive(p))
                && self.extracted_target(&a, &root).is_ok();
            if original || extracted {
                folder |= self.product_directory(&a, &root).is_ok_and(|p| p.is_dir());
            }
            if original || extracted {
                if let Some(file) = a["fileId"].as_str().and_then(|id| remote.get(id)) {
                    if file["revision"]
                        .as_str()
                        .is_some_and(|r| !r.is_empty() && r != "unknown")
                        && artifact_id(file) == a["id"].as_str().unwrap_or("")
                    {
                        openable.push(file["id"].clone());
                    }
                }
            }
        }
        Ok(
            json!({"folderAvailable":folder,"openableFileIds":openable,"rootStatus":if online{"online"}else if offline{"offline"}else{"unconfigured"}}),
        )
    }
    pub(super) fn restore(
        &self,
        args: &Value,
        cancel: &AtomicBool,
        active_restore_job_id: Option<&str>,
    ) -> Result<Value> {
        let backup = Path::new(io::required_string(args, "path")?);
        let destination = if let Some(path) = args["destinationPath"].as_str() {
            PathBuf::from(path)
        } else {
            self.root_path(&self.root(None)?)?
                .join(format!("restored-{}", uuid::Uuid::new_v4()))
        };
        if let Some(library) = self.catalogue.library_root() {
            ensure!(
                destination.starts_with(library) && destination != library,
                "invalid_path"
            );
        }
        let restored = portable::restore_backup(backup, &destination, cancel)?;
        let mut manifest = restored.catalogue;
        let mappings =
            self.install_restored_manifest(&mut manifest, &restored.payload, &destination, cancel)?;
        io::check_cancel(cancel)?;
        if let Some(id) = active_restore_job_id {
            self.catalogue
                .restore_verified_manifest_for_job(manifest, mappings, id.to_owned())?;
        } else {
            self.catalogue
                .restore_verified_manifest(manifest, mappings)?;
        }
        Ok(json!({"restoredFiles":restored.payload.len()}))
    }
    fn install_restored_manifest(
        &self,
        manifest: &mut Value,
        inventory: &[portable::InventoryEntry],
        destination: &Path,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        // Metadata format is versioned by Catalogue. Root/media paths are replaced only after full payload verification.
        ensure!(
            manifest["format"] == "NekoNekoAsset"
                && manifest["version"] == 1
                && manifest["complete"] == true,
            "unsupported_backup_version"
        );
        let mut mappings = serde_json::Map::new();
        for root in manifest["records"]["root"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?
        {
            let id = io::required_string(root, "id")?;
            let path = io::safe_join(
                destination,
                &format!("payload/{}", hex::encode(id.as_bytes())),
            )?;
            std::fs::create_dir_all(&path)?;
            mappings.insert(id.to_owned(), json!(path));
        }
        // A self-consistent ZIP inventory alone is insufficient: every metadata claim must have a matching verified payload.
        let inventory_by_path: HashMap<_, _> = inventory
            .iter()
            .map(|e| ((e.root_id.as_str(), e.relative_path.as_str()), e))
            .collect();
        let require_payload =
            |root: &str, relative: &str, hash: &str, size: u64, directory: bool| -> Result<()> {
                ensure!(
                    inventory_by_path
                        .get(&(root, relative))
                        .is_some_and(|e| e.hash == hash
                            && e.size_bytes == size
                            && e.is_directory == directory),
                    "backup_payload_missing"
                );
                Ok(())
            };
        for artifact in manifest["records"]["artifact"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?
        {
            let root = io::required_string(artifact, "rootId")?;
            ensure!(mappings.contains_key(root), "backup_payload_missing");
            if artifact["originalAvailable"] == true {
                require_payload(
                    root,
                    io::required_string(artifact, "relativePath")?,
                    io::required_string(artifact, "hash")?,
                    artifact["sizeBytes"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?,
                    false,
                )?;
            }
            if let Some(base) = artifact["extractedPath"].as_str() {
                for entry in artifact["extractedInventory"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?
                {
                    require_payload(
                        root,
                        &format!("{}/{}", base, io::required_string(entry, "relativePath")?),
                        entry["hash"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?,
                        entry["sizeBytes"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?,
                        entry["isDirectory"] == true,
                    )?;
                }
            } else if artifact["status"] == "ready" {
                ensure!(
                    artifact["originalAvailable"] == true,
                    "backup_payload_missing"
                );
            }
            if artifact["originalAvailable"] == true
                || artifact["extractedPath"].is_string()
                || matches!(
                    artifact["status"].as_str(),
                    Some("ready" | "downloaded" | "extractionFailed")
                )
            {
                self.verify_artifact_at(
                    artifact,
                    Path::new(
                        mappings[root]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?,
                    ),
                    cancel,
                )?;
            }
        }
        // Keep relative image identities verbatim, including references in opaque CRDT history.
        // Existing equal content is reused; a conflicting local image never gets overwritten.
        let mut media_paths = HashSet::new();
        for entry in inventory.iter().filter(|e| e.root_id == "__media") {
            io::check_cancel(cancel)?;
            ensure!(
                entry.relative_path.starts_with("media/") && !entry.is_directory,
                "backup_payload_missing"
            );
            let source = io::safe_join(destination, &entry.archive_path)?;
            let target = io::safe_join(&self.catalogue.data_dir(), &entry.relative_path)?;
            std::fs::create_dir_all(target.parent().unwrap())?;
            if target.exists() {
                io::verify(&target, &entry.hash, entry.size_bytes)
                    .map_err(|_| anyhow::anyhow!("media_path_collision"))?;
            } else {
                io::copy_new_verified(&source, &target, &entry.hash, entry.size_bytes, cancel)?;
            }
            media_paths.insert(entry.relative_path.clone());
        }
        for path in manifest["media"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?
        {
            ensure!(
                path.as_str().is_some_and(|p| media_paths.contains(p)),
                "backup_payload_missing"
            );
        }
        for overlay in manifest["records"]["overlay"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("backup_payload_missing"))?
        {
            if let Some(path) = overlay["thumbnailPath"].as_str() {
                ensure!(media_paths.contains(path), "backup_payload_missing");
            }
        }
        for artifact in manifest["records"]["artifact"].as_array_mut().unwrap() {
            if matches!(
                artifact["status"].as_str(),
                Some("queued" | "downloading" | "extracting")
            ) {
                artifact["status"] = json!(if artifact["originalAvailable"] == true {
                    "downloaded"
                } else {
                    "notDownloaded"
                });
            }
        }
        Ok(Value::Object(mappings))
    }
}
