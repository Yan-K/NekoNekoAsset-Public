use super::*;

fn retryable_download_resolution(error: &anyhow::Error) -> bool {
    matches!(
        io::stable_error(error),
        "network_failed" | "network_timeout" | "download_resolver_failed" | "link_expired"
    )
}

fn download_retry_delay(value: Option<&str>, attempt: u32, now: i64) -> Option<Duration> {
    let seconds = value
        .and_then(|value| {
            value.parse::<u64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .map(|at| at.timestamp().saturating_sub(now).max(0) as u64)
            })
        })
        .unwrap_or(1 << attempt)
        .max(1);
    (seconds <= 120).then(|| Duration::from_secs(seconds))
}

fn expected_download_size(
    provider: crate::stores::StoreProvider,
    response_length: Option<u64>,
    ticket_length: Option<u64>,
    saved_length: Option<u64>,
) -> Result<Option<u64>> {
    if provider == crate::stores::StoreProvider::Booth {
        // Historical listing size is not a current content validator. A chunked
        // transfer still binds actual streamed hash/size before publication.
        return Ok(response_length.or(ticket_length));
    }
    if let (Some(response), Some(verified)) = (response_length, ticket_length) {
        ensure!(response == verified, "file_integrity_failed");
    }
    Ok(ticket_length.or(response_length).or(saved_length))
}

fn bind_revision(
    file: &Value,
    current: &Value,
    response_revision: Option<String>,
    hash: &str,
) -> Result<(String, String)> {
    for observed in [file, current] {
        if observed["revisionBasis"] != "contentHash"
            && observed["requiresContentVerification"] != true
        {
            if let Some(expected) = observed["revision"]
                .as_str()
                .and_then(|r| r.strip_prefix("sha256:"))
            {
                ensure!(expected == hash, "file_integrity_failed");
            }
        }
    }
    let source = file["revision"].as_str().filter(|r| {
        !r.is_empty()
            && *r != "unknown"
            && file["revisionBasis"] == "source"
            && file["requiresContentVerification"] != true
    });
    Ok(if let Some(revision) = source {
        (revision.to_owned(), "source".into())
    } else if let Some(revision) = response_revision {
        (revision, "etag".into())
    } else {
        (format!("sha256:{hash}"), "contentHash".into())
    })
}
pub(super) fn bind_downloaded_file(
    mut file: Value,
    revision: String,
    basis: String,
    size: u64,
) -> Value {
    file["revision"] = json!(revision);
    file["revisionBasis"] = json!(basis);
    file["requiresContentVerification"] = json!(false);
    file["sizeBytes"] = json!(size);
    file
}
fn same_remote_identity(a: &Value, b: &Value) -> bool {
    [
        "id",
        "accountId",
        "sourceFileId",
        "variantId",
        "productId",
        "name",
        "inventoryId",
        "sourceRelativePath",
    ]
    .iter()
    .all(|key| a[*key] == b[*key])
}

// Jinxxy exposes ordinary leaves as relative filenames in one inventory tree.
// Jinxxy leaves share a product source tree; BOOTH/Gumroad use file directories.
fn source_tree_relative(
    file: &Value,
    job_id: &str,
    product_directory: &str,
) -> Result<Option<String>> {
    if file["provider"] != "jinxxy" || file["sourceRelativePath"].is_null() {
        return Ok(None);
    }
    let path = archive::validate_relative_path(io::required_string(file, "sourceRelativePath")?)?;
    let name = io::required_string(file, "name")?;
    ensure!(
        path.file_name() == Some(std::ffi::OsStr::new(name)),
        "archive_unsafe_path"
    );
    let account = io::required_string(file, "accountId")?;
    let inventory = io::required_string(file, "inventoryId")?;
    let group = io::key(&serde_json::to_string(&json!([account, inventory]))?);
    Ok(Some(format!(
        "{}/source-{}-{}/{}",
        product_directory,
        &io::key(job_id)[..12],
        &group[..12],
        path.to_str()
            .ok_or_else(|| anyhow::anyhow!("archive_unsafe_path"))?
            .replace('\\', "/")
    )))
}

fn validate_source_tree_paths(files: &[Value]) -> Result<()> {
    let mut paths = HashSet::new();
    for file in files {
        if let Some(path) = source_tree_relative(file, "validation", "NonVRChat")? {
            ensure!(paths.insert(path.to_lowercase()), "archive_unsafe_path");
        }
    }
    for path in &paths {
        let mut parent = path.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            ensure!(!paths.contains(prefix), "archive_unsafe_path");
            parent = prefix;
        }
    }
    Ok(())
}

fn publish_source_tree_part(
    root: &Path,
    relative: &str,
    part: &Path,
    hash: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    io::check_cancel(cancel)?;
    let target = io::safe_join(root, relative)?;
    if target.try_exists()? {
        io::verify(&target, hash, size)?;
        std::fs::remove_file(part)?;
        return Ok(());
    }
    std::fs::create_dir_all(target.parent().unwrap())?;
    let target = io::safe_join(root, relative)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let existing: Vec<u16> = part.as_os_str().encode_wide().chain(Some(0)).collect();
        let new: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // Same-root staging permits a same-volume rename. Flags 0 never replace
        // an existing target and work without hard links or another payload copy.
        if unsafe { MoveFileExW(existing.as_ptr(), new.as_ptr(), 0) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(not(windows))]
    {
        // Exclusive publication protects an existing sibling/retry target.
        if std::fs::hard_link(part, &target).is_err() {
            io::copy_new_verified(part, &target, hash, size, cancel)?;
        }
        std::fs::remove_file(part)?;
    }
    Ok(())
}

fn retain_source_tree_copy(
    root: &Path,
    old_relative: &str,
    relative: &str,
    hash: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    retain_source_tree_copy_between(root, old_relative, root, relative, hash, size, cancel)
}
fn retain_source_tree_copy_between(
    source_root: &Path,
    old_relative: &str,
    root: &Path,
    relative: &str,
    hash: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    let part = io::safe_join(
        root,
        &format!(".nna-partials/source-copy-{}.part", io::key(relative)),
    )?;
    let _partial = io::PartialGuard::new(part.clone());
    let target = io::safe_join(root, relative)?;
    if target.try_exists()? {
        return io::verify(&target, hash, size);
    }
    io::check_cancel(cancel)?;
    let source = io::safe_join(source_root, old_relative)?;
    io::space(root, size)?;
    std::fs::create_dir_all(part.parent().unwrap())?;
    if part.try_exists()? {
        io::ensure_regular(&part)?;
        std::fs::remove_file(&part)?;
    }
    io::copy_new_verified(&source, &part, hash, size, cancel)?;
    publish_source_tree_part(root, relative, &part, hash, size, cancel)
}

impl StorageService {
    pub(super) fn commit_download_records(
        &self,
        updates: Vec<(String, String, Value)>,
        expected: Vec<(String, String, Option<Value>)>,
    ) -> Result<()> {
        ensure!(
            self.catalogue.put_many_if_unchanged(updates, expected)?,
            "source_changed"
        );
        Ok(())
    }

    pub(super) fn delete_pending_download_record(&self, file_id: &str) -> Result<()> {
        let id = format!("pending-{}", io::key(file_id));
        self.catalogue.delete("artifact", &id)
    }

    async fn prepare_selected_source_siblings(
        &self,
        root: &Value,
        root_path: &Path,
        current: &Value,
        files: &[Value],
        job_id: &str,
        directory: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<(
        Vec<(String, String, Value)>,
        Vec<(String, String, Option<Value>)>,
    )> {
        let mut updates = Vec::new();
        let mut expected = Vec::new();
        for sibling in files.iter().filter(|file| {
            file["id"] != current["id"]
                && file["accountId"] == current["accountId"]
                && file["inventoryId"] == current["inventoryId"]
        }) {
            let Some(relative) = source_tree_relative(sibling, job_id, directory)? else {
                continue;
            };
            let id = artifact_id(sibling);
            let Some(prior) = self
                .catalogue
                .get("artifact", &id)?
                .filter(|a| a["status"] == "ready")
            else {
                continue;
            };
            ensure!(prior["originalAvailable"] == true, "source_tree_incomplete");
            let source_root = self.root_path(&self.root(prior["rootId"].as_str())?)?;
            let old_relative = io::required_string(&prior, "relativePath")?.to_owned();
            let hash = io::required_string(&prior, "hash")?.to_owned();
            let size = prior["sizeBytes"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("source_tree_incomplete"))?;
            let target_root = root_path.to_owned();
            let copy_relative = relative.clone();
            let copy_cancel = cancel.clone();
            let copy = tokio::task::spawn_blocking(move || {
                retain_source_tree_copy_between(
                    &source_root,
                    &old_relative,
                    &target_root,
                    &copy_relative,
                    &hash,
                    size,
                    &copy_cancel,
                )
            })
            .await?;
            if copy.is_err() {
                io::check_cancel(cancel)?;
                bail!("source_tree_incomplete");
            }
            io::check_cancel(cancel)?;
            expected.push((
                "remote_file".into(),
                io::required_string(sibling, "id")?.to_owned(),
                Some(sibling.clone()),
            ));
            expected.push(("artifact".into(), id.clone(), Some(prior.clone())));
            let mut historical = prior.clone();
            let historical_id = format!("historical-{}", uuid::Uuid::new_v4());
            historical["id"] = json!(historical_id);
            historical["sourceRevision"] = historical["revision"].clone();
            historical["revision"] = json!(historical_id);
            updates.push(("artifact".into(), historical_id, historical));
            let mut next = prior;
            next["relativePath"] = json!(relative);
            next["rootId"] = root["id"].clone();
            next["productDirectory"] = json!(directory);
            updates.push(("artifact".into(), id, next));
        }
        Ok((updates, expected))
    }
    async fn verified_single_source_content(
        &self,
        root: &Value,
        root_path: &Path,
        current: &Value,
        files: &[Value],
        complete: bool,
        hash: &str,
        size: u64,
    ) -> Result<Option<Value>> {
        if !complete
            || files.len() != 1
            || current["provider"] != "jinxxy"
            || !same_remote_identity(current, &files[0])
        {
            return Ok(None);
        }
        let Some(source_relative) = current["sourceRelativePath"].as_str() else {
            return Ok(None);
        };
        let Some(prior) = self.catalogue.get("artifact", &artifact_id(current))? else {
            return Ok(None);
        };
        let suffix = format!("/{}", source_relative.replace('\\', "/"));
        if prior["status"] != "ready"
            || prior["rootId"] != root["id"]
            || prior["revision"] != current["revision"]
            || prior["hash"] != hash
            || prior["sizeBytes"] != size
            || !prior["relativePath"]
                .as_str()
                .is_some_and(|path| path.replace('\\', "/").ends_with(&suffix))
            || !self.artifact_verified(&prior, root_path).await?
        {
            return Ok(None);
        }
        Ok(Some(prior))
    }

    async fn verified_source_manifest(
        &self,
        root: &Value,
        root_path: &Path,
        files: &[Value],
        complete: bool,
        cancel: &AtomicBool,
    ) -> Result<bool> {
        if !complete
            || files.is_empty()
            || files.iter().any(|file| file["provider"] != "jinxxy")
            || !files
                .iter()
                .any(|file| file["sourceRelativePath"].is_string())
        {
            return Ok(false);
        }
        let mut artifacts = Vec::new();
        let mut prefixes = HashMap::new();
        // Check all metadata before reading bytes: a changed/new leaf needs the
        // existing common-tree rebuild, not an extra hash pass over its siblings.
        for file in files {
            if file["requiresContentVerification"] == true
                || !file["revision"]
                    .as_str()
                    .is_some_and(|revision| !revision.is_empty() && revision != "unknown")
            {
                return Ok(false);
            }
            let Some(artifact) = self.catalogue.get("artifact", &artifact_id(file))? else {
                return Ok(false);
            };
            if artifact["status"] != "ready"
                || artifact["rootId"] != root["id"]
                || artifact["revision"] != file["revision"]
            {
                return Ok(false);
            }
            if let Some(source_relative) = file["sourceRelativePath"].as_str() {
                let source = archive::validate_relative_path(source_relative)?;
                let suffix = format!(
                    "/{}",
                    source
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("archive_unsafe_path"))?
                        .replace('\\', "/")
                );
                let stored = io::required_string(&artifact, "relativePath")?.replace('\\', "/");
                let Some(prefix) = stored.strip_suffix(&suffix) else {
                    return Ok(false);
                };
                let group = (
                    io::required_string(file, "accountId")?.to_owned(),
                    io::required_string(file, "inventoryId")?.to_owned(),
                );
                if prefixes
                    .get(&group)
                    .is_some_and(|existing| existing != prefix)
                {
                    return Ok(false);
                }
                prefixes.insert(group, prefix.to_owned());
            }
            artifacts.push(artifact);
        }
        for artifact in artifacts {
            io::check_cancel(cancel)?;
            if !self.artifact_verified(&artifact, root_path).await? {
                return Ok(false);
            }
        }
        io::check_cancel(cancel)?;
        Ok(true)
    }

    pub(super) async fn run_download(
        &self,
        app: &tauri::AppHandle,
        job: &mut Value,
        cancel: &Arc<AtomicBool>,
    ) -> Result<()> {
        io::check_cancel(cancel)?;
        let root = self.root(job["rootId"].as_str())?;
        let root_path = self.root_path(&root)?;
        let product = io::required_string(job, "productId")?.to_owned();
        let (files, complete) = self.remote_files(&product)?;
        let files = if job["automatic"] == true {
            self.enabled_files(files)?
        } else {
            files
        };
        let files = selected_download_files(files, &job["selectedFileIds"])?;
        let attempted_files = files.clone();
        ensure!(!files.is_empty(), "manifest_incomplete");
        validate_source_tree_paths(&files)?;
        job["fileIds"] = json!(files.iter().map(|f| f["id"].clone()).collect::<Vec<_>>());
        self.catalogue
            .put("job", io::required_string(job, "id")?, job.clone())?;
        if job["redownload"] != true
            && job["selectedFileIds"].is_null()
            && self
                .verified_source_manifest(&root, &root_path, &files, complete, cancel)
                .await?
        {
            let (current, complete_after) = self.remote_files(&product)?;
            let current = if job["automatic"] == true {
                self.enabled_files(current)?
            } else {
                current
            };
            if complete_after && fingerprint(&current) == fingerprint(&files) {
                job["manifestFingerprint"] = json!(fingerprint(&current));
                job["progress"] = json!(100.0);
                return Ok(());
            }
        }
        let provider = self
            .catalogue
            .get("product", &product)?
            .and_then(|product| {
                product["store"]
                    .as_str()
                    .or_else(|| product["provider"].as_str())
                    .and_then(|store| crate::stores::StoreProvider::parse(store).ok())
            });
        let job_id = io::required_string(job, "id")?.to_owned();
        let redownload = job["redownload"] == true;
        let selected = job["selectedFileIds"].is_array();
        let mut waiting = files.iter().cloned();
        let mut next = waiting.next();
        let mut in_flight = FuturesUnordered::new();
        let mut results = Vec::new();
        while next.is_some() || !in_flight.is_empty() {
            let concurrency = adaptive_download_count(
                &self.preferences()?["downloadConcurrency"],
                provider,
                &self.catalogue.list("job")?,
            );
            // Sync reduces new launches for the same provider. Existing transfers
            // finish normally; each completion rechecks sync and saved preferences.
            while next.is_some() && in_flight.len() < concurrency && !cancel.load(Ordering::Acquire)
            {
                let file = next.take().unwrap();
                let cancel = cancel.clone();
                let root = root.clone();
                let job_id = job_id.clone();
                in_flight.push(async move {
                    let mut result = self
                        .download_one(app, &job_id, &root, &file, &cancel, redownload, selected)
                        .await;
                    if result.is_ok() {
                        result = self
                            .file_progress(app, &job_id, file["id"].as_str().unwrap_or(""), 1.0)
                            .await;
                    }
                    (file, result)
                });
                next = waiting.next();
            }
            if let Some(result) = in_flight.next().await {
                results.push(result);
            } else {
                break;
            }
        }
        io::check_cancel(cancel)?;
        // A failed sibling never discards completed bytes. Extract only after all volumes have arrived.
        let mut error = None;
        job["failures"] = json!({});
        for (file, result) in results {
            if let Err(failure) = result {
                let id = file["id"].as_str().unwrap_or("");
                job["failures"][id] = json!({"fileId":id,"fileName":file["name"],"errorCode":io::stable_error(&failure)});
                if error.is_none() {
                    error = Some(failure);
                }
            }
        }
        if let Some(latest) = self.catalogue.get("job", &job_id)? {
            job["progress"] = latest["progress"].clone();
            job["fileProgress"] = latest["fileProgress"].clone();
        }
        let (all_files, _) = self.remote_files(&product)?;
        let files = if job["automatic"] == true {
            self.enabled_files(all_files.clone())?
        } else {
            all_files.clone()
        };
        let files = selected_download_files(files, &job["selectedFileIds"])?;
        let mut processed = Vec::new();
        for before in &attempted_files {
            let current = files.iter().find(|f| f["id"] == before["id"]);
            let bound = if let Some(current) = current {
                self.catalogue
                    .get("artifact", &artifact_id(current))?
                    .is_some_and(|a| {
                        matches!(
                            a["status"].as_str(),
                            Some("ready" | "downloaded" | "extractionFailed")
                        )
                    })
            } else {
                false
            };
            processed.push(if bound {
                current.unwrap().clone()
            } else {
                before.clone()
            });
        }
        job["manifestFingerprint"] = json!(fingerprint(&processed));
        if processed.len() != files.len() && error.is_none() {
            error = Some(anyhow::anyhow!("manifest_changed"));
        }
        let seven_zip = self.seven_zip(app)?;
        for file in &files {
            io::check_cancel(cancel)?;
            let id = artifact_id(file);
            let Some(mut artifact) = self.catalogue.get("artifact", &id)? else {
                if error.is_none() {
                    error = Some(anyhow::anyhow!("manifest_changed"));
                }
                continue;
            };
            if artifact["status"] == "ready" {
                continue;
            }
            if artifact["errorCode"] == "source_tree_incomplete" {
                continue;
            }
            if !matches!(
                artifact["status"].as_str(),
                Some("downloaded" | "extractionFailed")
            ) {
                continue;
            }
            let path = io::safe_join(&root_path, io::required_string(&artifact, "relativePath")?)?;
            if !archive::is_supported_archive(&path) {
                artifact["status"] = json!("ready");
                self.catalogue.put("artifact", &id, artifact)?;
                continue;
            }
            let result = self
                .extract_for_download(&seven_zip, &root, &mut artifact, &all_files, cancel)
                .await;
            if let Err(e) = result {
                let file_id = file["id"].as_str().unwrap_or("");
                job["failures"][file_id] = json!({"fileId":file_id,"fileName":file["name"],"errorCode":io::stable_error(&e)});
                if error.is_none() {
                    error = Some(e)
                }
            }
            self.changed(app, Some(&job_id));
        }
        if let Some(error) = error {
            return Err(error);
        }
        // Completion describes the known selected work, not source coverage.
        // Catalogue keeps a partial product partial until every required file is Ready.
        job["progress"] = json!(100.0);
        Ok(())
    }
    async fn file_progress(
        &self,
        app: &tauri::AppHandle,
        job_id: &str,
        file_id: &str,
        fraction: f64,
    ) -> Result<()> {
        let _gate = self.queue_gate.lock().await;
        if let Some(mut job) = self.catalogue.get("job", job_id)? {
            job["fileProgress"][file_id] = json!(fraction.clamp(0.0, 1.0));
            let ids = strings(&job["fileIds"]);
            let sum: f64 = ids
                .iter()
                .map(|id| job["fileProgress"][id].as_f64().unwrap_or(0.0))
                .sum();
            job["progress"] = json!(90.0 * sum / ids.len().max(1) as f64);
            job["updatedAt"] = json!(now());
            self.catalogue.put("job", job_id, job)?;
            self.jobs_changed(app, job_id);
        }
        Ok(())
    }
    fn seven_zip(&self, app: &tauri::AppHandle) -> Result<PathBuf> {
        // Test overrides require a debug build and explicitly isolated synthetic data.
        if cfg!(debug_assertions) && std::env::var_os("NNA_DATA_DIR").is_some() {
            if let Some(path) = std::env::var_os("NNA_7ZIP") {
                return Ok(path.into());
            }
        }
        #[cfg(windows)]
        {
            Ok(app.path().resource_dir()?.join("tools/7zip/7z.exe"))
        }
        #[cfg(not(windows))]
        {
            let _ = app;
            Ok(PathBuf::from("7z"))
        }
    }
    pub(super) async fn artifact_verified(&self, artifact: &Value, root: &Path) -> Result<bool> {
        let artifact = artifact.clone();
        let root = root.to_owned();
        tokio::task::spawn_blocking(move || -> Result<bool> {
            if artifact["status"] == "ready" && artifact["extractedPath"].as_str().is_some() {
                let base = io::safe_join(&root, io::required_string(&artifact, "extractedPath")?)?;
                let entries = artifact["extractedInventory"]
                    .as_array()
                    .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?;
                ensure!(!entries.is_empty(), "file_integrity_failed");
                for entry in entries {
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
                return Ok(true);
            }
            let path = io::safe_join(&root, io::required_string(&artifact, "relativePath")?)?;
            io::verify(
                &path,
                io::required_string(&artifact, "hash")?,
                artifact["sizeBytes"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?,
            )?;
            Ok(true)
        })
        .await?
        .or(Ok(false))
    }
    async fn download_one(
        &self,
        app: &tauri::AppHandle,
        job_id: &str,
        root: &Value,
        file: &Value,
        cancel: &Arc<AtomicBool>,
        redownload: bool,
        selected: bool,
    ) -> Result<()> {
        io::check_cancel(cancel)?;
        let root_path = self.root_path(root)?;
        let existing_id = artifact_id(file);
        let existing = self.catalogue.get("artifact", &existing_id)?;
        let has_source_tree = file["provider"] == "jinxxy" && !file["sourceRelativePath"].is_null();
        let known_revision = file["revision"]
            .as_str()
            .is_some_and(|r| !r.is_empty() && r != "unknown");
        if !has_source_tree {
            if let Some(artifact) = existing.as_ref() {
                if !redownload
                    && known_revision
                    && file["requiresContentVerification"] != true
                    && matches!(
                        artifact["status"].as_str(),
                        Some("ready" | "downloaded" | "extractionFailed" | "extracting")
                    )
                    && self.artifact_verified(artifact, &root_path).await?
                {
                    if artifact["status"] == "extracting" {
                        let mut recovered = artifact.clone();
                        recovered["status"] = json!("downloaded");
                        self.catalogue.put("artifact", &existing_id, recovered)?;
                    }
                    return Ok(());
                }
            }
        }
        let id = format!("pending-{}", io::key(io::required_string(file, "id")?));
        let name = io::required_string(file, "name")?;
        let safe_name = archive::validate_relative_path(name)?;
        ensure!(safe_name.components().count() == 1, "archive_unsafe_path");
        let product = io::required_string(file, "productId")?;
        let product_source = self
            .catalogue
            .get("product", product)?
            .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
        // Freeze initial storage category independently of future effective classification edits.
        let prior = self
            .catalogue
            .related("artifact", product)?
            .into_iter()
            .next();
        let inference = crate::taxonomy::infer_category(
            &product_source["sourceCategory"],
            &strings(&product_source["sourceTags"]),
        );
        let initial = prior
            .as_ref()
            .and_then(|a| a["initialCategory"].as_str())
            .unwrap_or_else(|| inference["category"].as_str().unwrap_or("NonVRChat"));
        let category = if [
            "Base",
            "Clothes",
            "Hair",
            "Accessory",
            "Texture",
            "Texture-Material",
            "Motion",
            "Tool",
            "Shader",
            "Gimmick",
            "World",
            "Udon",
            "NonVRChat",
        ]
        .contains(&initial)
        {
            initial
        } else {
            "NonVRChat"
        };
        let product_directory = self.managed_product_directory(product)?;
        let source_tree = source_tree_relative(file, job_id, &product_directory)?;
        let relative = if let Some(path) = &source_tree {
            path.clone()
        } else {
            let directory = format!(
                "{}/{}",
                product_directory,
                io::readable_component(
                    Path::new(name)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("File"),
                    io::required_string(file, "id")?
                )
            );
            format!("{}/{}", io::unused_relative(&root_path, &directory)?, name)
        };
        if let Some(artifact) = existing.filter(|_| has_source_tree) {
            if !redownload
                && known_revision
                && file["requiresContentVerification"] != true
                && matches!(
                    artifact["status"].as_str(),
                    Some("ready" | "downloaded" | "extractionFailed" | "extracting")
                )
                && artifact["originalAvailable"] == true
                && artifact["rootId"] == root["id"]
                && self.artifact_verified(&artifact, &root_path).await?
            {
                let mut recovered = artifact.clone();
                let mut updates = Vec::new();
                let mut recovered_changed = false;
                if source_tree.is_some() {
                    let old_relative = io::required_string(&artifact, "relativePath")?.to_owned();
                    let copy_root = root_path.clone();
                    let copy_relative = relative.clone();
                    let old = old_relative.clone();
                    let hash = io::required_string(&artifact, "hash")?.to_owned();
                    let size = artifact["sizeBytes"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?;
                    let copy_cancel = cancel.clone();
                    tokio::task::spawn_blocking(move || {
                        retain_source_tree_copy(
                            &copy_root,
                            &old,
                            &copy_relative,
                            &hash,
                            size,
                            &copy_cancel,
                        )
                    })
                    .await??;
                    if old_relative != relative {
                        let mut historical = artifact;
                        let historical_id = format!("historical-{}", uuid::Uuid::new_v4());
                        historical["id"] = json!(historical_id);
                        historical["sourceRevision"] = historical["revision"].clone();
                        historical["revision"] = json!(historical_id);
                        updates.push(("artifact".into(), historical_id, historical));
                        recovered["relativePath"] = json!(relative);
                        recovered["productDirectory"] = json!(product_directory);
                        recovered["updatedAt"] = json!(now());
                        recovered_changed = true;
                    }
                }
                if recovered["status"] == "extracting" {
                    recovered["status"] = json!("downloaded");
                    recovered_changed = true;
                }
                if recovered_changed {
                    updates.push(("artifact".into(), existing_id, recovered));
                    self.catalogue.put_many(updates)?;
                    self.changed(app, Some(job_id));
                }
                return Ok(());
            }
        }
        if let Some(mut retained) = self.catalogue.get("artifact", &id)? {
            if retained["originalAvailable"] == true {
                let historical = format!("historical-{}", uuid::Uuid::new_v4());
                retained["id"] = json!(historical);
                retained["sourceRevision"] = retained["revision"].clone();
                retained["revision"] = json!(historical);
                self.catalogue.put("artifact", &historical, retained)?;
            }
        }
        let destination = io::safe_join(&root_path, &relative)?;
        let part = io::safe_join(&root_path, &format!(".nna-partials/{}.part", id))?;
        std::fs::create_dir_all(part.parent().unwrap())?;
        // Only our stable per-file partial is removed; prior completed revisions are never overwritten.
        if part.exists() {
            io::ensure_regular(&part)?;
            std::fs::remove_file(&part)?;
        }
        let _partial = io::PartialGuard::new(part.clone());
        let mut artifact = json!({"id":id,"productId":product,"fileId":file["id"],"rootId":root["id"],"relativePath":relative,"productDirectory":product_directory,"revision":null,"initialCategory":category,"status":"downloading","originalAvailable":false});
        self.catalogue.put("artifact", &id, artifact.clone())?;
        let result = self
            .stream_download(app, job_id, file, &part, &root_path, cancel)
            .await;
        let outcome = match result {
            Ok((hash, size, revision, basis, observed)) => {
                async {
                    if cancel.load(Ordering::Acquire) {
                        let _ = std::fs::remove_file(&part);
                        artifact["status"] = json!("notDownloaded");
                        artifact["errorCode"] = json!("cancelled");
                        self.catalogue.put("artifact", &id, artifact)?;
                        bail!("cancelled")
                    }
                    let current = self
                        .catalogue
                        .get("remote_file", io::required_string(file, "id")?)?
                        .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
                    ensure!(same_remote_identity(&current, &observed), "source_changed");
                    ensure!(
                        current["revision"] == observed["revision"]
                            && !(current["requiresContentVerification"] == true
                                && observed["requiresContentVerification"] != true),
                        "source_changed"
                    );
                    let observed_current = current.clone();
                    let current = bind_downloaded_file(current, revision, basis, size);
                    let final_id = artifact_id(&current);
                    let mut source_expected = vec![(
                        "remote_file".into(),
                        io::required_string(file, "id")?.to_owned(),
                        Some(observed_current),
                    )];
                    if source_tree.is_some() {
                        let mut expected = source_expected.clone();
                        expected.extend([
                            (
                                "root".into(),
                                io::required_string(root, "id")?.to_owned(),
                                Some(root.clone()),
                            ),
                            (
                                "product".into(),
                                product.to_owned(),
                                self.catalogue.get("product", product)?,
                            ),
                        ]);
                        for entitlement in self.catalogue.related("entitlement", product)? {
                            expected.push((
                                "entitlement".into(),
                                io::required_string(&entitlement, "id")?.to_owned(),
                                Some(entitlement),
                            ));
                        }
                        let (files, complete) = self.remote_files(product)?;
                        let prior = self
                            .verified_single_source_content(
                                root,
                                &root_path,
                                &current,
                                if selected {
                                    std::slice::from_ref(&current)
                                } else {
                                    &files
                                },
                                complete || selected,
                                &hash,
                                size,
                            )
                            .await?;
                        if cancel.load(Ordering::Acquire) {
                            let _ = std::fs::remove_file(&part);
                            artifact["status"] = json!("notDownloaded");
                            artifact["errorCode"] = json!("cancelled");
                            self.catalogue.put("artifact", &id, artifact)?;
                            bail!("cancelled")
                        }
                        if let Some(prior) = prior {
                            // The actual GET has established equal content. One leaf has
                            // no sibling layout to rebuild; retain its durable Ready path.
                            expected.push((
                                "artifact".into(),
                                final_id.clone(),
                                Some(prior.clone()),
                            ));
                            self.commit_download_records(
                                vec![
                                    (
                                        "remote_file".into(),
                                        io::required_string(&current, "id")?.to_owned(),
                                        current,
                                    ),
                                    ("artifact".into(), final_id, prior),
                                ],
                                expected,
                            )?;
                            std::fs::remove_file(&part)?;
                            self.delete_pending_download_record(io::required_string(file, "id")?)?;
                            self.changed(app, Some(job_id));
                            return Ok(());
                        }
                        source_expected = expected;
                    }
                    // A source tree always publishes bytes at the declared relative path.
                    // BOOTH and Gumroad retain verified-content sharing within the product.
                    let mut duplicate = None;
                    for existing in if source_tree.is_some() {
                        Vec::new()
                    } else {
                        self.catalogue.related("artifact", product)?
                    } {
                        if existing["hash"] == hash
                            && existing["sizeBytes"] == size
                            && existing["rootId"] == root["id"]
                            && existing["originalAvailable"] == true
                            && existing["relativePath"]
                                .as_str()
                                .and_then(|p| Path::new(p).file_name())
                                .is_some_and(|n| n == std::ffi::OsStr::new(name))
                        {
                            let p = io::safe_join(
                                &root_path,
                                io::required_string(&existing, "relativePath")?,
                            )?;
                            let p2 = p.clone();
                            let h = hash.clone();
                            if tokio::task::spawn_blocking(move || io::verify(&p2, &h, size))
                                .await?
                                .is_ok()
                            {
                                duplicate = Some(existing["relativePath"].clone());
                                break;
                            }
                        }
                    }
                    if source_tree.is_some() {
                        let copy_root = root_path.clone();
                        let copy_relative = relative.clone();
                        let copy_part = part.clone();
                        let copy_hash = hash.clone();
                        let copy_cancel = cancel.clone();
                        tokio::task::spawn_blocking(move || {
                            publish_source_tree_part(
                                &copy_root,
                                &copy_relative,
                                &copy_part,
                                &copy_hash,
                                size,
                                &copy_cancel,
                            )
                        })
                        .await??;
                    } else if let Some(relative) = duplicate {
                        artifact["relativePath"] = relative;
                        std::fs::remove_file(&part)?;
                    } else {
                        std::fs::create_dir_all(destination.parent().unwrap())?;
                        std::fs::rename(&part, &destination)?;
                    }
                    artifact["hash"] = json!(hash);
                    artifact["sizeBytes"] = json!(size);
                    artifact["status"] = json!("downloaded");
                    artifact["originalAvailable"] = json!(true);
                    self.catalogue.put("artifact", &id, artifact.clone())?;
                    let mut sibling_updates = Vec::new();
                    let mut sibling_error = None;
                    if source_tree.is_some() && selected {
                        let (files, _) = self.remote_files(product)?;
                        match self
                            .prepare_selected_source_siblings(
                                root,
                                &root_path,
                                &current,
                                &files,
                                job_id,
                                &product_directory,
                                cancel,
                            )
                            .await
                        {
                            Ok((updates, expected)) => {
                                sibling_updates = updates;
                                source_expected.extend(expected);
                            }
                            Err(error) => {
                                io::check_cancel(cancel)?;
                                sibling_error = Some(error);
                            }
                        }
                    }
                    artifact["id"] = json!(final_id);
                    artifact["revision"] = current["revision"].clone();
                    artifact["hash"] = json!(hash);
                    artifact["sizeBytes"] = json!(size);
                    artifact["status"] = json!("downloaded");
                    artifact["originalAvailable"] = json!(true);
                    artifact["updatedAt"] = json!(now());
                    let mut updates = sibling_updates;
                    let prior = self.catalogue.get("artifact", &final_id)?;
                    source_expected.push(("artifact".into(), final_id.clone(), prior.clone()));
                    if let Some(mut prior) = prior {
                        if sibling_error.is_none()
                            && prior["status"] == "ready"
                            && prior["hash"] == hash
                            && prior["sizeBytes"] == size
                            && self.artifact_verified(&prior, &root_path).await?
                        {
                            artifact["status"] = json!("ready");
                            if let Some(path) = prior.get("extractedPath") {
                                artifact["extractedPath"] = path.clone();
                                artifact["extractedInventory"] =
                                    prior["extractedInventory"].clone();
                            }
                            if source_tree.is_some()
                                && prior["relativePath"] != artifact["relativePath"]
                            {
                                let historical = format!("historical-{}", uuid::Uuid::new_v4());
                                prior["id"] = json!(historical);
                                prior["sourceRevision"] = prior["revision"].clone();
                                prior["revision"] = json!(historical);
                                updates.push(("artifact".into(), historical, prior));
                            }
                        } else {
                            let historical = format!("historical-{}", uuid::Uuid::new_v4());
                            prior["id"] = json!(historical);
                            prior["sourceRevision"] = prior["revision"].clone();
                            prior["revision"] = json!(historical);
                            updates.push(("artifact".into(), historical, prior));
                        }
                    }
                    updates.push((
                        "remote_file".into(),
                        io::required_string(&current, "id")?.to_owned(),
                        current,
                    ));
                    if sibling_error.is_some() {
                        artifact["errorCode"] = json!("source_tree_incomplete");
                    }
                    updates.push(("artifact".into(), final_id, artifact));
                    self.commit_download_records(updates, source_expected)?;
                    self.delete_pending_download_record(io::required_string(file, "id")?)?;
                    self.changed(app, Some(job_id));
                    if let Some(error) = sibling_error {
                        return Err(error);
                    }
                    Ok(())
                }
                .await
            }
            Err(error) => {
                if part.exists() {
                    let _ = std::fs::remove_file(&part);
                }
                artifact["status"] = json!("notDownloaded");
                artifact["errorCode"] = json!(io::stable_error(&error));
                self.catalogue.put("artifact", &id, artifact)?;
                Err(error)
            }
        };
        if let Err(error) = &outcome {
            if let Some(mut pending) = self.catalogue.get("artifact", &id)? {
                if pending["originalAvailable"] != true {
                    pending["status"] = json!("notDownloaded");
                    pending["errorCode"] = json!(io::stable_error(error));
                    self.catalogue.put("artifact", &id, pending)?;
                }
            }
        }
        outcome
    }
    async fn stream_download(
        &self,
        app: &tauri::AppHandle,
        job_id: &str,
        file: &Value,
        part: &Path,
        root: &Path,
        cancel: &AtomicBool,
    ) -> Result<(String, u64, String, String, Value)> {
        let mut last = anyhow::anyhow!("download_failed");
        for attempt in 0..3u32 {
            io::check_cancel(cancel)?;
            let ticket = io::cancellable(
                cancel,
                self.stores
                    .download_ticket(app, io::required_string(file, "id")?),
            )
            .await?;
            let ticket = match ticket {
                Ok(ticket) => ticket,
                Err(error) if retryable_download_resolution(&error) && attempt < 2 => {
                    io::delay(cancel, Duration::from_secs(1 << attempt)).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let current = self
                .catalogue
                .get("remote_file", io::required_string(file, "id")?)?
                .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
            ensure!(same_remote_identity(file, &current), "source_changed");
            if file["revisionBasis"] == "source" && current["revisionBasis"] == "source" {
                ensure!(file["revision"] == current["revision"], "source_changed");
            }
            if let Some(size) = expected_download_size(
                ticket.provider,
                None,
                ticket.expected_size,
                file["sizeBytes"].as_u64(),
            )? {
                io::space(root, size)?;
            }
            let response = io::cancellable(
                cancel,
                ticket
                    .client
                    .get(ticket.url)
                    .timeout(Duration::from_secs(3600))
                    .send(),
            )
            .await?
            .map_err(|_| anyhow::anyhow!("network_failed"));
            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    last = e;
                    if attempt < 2 {
                        io::delay(cancel, Duration::from_secs(1 << attempt)).await?;
                    }
                    continue;
                }
            };
            let status = response.status();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                let error = crate::stores::validate_download_response(
                    ticket.provider,
                    &response,
                    ticket.expected_origin.as_ref(),
                )
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("download_http_failed"));
                if io::stable_error(&error) != "link_expired" {
                    return Err(error);
                }
                last = error;
                if attempt < 2 {
                    io::delay(cancel, Duration::from_secs(1 << attempt)).await?;
                }
                continue;
            }
            if status.as_u16() == 429 || status.is_server_error() {
                last = anyhow::anyhow!(if status.as_u16() == 429 {
                    "rate_limited"
                } else {
                    "download_http_failed"
                });
                let wait = download_retry_delay(
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    attempt,
                    chrono::Utc::now().timestamp(),
                );
                let Some(wait) = wait else {
                    return Err(last);
                };
                if attempt < 2 {
                    io::delay(cancel, wait).await?;
                }
                continue;
            }
            crate::stores::validate_download_response(
                ticket.provider,
                &response,
                ticket.expected_origin.as_ref(),
            )?;
            ensure!(status.is_success(), "download_http_failed");
            let response_revision = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.starts_with('"') && v.ends_with('"') && v.len() <= 1024)
                .map(|v| format!("etag:{}", io::key(v)));
            let expected = expected_download_size(
                ticket.provider,
                response.content_length(),
                ticket.expected_size,
                file["sizeBytes"].as_u64(),
            )?;
            let size_error = if ticket.provider == crate::stores::StoreProvider::Booth {
                "download_size_mismatch"
            } else {
                "file_integrity_failed"
            };
            let mut output = tokio::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(part)
                .await?;
            let mut hasher = Sha256::new();
            let mut size = 0u64;
            let mut chunks = response.bytes_stream();
            let mut last_progress = std::time::Instant::now();
            let mut next_space_check = 0u64;
            let mut rate = self.preferences()?["downloadRateBytesPerSecond"]
                .as_u64()
                .unwrap_or(0);
            let slots = download_count(&self.preferences()?["downloadConcurrency"]) as f64;
            let result = async {
                while let Some(chunk) = io::cancellable(
                    cancel,
                    tokio::time::timeout(Duration::from_secs(60), chunks.next()),
                )
                .await?
                .map_err(|_| anyhow::anyhow!("network_timeout"))?
                {
                    io::check_cancel(cancel)?;
                    let bytes = chunk.map_err(|_| anyhow::anyhow!("network_failed"))?;
                    if size >= next_space_check {
                        io::space(root, 8 * 1024 * 1024)?;
                        next_space_check = size.saturating_add(8 * 1024 * 1024);
                    }
                    output.write_all(&bytes).await?;
                    hasher.update(&bytes);
                    size += bytes.len() as u64;
                    if let Some(expected) = expected {
                        ensure!(size <= expected, "{size_error}");
                    }
                    if last_progress.elapsed() >= Duration::from_millis(750) {
                        self.file_progress(
                            app,
                            job_id,
                            io::required_string(file, "id")?,
                            expected
                                .filter(|n| *n > 0)
                                .map(|n| size as f64 / n as f64)
                                .unwrap_or(0.0),
                        )
                        .await?;
                        rate = self.preferences()?["downloadRateBytesPerSecond"]
                            .as_u64()
                            .unwrap_or(0);
                        last_progress = std::time::Instant::now();
                    }
                    if rate > 0 {
                        io::delay(
                            cancel,
                            Duration::from_secs_f64(bytes.len() as f64 * slots / rate as f64),
                        )
                        .await?;
                    }
                }
                if let Some(expected) = expected {
                    ensure!(size == expected, "{size_error}");
                }
                output.flush().await?;
                output.sync_all().await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            drop(output);
            match result {
                Ok(()) => {
                    let hash = hex::encode(hasher.finalize());
                    let (revision, basis) =
                        bind_revision(file, &current, response_revision, &hash)?;
                    return Ok((hash, size, revision, basis, current));
                }
                Err(e) => {
                    let code = io::stable_error(&e);
                    if matches!(code, "cancelled" | "low_space" | "permission_denied") {
                        return Err(e);
                    }
                    last = e;
                }
            }
            if attempt < 2 {
                io::delay(cancel, Duration::from_secs(1 << attempt)).await?;
            }
        }
        Err(last)
    }
    pub(super) async fn extract_for_download(
        &self,
        seven_zip: &Path,
        root: &Value,
        artifact: &mut Value,
        files: &[Value],
        cancel: &Arc<AtomicBool>,
    ) -> Result<()> {
        let result = self
            .extract_one(seven_zip, root, artifact, files, cancel)
            .await;
        if let Err(error) = &result {
            artifact["status"] = json!("extractionFailed");
            artifact["errorCode"] = json!(io::stable_error(error));
            self.catalogue.put(
                "artifact",
                io::required_string(artifact, "id")?,
                artifact.clone(),
            )?;
        }
        result
    }

    pub(super) async fn extract_one(
        &self,
        seven_zip: &Path,
        root: &Value,
        artifact: &mut Value,
        files: &[Value],
        cancel: &Arc<AtomicBool>,
    ) -> Result<()> {
        let _permit = self.extraction.acquire().await?;
        io::check_cancel(cancel)?;
        let root_path = self.root_path(root)?;
        let source = io::safe_join(&root_path, io::required_string(artifact, "relativePath")?)?;
        let current_file = self
            .catalogue
            .get("remote_file", io::required_string(artifact, "fileId")?)?
            .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
        let mut candidates = HashMap::new();
        for file in files {
            if file["accountId"] != current_file["accountId"]
                || file["variantId"] != current_file["variantId"]
            {
                continue;
            }
            if let Some(a) = self.catalogue.get("artifact", &artifact_id(file))? {
                if a["originalAvailable"] == true {
                    let path = io::safe_join(&root_path, io::required_string(&a, "relativePath")?)?;
                    if path.file_name() == source.file_name() && path != source {
                        continue;
                    }
                    let h = io::required_string(&a, "hash")?.to_owned();
                    let size = a["sizeBytes"].as_u64().unwrap_or(0);
                    candidates.insert(path, (h, size));
                }
            }
        }
        // Identify the requested archive's group before reading any bytes.
        // Other complete archives in the same account/variant are unrelated.
        let volumes = archive::validate_volume_group(
            &source,
            &candidates.keys().cloned().collect::<Vec<_>>(),
        )?;
        for path in &volumes {
            let (hash, size) = candidates
                .get(path)
                .ok_or_else(|| anyhow::anyhow!("archive_missing_volume"))?;
            let (path, hash, size) = (path.clone(), hash.clone(), *size);
            tokio::task::spawn_blocking(move || io::verify(&path, &hash, size)).await??;
        }
        let id = io::required_string(artifact, "id")?.to_owned();
        artifact["status"] = json!("extracting");
        self.catalogue.put("artifact", &id, artifact.clone())?;
        let staging = io::safe_join(&root_path, ".nna-staging")?;
        std::fs::create_dir_all(&staging)?;
        let _empty_staging = io::EmptyDirectoryGuard(staging.clone());
        let archive_directory = io::readable_component(
            source
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Archive"),
            io::required_string(artifact, "fileId")?,
        );
        let tool = seven_zip.to_owned();
        let cancel2 = cancel.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            archive::extract_archive(&tool, &source, &volumes, &staging, &cancel2)
        })
        .await??;
        io::check_cancel(cancel)?;
        // The listed and CRC-checked extraction must still correspond to the
        // recorded source revisions before it can become durably Ready.
        for path in &extracted.source_paths {
            let (hash, size) = candidates
                .get(path)
                .ok_or_else(|| anyhow::anyhow!("archive_missing_volume"))?;
            let (path, hash, size) = (path.clone(), hash.clone(), *size);
            tokio::task::spawn_blocking(move || io::verify(&path, &hash, size)).await??;
        }
        let product_directory =
            self.managed_product_directory(io::required_string(artifact, "productId")?)?;
        let relative = io::unused_relative(
            &root_path,
            &format!("{}/extracted/{}", product_directory, archive_directory),
        )?;
        let destination = io::safe_join(&root_path, &relative)?;
        std::fs::create_dir_all(destination.parent().unwrap())?;
        std::fs::rename(&extracted.staging_path, &destination)?;
        artifact["status"] = json!("ready");
        artifact["extractedPath"] = json!(relative);
        artifact["productDirectory"] = json!(product_directory);
        artifact["extractedInventory"] = serde_json::to_value(&extracted.inventory)?;
        artifact["errorCode"] = Value::Null;
        // Critical ordering: usable inventory is durable before any archive is considered removable.
        self.catalogue.put("artifact", &id, artifact.clone())?;
        // Each volume retains its own original hash while sharing one complete extracted inventory.
        let mut group_updates = Vec::new();
        for mut peer in self
            .catalogue
            .related("artifact", io::required_string(artifact, "productId")?)?
        {
            if peer["rootId"] != root["id"] || peer["productId"] != artifact["productId"] {
                continue;
            }
            let Some(path) = peer["relativePath"]
                .as_str()
                .and_then(|r| io::safe_join(&root_path, r).ok())
            else {
                continue;
            };
            if extracted.source_paths.contains(&path) {
                peer["status"] = json!("ready");
                peer["extractedPath"] = artifact["extractedPath"].clone();
                peer["productDirectory"] = artifact["productDirectory"].clone();
                peer["extractedInventory"] = artifact["extractedInventory"].clone();
                peer["errorCode"] = Value::Null;
                group_updates.push((
                    "artifact".into(),
                    io::required_string(&peer, "id")?.to_owned(),
                    peer,
                ));
            }
        }
        self.catalogue.put_many(group_updates)?;
        // Every group member is durably ready before any source volume deletion.
        let group_records = self
            .catalogue
            .related("artifact", io::required_string(artifact, "productId")?)?;
        for source in extracted.source_paths {
            let records: Vec<_> = group_records
                .iter()
                .cloned()
                .filter(|a| {
                    a["rootId"] == root["id"]
                        && a["relativePath"]
                            .as_str()
                            .and_then(|r| io::safe_join(&root_path, r).ok())
                            .as_ref()
                            == Some(&source)
                })
                .collect();
            if records.is_empty() || records.iter().any(|a| a["status"] != "ready") {
                continue;
            }
            let hash = io::required_string(&records[0], "hash")?.to_owned();
            let size = records[0]["sizeBytes"].as_u64().unwrap_or(0);
            let check = source.clone();
            tokio::task::spawn_blocking(move || io::verify(&check, &hash, size)).await??;
            let pending: Vec<_> = records
                .iter()
                .map(|record| {
                    let mut record = record.clone();
                    record["originalDeletionPending"] = json!(true);
                    (
                        "artifact".into(),
                        record["id"].as_str().unwrap().to_owned(),
                        record,
                    )
                })
                .collect();
            self.catalogue.put_many(pending)?;
            // Deletion failure leaves usable extracted state and the original available.
            let removed = std::fs::remove_file(&source).is_ok();
            if removed {
                io::remove_empty_parents(
                    source.parent().unwrap(),
                    &io::safe_join(&root_path, &product_directory)?,
                );
            }
            let mut updates = Vec::new();
            for mut record in records {
                record["originalAvailable"] = json!(!removed);
                record
                    .as_object_mut()
                    .unwrap()
                    .remove("originalDeletionPending");
                let peer_id = io::required_string(&record, "id")?.to_owned();
                updates.push(("artifact".into(), peer_id.clone(), record.clone()));
                if peer_id == id {
                    *artifact = record;
                }
            }
            self.catalogue.put_many(updates)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_transient_resolution_errors_retry_and_actionable_failures_remain_terminal() {
        for code in [
            "NETWORK_UNAVAILABLE",
            "network_failed",
            "network_timeout",
            "DOWNLOAD_RESOLVER_FAILED",
            "SIGNED_LINK_EXPIRED",
        ] {
            assert!(
                retryable_download_resolution(&anyhow::anyhow!(code)),
                "{code}"
            );
        }
        for code in [
            "AUTH_REQUIRED",
            "RATE_LIMITED",
            "PERMISSION_DENIED",
            "FILE_UNAVAILABLE",
            "SOURCE_CHANGED",
            "UNSAFE_DOWNLOAD_URL",
            "cancelled",
            "low_space",
            "invalid_resolver",
        ] {
            assert!(
                !retryable_download_resolution(&anyhow::anyhow!(code)),
                "{code}"
            );
        }
    }

    #[test]
    fn download_retry_after_respects_dates_and_never_shortens_long_server_deferrals() {
        let now = chrono::DateTime::parse_from_rfc2822("Mon, 12 Oct 2026 01:00:00 GMT")
            .unwrap()
            .timestamp();
        assert_eq!(
            download_retry_delay(Some("120"), 0, now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(download_retry_delay(Some("121"), 0, now), None);
        assert_eq!(
            download_retry_delay(Some("0"), 0, now),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            download_retry_delay(None, 2, now),
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            download_retry_delay(Some("Mon, 12 Oct 2026 01:00:10 GMT"), 0, now),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            download_retry_delay(Some("Mon, 12 Oct 2026 01:03:00 GMT"), 0, now),
            None
        );
    }

    #[test]
    fn source_tree_publication_reuses_staged_bytes_and_preserves_existing_targets() {
        let root = std::env::temp_dir().join(format!(
            "nna-source-publication-synthetic-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join(".nna-partials")).unwrap();
        let part = root.join(".nna-partials/file.part");
        let relative = "source/file.bin";
        let target = root.join(relative);
        let bytes = b"synthetic source bytes";
        std::fs::write(&part, bytes).unwrap();
        let (hash, size) = io::hash_file(&part).unwrap();
        let cancel = AtomicBool::new(false);
        #[cfg(windows)]
        let mut original = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&part)
            .unwrap();
        publish_source_tree_part(&root, relative, &part, &hash, size, &cancel).unwrap();
        assert!(!part.exists());
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        #[cfg(windows)]
        {
            use std::io::Write;
            // The open staged handle still addresses the final file, proving
            // publication moved the file object instead of copying its bytes.
            original.set_len(0).unwrap();
            assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
            original.write_all(bytes).unwrap();
            original.sync_all().unwrap();
            drop(original);
        }
        // A matching retry removes only its own partial and keeps the target.
        std::fs::write(&part, bytes).unwrap();
        publish_source_tree_part(&root, relative, &part, &hash, size, &cancel).unwrap();
        assert!(!part.exists());
        assert_eq!(std::fs::read(&target).unwrap(), bytes);

        std::fs::write(&part, bytes).unwrap();
        std::fs::write(&target, b"preserve existing target").unwrap();
        assert!(publish_source_tree_part(&root, relative, &part, &hash, size, &cancel).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve existing target");
        assert_eq!(std::fs::read(&part).unwrap(), bytes);

        cancel.store(true, Ordering::Release);
        assert!(
            publish_source_tree_part(&root, "cancelled/file.bin", &part, &hash, size, &cancel)
                .is_err()
        );
        assert!(!root.join("cancelled").exists());
        assert_eq!(std::fs::read(&part).unwrap(), bytes);
        std::fs::remove_file(&part).unwrap();
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_dir(root.join(".nna-partials")).unwrap();
        std::fs::remove_dir(root.join("source")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn source_file_tree_preserves_sibling_paths_verified_reuse_and_prior_bytes() {
        let root = std::env::temp_dir().join(format!(
            "nna-source-tree-synthetic-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("old-flat")).unwrap();
        let model = json!({"id":"model","provider":"jinxxy","productId":"jinxxy:synthetic","accountId":"synthetic-account","inventoryId":"synthetic-inventory","name":"model.fbx","sourceRelativePath":"folder/model.fbx","revision":"source:r1"});
        let mut material = model.clone();
        material["id"] = json!("material");
        material["name"] = json!("material.mat");
        material["sourceRelativePath"] = json!("folder/materials/material.mat");
        let mut other = material.clone();
        other["id"] = json!("other-material");
        other["sourceRelativePath"] = json!("alternate/material.mat");
        validate_source_tree_paths(&[model.clone(), material.clone(), other.clone()]).unwrap();
        let model_relative = source_tree_relative(&model, "download:one", "Tool")
            .unwrap()
            .unwrap();
        let material_relative = source_tree_relative(&material, "download:one", "Tool")
            .unwrap()
            .unwrap();
        assert_eq!(
            source_tree_relative(&model, "download:one", "Tool")
                .unwrap()
                .unwrap(),
            model_relative
        );
        std::fs::write(
            root.join("old-flat/model.fbx"),
            b"references materials/material.mat",
        )
        .unwrap();
        std::fs::write(
            root.join("old-flat/material.mat"),
            b"synthetic material bytes",
        )
        .unwrap();
        let (model_hash, model_size) = io::hash_file(&root.join("old-flat/model.fbx")).unwrap();
        let (material_hash, material_size) =
            io::hash_file(&root.join("old-flat/material.mat")).unwrap();
        let cancel = AtomicBool::new(false);
        retain_source_tree_copy(
            &root,
            "old-flat/model.fbx",
            &model_relative,
            &model_hash,
            model_size,
            &cancel,
        )
        .unwrap();
        retain_source_tree_copy(
            &root,
            "old-flat/material.mat",
            &material_relative,
            &material_hash,
            material_size,
            &cancel,
        )
        .unwrap();
        let model_path = io::safe_join(&root, &model_relative).unwrap();
        let material_path = model_path.parent().unwrap().join("materials/material.mat");
        assert_eq!(
            material_path,
            io::safe_join(&root, &material_relative).unwrap()
        );
        io::verify(&material_path, &material_hash, material_size).unwrap();
        {
            let catalogue = Catalogue::open(&root.join("data")).unwrap();
            let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
            let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
            let service = StorageService::new(catalogue.clone(), stores).unwrap();
            let mapped_root = json!({"id":"synthetic-root","path":root,"status":"online"});
            catalogue
                .put("root", "synthetic-root", mapped_root.clone())
                .unwrap();
            let model_artifact = json!({"id":artifact_id(&model),"fileId":model["id"],"productId":model["productId"],"rootId":"synthetic-root","relativePath":model_relative,"revision":model["revision"],"hash":model_hash,"sizeBytes":model_size,"status":"ready","originalAvailable":true});
            let material_artifact = json!({"id":artifact_id(&material),"fileId":material["id"],"productId":material["productId"],"rootId":"synthetic-root","relativePath":material_relative,"revision":material["revision"],"hash":material_hash,"sizeBytes":material_size,"status":"ready","originalAvailable":true});
            for artifact in [&model_artifact, &material_artifact] {
                catalogue
                    .put(
                        "artifact",
                        artifact["id"].as_str().unwrap(),
                        artifact.clone(),
                    )
                    .unwrap();
            }
            let files = [model.clone(), material.clone()];
            let before = catalogue.list("artifact").unwrap();
            let mut selected_files = files.to_vec();
            selected_files.push(other.clone()); // Unselected, undownloaded leaves stay partial.
            let (prepared, expected) = service
                .prepare_selected_source_siblings(
                    &mapped_root,
                    &root,
                    &model,
                    &selected_files,
                    "download:selected",
                    "assets/Synthetic--stable",
                    &Arc::new(AtomicBool::new(false)),
                )
                .await
                .unwrap();
            assert_eq!(prepared.len(), 2); // One retained historical + one current copied sibling.
            assert_eq!(expected.len(), 2);
            let copied = prepared
                .iter()
                .find(|(_, id, _)| id == &artifact_id(&material))
                .unwrap();
            let copied_path =
                io::safe_join(&root, copied.2["relativePath"].as_str().unwrap()).unwrap();
            io::verify(&copied_path, &material_hash, material_size).unwrap();
            assert_eq!(catalogue.list("artifact").unwrap(), before);
            io::verify(
                &io::safe_join(&root, &material_relative).unwrap(),
                &material_hash,
                material_size,
            )
            .unwrap();
            let mut unavailable = material_artifact.clone();
            unavailable["originalAvailable"] = json!(false);
            catalogue
                .put("artifact", &artifact_id(&material), unavailable)
                .unwrap();
            assert_eq!(
                service
                    .prepare_selected_source_siblings(
                        &mapped_root,
                        &root,
                        &model,
                        &selected_files,
                        "download:selected-missing",
                        "assets/Synthetic--stable",
                        &Arc::new(AtomicBool::new(false)),
                    )
                    .await
                    .unwrap_err()
                    .to_string(),
                "source_tree_incomplete"
            );
            catalogue
                .put(
                    "artifact",
                    &artifact_id(&material),
                    material_artifact.clone(),
                )
                .unwrap();
            // Repeated selected updates keep the same verified tree and artifact records.
            for _ in 0..2 {
                assert!(service
                    .verified_source_manifest(&mapped_root, &root, &files, true, &cancel)
                    .await
                    .unwrap());
            }
            assert_eq!(catalogue.list("artifact").unwrap(), before);
            // A source without an updated timestamp still performs GET; only
            // its actual equal hash/size may retain this single-leaf Ready path.
            let mut unverified = model.clone();
            unverified["requiresContentVerification"] = json!(true);
            assert_eq!(
                service
                    .verified_single_source_content(
                        &mapped_root,
                        &root,
                        &model,
                        &[unverified.clone()],
                        true,
                        &model_hash,
                        model_size
                    )
                    .await
                    .unwrap(),
                Some(model_artifact.clone())
            );
            assert!(service
                .verified_single_source_content(
                    &mapped_root,
                    &root,
                    &model,
                    &[unverified.clone()],
                    true,
                    "different-actual-hash",
                    model_size
                )
                .await
                .unwrap()
                .is_none());
            assert!(service
                .verified_single_source_content(
                    &mapped_root,
                    &root,
                    &model,
                    &[unverified.clone()],
                    true,
                    &model_hash,
                    model_size + 1
                )
                .await
                .unwrap()
                .is_none());
            let mut moved = model.clone();
            moved["sourceRelativePath"] = json!("moved/model.fbx");
            assert!(service
                .verified_single_source_content(
                    &mapped_root,
                    &root,
                    &moved,
                    &[moved.clone()],
                    true,
                    &model_hash,
                    model_size
                )
                .await
                .unwrap()
                .is_none());
            assert!(service
                .verified_single_source_content(
                    &mapped_root,
                    &root,
                    &model,
                    &[unverified, material.clone()],
                    true,
                    &model_hash,
                    model_size
                )
                .await
                .unwrap()
                .is_none());
            assert_eq!(catalogue.list("artifact").unwrap(), before);
            assert!(!io::safe_join(
                &root,
                &source_tree_relative(&model, "download:two", "Tool")
                    .unwrap()
                    .unwrap()
            )
            .unwrap()
            .exists());
            let mut changed = model.clone();
            changed["revision"] = json!("source:r2");
            assert!(!service
                .verified_source_manifest(
                    &mapped_root,
                    &root,
                    &[changed, material.clone()],
                    true,
                    &cancel
                )
                .await
                .unwrap());
            assert!(!service
                .verified_source_manifest(
                    &mapped_root,
                    &root,
                    &[model.clone(), material.clone(), other.clone()],
                    true,
                    &cancel
                )
                .await
                .unwrap());
            // An interrupted mixed layout must still rebuild one coherent sibling tree.
            let mixed_relative = source_tree_relative(&material, "download:interrupted", "Tool")
                .unwrap()
                .unwrap();
            retain_source_tree_copy(
                &root,
                &material_relative,
                &mixed_relative,
                &material_hash,
                material_size,
                &cancel,
            )
            .unwrap();
            let mut mixed_artifact = material_artifact.clone();
            mixed_artifact["relativePath"] = json!(mixed_relative);
            catalogue
                .put("artifact", &artifact_id(&material), mixed_artifact)
                .unwrap();
            assert!(!service
                .verified_source_manifest(&mapped_root, &root, &files, true, &cancel)
                .await
                .unwrap());
            catalogue
                .put("artifact", &artifact_id(&material), material_artifact)
                .unwrap();
            std::fs::remove_file(&model_path).unwrap();
            assert!(service
                .verified_single_source_content(
                    &mapped_root,
                    &root,
                    &model,
                    &[model.clone()],
                    true,
                    &model_hash,
                    model_size
                )
                .await
                .unwrap()
                .is_none());
            assert!(!service
                .verified_source_manifest(&mapped_root, &root, &files, true, &cancel)
                .await
                .unwrap());
            retain_source_tree_copy(
                &root,
                "old-flat/model.fbx",
                &model_relative,
                &model_hash,
                model_size,
                &cancel,
            )
            .unwrap();
            assert!(service
                .verified_source_manifest(&mapped_root, &root, &files, true, &cancel)
                .await
                .unwrap());
            assert_eq!(catalogue.list("artifact").unwrap(), before);
        }
        // Same-job retry reuses complete bytes, while a new job retains a separate tree.
        retain_source_tree_copy(
            &root,
            "old-flat/model.fbx",
            &model_relative,
            &model_hash,
            model_size,
            &cancel,
        )
        .unwrap();
        let next_relative = source_tree_relative(&model, "download:two", "Tool")
            .unwrap()
            .unwrap();
        assert_ne!(next_relative, model_relative);
        retain_source_tree_copy(
            &root,
            &model_relative,
            &next_relative,
            &model_hash,
            model_size,
            &cancel,
        )
        .unwrap();
        io::verify(&root.join("old-flat/model.fbx"), &model_hash, model_size).unwrap();
        io::verify(&model_path, &model_hash, model_size).unwrap();
        // A changed destination is preserved rather than replaced by matching old bytes.
        std::fs::write(&material_path, b"retained damaged destination").unwrap();
        assert!(retain_source_tree_copy(
            &root,
            "old-flat/material.mat",
            &material_relative,
            &material_hash,
            material_size,
            &cancel
        )
        .is_err());
        assert_eq!(
            std::fs::read(&material_path).unwrap(),
            b"retained damaged destination"
        );
        io::verify(
            &root.join("old-flat/material.mat"),
            &material_hash,
            material_size,
        )
        .unwrap();
        let mut unsafe_file = model.clone();
        unsafe_file["sourceRelativePath"] = json!("../model.fbx");
        assert!(validate_source_tree_paths(&[unsafe_file]).is_err());
        let mut collision = model.clone();
        collision["id"] = json!("another");
        collision["sourceRelativePath"] = json!("Folder/model.fbx");
        assert!(validate_source_tree_paths(&[model.clone(), collision]).is_err());
        let mut moved = model.clone();
        moved["sourceRelativePath"] = json!("other/model.fbx");
        assert!(!same_remote_identity(&model, &moved));
        let mut booth = model;
        booth["provider"] = json!("booth");
        assert!(source_tree_relative(&booth, "download:one", "Tool")
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn verified_provider_file_length_cannot_be_overridden_by_response_body_length() {
        use crate::stores::StoreProvider;
        for provider in [StoreProvider::Gumroad, StoreProvider::Jinxxy] {
            let error = expected_download_size(provider, Some(12), Some(42), None).unwrap_err();
            assert_eq!(io::stable_error(&error), "file_integrity_failed");
            assert_eq!(
                expected_download_size(provider, Some(0), Some(0), Some(42)).unwrap(),
                Some(0)
            );
            assert_eq!(
                expected_download_size(provider, None, Some(42), Some(12)).unwrap(),
                Some(42)
            );
            assert_eq!(
                expected_download_size(provider, Some(42), None, None).unwrap(),
                Some(42)
            );
        }
        assert_eq!(
            expected_download_size(StoreProvider::Booth, Some(12), Some(42), None).unwrap(),
            Some(12)
        );
        assert_eq!(
            expected_download_size(StoreProvider::Booth, None, Some(42), Some(12)).unwrap(),
            Some(42)
        );
        assert_eq!(
            expected_download_size(StoreProvider::Booth, None, None, Some(12)).unwrap(),
            None
        );
    }
    #[test]
    fn final_revision_uses_positive_source_or_actual_verified_content() {
        let unknown = json!({"revision":"unknown"});
        assert_eq!(
            bind_revision(&unknown, &unknown, None, "abcd").unwrap(),
            ("sha256:abcd".into(), "contentHash".into())
        );
        assert_eq!(
            bind_revision(&unknown, &unknown, Some("etag:actual".into()), "abcd")
                .unwrap()
                .0,
            "etag:actual"
        );
        let source = json!({"revision":"source:v1","revisionBasis":"source"});
        assert_eq!(
            bind_revision(&source, &source, Some("etag:actual".into()), "abcd")
                .unwrap()
                .0,
            "source:v1"
        );
    }
    #[test]
    fn source_hash_mismatch_fails_but_historical_hash_does_not_override_new_content() {
        let expected = json!({"revision":"sha256:expected","revisionBasis":"source"});
        assert!(bind_revision(&expected, &expected, None, "different").is_err());
        let historical = json!({"revision":"sha256:old","revisionBasis":"source","requiresContentVerification":true});
        assert_eq!(
            bind_revision(&historical, &historical, None, "new")
                .unwrap()
                .0,
            "sha256:new"
        );
    }
}
