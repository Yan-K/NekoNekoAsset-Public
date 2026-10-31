//! Durable product-level downloads and guarded, verified local storage.
mod ai_evidence;
pub(crate) use ai_evidence::{
    ai_public_urls_in_text, redact_ai_evidence_text, safe_ai_evidence_name,
};
mod archive;
mod io;
mod local;
mod local_delete;
mod operations;
mod portable;
mod shared;
#[cfg(test)]
mod tests;
mod transfer;
mod unity_snapshot;
pub use unity_snapshot::{UnityLeaf, UnitySnapshot, UnitySnapshotEntry};

pub(crate) fn checked_library_root(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "root_offline");
    let metadata = std::fs::metadata(path)
        .map_err(|error| anyhow::Error::new(error).context("root_offline"))?;
    ensure!(metadata.is_dir(), "library_path_not_directory");
    archive::validate_no_links(path)?;
    Ok(path.canonicalize()?)
}

pub(crate) fn validate_library_path(path: &Path) -> Result<()> {
    archive::validate_no_links(path)
}

use crate::{catalogue::Catalogue, stores::StoreService};
use anyhow::{bail, ensure, Result};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{Emitter, Manager};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, RwLock, Semaphore},
};

#[derive(Clone)]
pub struct StorageService {
    catalogue: Catalogue,
    stores: Arc<StoreService>,
    started: Arc<AtomicBool>,
    queue_gate: Arc<AsyncMutex<()>>,
    operations: Arc<RwLock<()>>,
    extraction: Arc<Semaphore>,
    cancellations: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    process_lock: Arc<Mutex<Option<File>>>,
    automatic_pending: Arc<Mutex<AutomaticPending>>,
}

#[derive(Default)]
struct AutomaticPending {
    full: bool,
    products: HashSet<String>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn artifact_id(file: &Value) -> String {
    io::key(&format!(
        "{}\0{}",
        file["id"].as_str().unwrap_or(""),
        file["revision"].as_str().unwrap_or("unknown")
    ))
}
fn download_count(v: &Value) -> usize {
    match v.as_i64() {
        Some(n) if n > 6 => 6,
        Some(n) if n < 1 => 1,
        Some(n) => n as usize,
        None => 3,
    }
}
fn adaptive_download_count(
    configured: &Value,
    provider: Option<crate::stores::StoreProvider>,
    jobs: &[Value],
) -> usize {
    use crate::stores::StoreProvider;
    let syncing = provider.is_some_and(|provider| {
        jobs.iter().any(|job| {
            matches!(job["state"].as_str(), Some("queued" | "running"))
                && match (provider, job["type"].as_str()) {
                    (StoreProvider::Booth, Some("boothSync" | "boothUpdate"))
                    | (StoreProvider::Gumroad, Some("gumroadSync" | "gumroadUpdate"))
                    | (StoreProvider::Jinxxy, Some("jinxxySync" | "jinxxyUpdate")) => true,
                    _ => false,
                }
        })
    });
    if syncing {
        1
    } else {
        download_count(configured)
    }
}
fn file_manifest(files: Vec<Value>, entitlements: &[Value]) -> (Vec<Value>, bool) {
    let complete =
        !entitlements.is_empty() && entitlements.iter().all(|e| e["manifestComplete"] == true);
    let ids: HashSet<_> = entitlements
        .iter()
        .flat_map(|e| strings(&e["fileIds"]))
        .collect();
    let files: Vec<_> = files
        .into_iter()
        .filter(|f| f["id"].as_str().is_some_and(|id| ids.contains(id)))
        .collect();
    let complete = complete && files.len() == ids.len();
    (files, complete)
}
fn fingerprint(files: &[Value]) -> String {
    let mut fields: Vec<_> = files
        .iter()
        .map(|f| {
            json!([
                f["id"],
                f["revision"],
                f["requiresContentVerification"] == true
            ])
        })
        .collect();
    fields.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    io::key(&serde_json::to_string(&fields).unwrap_or_default())
}
fn selected_download_files(files: Vec<Value>, selected: &Value) -> Result<Vec<Value>> {
    if selected.is_null() {
        return Ok(files);
    }
    let ids = strings(selected);
    ensure!(
        selected.as_array().is_some_and(|values| values.len() == 1) && ids.len() == 1,
        "invalid_arguments"
    );
    let files: Vec<_> = files
        .into_iter()
        .filter(|file| file["id"] == ids[0])
        .collect();
    ensure!(files.len() == 1, "file_missing");
    Ok(files)
}
#[cfg(test)]
fn native_seven_zip_test_tool() -> PathBuf {
    let path = std::env::var_os("NNA_7ZIP")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/tools/7zip/7z.exe")
        });
    assert!(
        path.is_file(),
        "native archive regression requires bundled 7-Zip or explicit NNA_7ZIP"
    );
    path
}
impl StorageService {
    pub fn new(catalogue: Catalogue, stores: Arc<StoreService>) -> Result<Self> {
        Ok(Self {
            catalogue,
            stores,
            started: Arc::new(AtomicBool::new(false)),
            queue_gate: Arc::new(AsyncMutex::new(())),
            operations: Arc::new(RwLock::new(())),
            extraction: Arc::new(Semaphore::new(1)),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            process_lock: Arc::new(Mutex::new(None)),
            automatic_pending: Arc::new(Mutex::new(AutomaticPending::default())),
        })
    }
    fn changed(&self, app: &tauri::AppHandle, id: Option<&str>) {
        let _ = app.emit("nna:changed", json!({"scope":"products","jobId":id}));
    }
    fn jobs_changed(&self, app: &tauri::AppHandle, id: &str) {
        let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":id}));
    }
    fn root(&self, id: Option<&str>) -> Result<Value> {
        let configured = self.preferences()?["libraryRootId"]
            .as_str()
            .map(str::to_owned);
        let root = if let Some(id) = id.or(configured.as_deref()) {
            self.catalogue.get("root", id)?
        } else {
            self.catalogue
                .list("root")?
                .into_iter()
                .find(|r| r["active"] != false)
        };
        root.ok_or_else(|| anyhow::anyhow!("root_unconfigured"))
    }
    fn root_path(&self, root: &Value) -> Result<PathBuf> {
        self.catalogue.check_library_access()?;
        let p = if root["libraryRelative"] == true {
            let library = self
                .catalogue
                .library_root()
                .ok_or_else(|| anyhow::anyhow!("root_offline"))?;
            if root["path"] == "." {
                library.to_path_buf()
            } else {
                io::safe_join(library, io::required_string(root, "path")?)?
            }
        } else {
            PathBuf::from(io::required_string(root, "path")?)
        };
        ensure!(p.is_dir(), "root_offline");
        io::reject_link(&p)?;
        Ok(p)
    }
    fn preferences(&self) -> Result<Value> {
        Ok(self
            .catalogue
            .get("preferences", "app")?
            .unwrap_or(json!({})))
    }
    fn managed_product_directory(&self, product: &str) -> Result<String> {
        if let Some(directory) = self
            .catalogue
            .related("artifact", product)?
            .iter()
            .find_map(|a| a["productDirectory"].as_str().map(str::to_owned))
        {
            archive::validate_relative_path(&directory)?;
            return Ok(directory);
        }
        let source = self
            .catalogue
            .get("product", product)?
            .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
        Ok(format!(
            "assets/{}",
            io::readable_component(
                source["sourceTitle"]
                    .as_str()
                    .or(source["title"].as_str())
                    .unwrap_or("Asset"),
                product
            )
        ))
    }
    fn put_job(&self, job: &mut Value, state: &str, error: Option<&str>) -> Result<()> {
        job["state"] = json!(state);
        job["updatedAt"] = json!(now());
        job["errorCode"] = json!(error);
        self.catalogue
            .put("job", io::required_string(job, "id")?, job.clone())
    }
    fn cancellation(&self, id: &str) -> Arc<AtomicBool> {
        self.cancellations
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }
    fn cancel_job(&self, id: &str) -> Result<Value> {
        loop {
            let before = self.catalogue.get("job", id)?
                .ok_or_else(|| anyhow::anyhow!("job_missing"))?;
            ensure!(before["type"] == "download"
                || before["type"].as_str().is_some_and(|t| t.starts_with("storage:")),
                "invalid_job");
            if !matches!(before["state"].as_str(), Some("queued" | "running")) {
                return Ok(json!({"cancelRequested":false}));
            }
            let mut job = before.clone();
            job["cancelRequested"] = json!(true);
            job["updatedAt"] = json!(now());
            if job["state"] == "queued" {
                job["state"] = json!("cancelled");
                job["errorCode"] = Value::Null;
            }
            if self.catalogue.put_many_if_unchanged(
                vec![("job".into(), id.to_owned(), job)],
                vec![("job".into(), id.to_owned(), Some(before))],
            )? {
                self.cancellation(id).store(true, Ordering::Release);
                return Ok(json!({"cancelRequested":true}));
            }
        }
    }
    fn remote_files(&self, product: &str) -> Result<(Vec<Value>, bool)> {
        if let Some(source) = self
            .catalogue
            .get("product", product)?
            .filter(|p| p["provider"] == "local")
        {
            let ids: HashSet<_> = strings(&source["localManifest"]["fileIds"])
                .into_iter()
                .collect();
            let files: Vec<_> = self
                .catalogue
                .related("remote_file", product)?
                .into_iter()
                .filter(|f| {
                    f["id"].as_str().is_some_and(|id| ids.contains(id))
                        && f["originKind"] == "local"
                })
                .collect();
            let complete = source["localManifest"]["complete"] == true
                && !ids.is_empty()
                && files.len() == ids.len();
            return Ok((files, complete));
        }
        let entitlements = self.catalogue.effective_entitlements(product)?;
        Ok(file_manifest(
            self.catalogue.related("remote_file", product)?,
            &entitlements,
        ))
    }
    fn enabled_files(&self, files: Vec<Value>) -> Result<Vec<Value>> {
        let enabled = self.available_download_accounts()?;
        Ok(files
            .into_iter()
            .filter(|f| f["accountId"].as_str().is_some_and(|a| enabled.contains(a)))
            .collect())
    }
    fn available_download_accounts(&self) -> Result<HashSet<String>> {
        Ok(self
            .catalogue
            .list("account")?
            .into_iter()
            .filter(|a| {
                a["disconnected"] != true && crate::stores::has_local_session(&self.catalogue, a)
            })
            .filter_map(|a| a["id"].as_str().map(str::to_owned))
            .collect())
    }
    fn download_has_local_session(&self, job: &Value, accounts: &HashSet<String>) -> Result<bool> {
        if self.catalogue.library_root().is_none() {
            return Ok(true);
        }
        for id in strings(&job["fileIds"]) {
            let Some(file) = self.catalogue.get("remote_file", &id)? else {
                continue;
            };
            if !accounts.contains(file["accountId"].as_str().unwrap_or("")) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn put_queued_download(
        &self,
        before: &Value,
        job: &mut Value,
        state: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        ensure!(before["type"] == "download" && before["state"] == "queued", "invalid_job");
        let id = io::required_string(before, "id")?;
        job["state"] = json!(state);
        job["updatedAt"] = json!(now());
        job["errorCode"] = json!(error);
        self.catalogue.put_many_if_unchanged(
            vec![("job".into(), id.to_owned(), job.clone())],
            vec![("job".into(), id.to_owned(), Some(before.clone()))],
        )
    }
    fn queued_download(&self) -> Result<(Option<Value>, bool)> {
        let jobs = self.catalogue.queued_jobs(Some("download"), None)?;
        if jobs.is_empty() {
            return Ok((None, false));
        }
        let accounts = self.available_download_accounts()?;
        let mut changed = false;
        for mut job in jobs {
            let before = job.clone();
            if !self.download_has_local_session(&job, &accounts)? {
                if job["stage"] != "authRequired" || job["errorCode"] != "auth_required" {
                    job["stage"] = json!("authRequired");
                    changed |= self.put_queued_download(&before, &mut job, "queued", Some("auth_required"))?;
                }
                continue;
            }
            if job["stage"] == "authRequired" {
                job.as_object_mut().unwrap().remove("stage");
                if !self.put_queued_download(&before, &mut job, "queued", None)? {
                    continue;
                }
                changed = true;
            }
            return Ok((Some(job), changed));
        }
        Ok((None, changed))
    }
    /// Held for the App lifetime in the library data folder. Windows denies other
    /// handles' reads of this byte-range lock, so a library move skips it.
    fn lock_process(&self) -> Result<()> {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.catalogue.data_dir().join("storage.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| anyhow::anyhow!("storage_already_running"))?;
        *self.process_lock.lock().unwrap() = Some(lock);
        Ok(())
    }
    pub async fn start(&self, app: tauri::AppHandle) -> Result<()> {
        let _startup = self.catalogue.begin_catalogue_work()?;
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let setup = (|| -> Result<()> {
            self.lock_process()?;
            for mut job in self.catalogue.list("job")? {
                if matches!(job["state"].as_str(), Some("running" | "queued")) {
                    if job["type"] == "storage:import_local"
                        && self.catalogue.library_root().is_some()
                        && job["originDeviceId"] != self.catalogue.device_id()
                    {
                        self.put_job(&mut job, "needsAction", Some("local_source_required"))?;
                        continue;
                    }
                    if job["type"] == "download" || job["type"] == "storage:import_local" {
                        self.put_job(&mut job, "queued", None)?;
                    } else if job["type"]
                        .as_str()
                        .is_some_and(|t| t.starts_with("storage:"))
                    {
                        self.put_job(&mut job, "needsAction", Some("interrupted"))?;
                    }
                }
            }
            self.reconcile_archive_cleanup()?;
            Ok(())
        })();
        if let Err(e) = setup {
            *self.process_lock.lock().unwrap() = None;
            self.started.store(false, Ordering::Release);
            return Err(e);
        }
        let service = self.clone();
        tauri::async_runtime::spawn(async move {
            // Recovery evaluates the existing catalogue once. Source ingestion and
            // preference changes subsequently provide explicit, bounded invalidation.
            let _ = service.reevaluate_automatic().await;
            loop {
                let _ = service.drain_automatic_changes().await;
                let work = match service.catalogue.begin_catalogue_work() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };
                let local = service
                    .catalogue
                    .queued_jobs(Some("storage:import_local"), None)
                    .ok()
                    .and_then(|jobs| {
                        jobs.into_iter().find(|job| {
                            service.catalogue.library_root().is_none()
                                || job["originDeviceId"] == service.catalogue.device_id()
                        })
                    });
                if let Some(job) = local {
                    service.process_local_job(&app, job).await;
                    continue;
                }
                let operation = service.operations.read().await;
                let (next, changed) = service.queued_download().unwrap_or((None, false));
                if changed {
                    service.changed(&app, None);
                }
                if let Some(mut job) = next {
                    let before = job.clone();
                    let id = job["id"].as_str().unwrap_or("").to_owned();
                    let cancel = service.cancellation(&id);
                    if job["cancelRequested"] == true {
                        cancel.store(true, Ordering::Release);
                    }
                    if service.put_queued_download(&before, &mut job, "running", None).unwrap_or(false) {
                        service.changed(&app, Some(&id));
                        let result = service.run_download(&app, &mut job, &cancel).await;
                        let (state, error) = match &result {
                            Ok(()) => ("completed", None),
                            Err(e) if io::stable_error(e) == "cancelled" => ("cancelled", None),
                            Err(e) => {
                                let code = io::stable_error(e);
                                (
                                    if matches!(
                                        code,
                                        "root_offline"
                                            | "low_space"
                                            | "auth_required"
                                            | "permission_denied"
                                            | "manifest_incomplete"
                                            | "archive_password_required"
                                            | "archive_missing_volume"
                                            | "source_tree_incomplete"
                                    ) {
                                        "needsAction"
                                    } else {
                                        "failed"
                                    },
                                    Some(code),
                                )
                            }
                        };
                        let _ = service.put_job(&mut job, state, error);
                        service.changed(&app, Some(&id));
                        if let Some(product) = job["productId"].as_str() {
                            // A source update received while this job was active
                            // may have been deduplicated against it.
                            let _ = service.source_products_changed(&[product.to_owned()]).await;
                        }
                    }
                    service.cancellations.lock().unwrap().remove(&id);
                } else {
                    drop(operation);
                    drop(work);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
        Ok(())
    }
    /// Startup or an automatic-download/root/account preference change only.
    pub async fn reevaluate_automatic(&self) -> Result<()> {
        self.automatic_pending.lock().unwrap().full = true;
        self.drain_automatic_changes().await
    }
    /// Reevaluate only products whose durable source manifest changed.
    pub async fn source_products_changed(&self, product_ids: &[String]) -> Result<()> {
        if product_ids.is_empty() {
            return Ok(());
        }
        self.automatic_pending
            .lock()
            .unwrap()
            .products
            .extend(product_ids.iter().cloned());
        self.drain_automatic_changes().await
    }
    async fn drain_automatic_changes(&self) -> Result<()> {
        {
            let pending = self.automatic_pending.lock().unwrap();
            if !pending.full && pending.products.is_empty() {
                return Ok(());
            }
        }
        let Ok(_operation) = self.operations.try_read() else {
            return Ok(());
        };
        // Retain the lightweight invalidation while offline/busy. In particular,
        // don't scan a whole startup catalogue repeatedly until its root returns.
        let root = match self.root(None) {
            Ok(root) => root,
            Err(error) if error.to_string() == "root_unconfigured" => return Ok(()),
            Err(error) => return Err(error),
        };
        if let Err(error) = self.root_path(&root) {
            if error.to_string() == "root_offline" {
                return Ok(());
            }
            return Err(error);
        }
        let pending = std::mem::take(&mut *self.automatic_pending.lock().unwrap());
        let result = async {
            let mut ids = pending.products.clone();
            if pending.full {
                for file in self.catalogue.list("remote_file")? {
                    if let Some(product) = file["productId"].as_str() {
                        ids.insert(product.to_owned());
                    }
                }
            }
            self.evaluate_automatic(&ids).await
        }
        .await;
        if result.is_err() {
            let mut retained = self.automatic_pending.lock().unwrap();
            retained.full |= pending.full;
            retained.products.extend(pending.products);
        }
        result
    }
    async fn evaluate_automatic(&self, product_ids: &HashSet<String>) -> Result<()> {
        let prefs = self.preferences()?;
        let all = prefs["autoDownloadAll"].as_bool().unwrap_or(false);
        let accounts = self.available_download_accounts()?;
        let mut pending = Vec::new();
        for id in product_ids {
            if !all
                && !self
                    .catalogue
                    .get("download_intent", id)?
                    .is_some_and(|intent| intent["wantDownload"] == true)
            {
                continue;
            }
            let files = self.catalogue.related("remote_file", id)?;
            let entitlements = self.catalogue.effective_entitlements(&id)?;
            let (files, _) = file_manifest(files, &entitlements);
            let files: Vec<_> = files
                .into_iter()
                .filter(|f| {
                    f["accountId"]
                        .as_str()
                        .is_some_and(|a| accounts.contains(a))
                })
                .collect();
            if files.is_empty() {
                continue;
            }
            let fingerprint = fingerprint(&files);
            // Failed/cancelled work requires an explicit retry; only a changed manifest queues again.
            if self
                .catalogue
                .related("job", id)?
                .iter()
                .any(|job| job["type"] == "download" && job["manifestFingerprint"] == fingerprint)
            {
                continue;
            }
            pending.push(id.clone());
        }
        if !pending.is_empty() {
            self.enqueue(&json!({"productIds":pending,"automatic":true}))
                .await?;
        }
        Ok(())
    }
    /// Account logout/removal holds this existing gate through session cleanup
    /// and record mutation; queued work rechecks current account facts afterward.
    pub async fn pause_download_queue(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.queue_gate.lock().await
    }
    async fn enqueue(&self, args: &Value) -> Result<Value> {
        let _work = self.catalogue.begin_catalogue_work()?;
        let _operation = self
            .operations
            .try_read()
            .map_err(|_| anyhow::anyhow!("storage_busy"))?;
        let _gate = self.queue_gate.lock().await;
        let automatic = args["automatic"] == true;
        let root = self.root(args["rootId"].as_str())?;
        self.root_path(&root)?;
        let ids = strings(&args["productIds"]);
        ensure!(!ids.is_empty(), "invalid_arguments");
        let selected = args.get("selectedFileIds").cloned().unwrap_or(Value::Null);
        ensure!(selected.is_null() || ids.len() == 1, "invalid_arguments");
        let mut result = Vec::new();
        let jobs = self.catalogue.list("job")?;
        let active: Vec<_> = jobs
            .iter()
            .filter(|j| {
                j["type"] == "download" && j["cancelRequested"] != true
                    && matches!(j["state"].as_str(), Some("queued" | "running"))
            })
            .collect();
        let mut writes = Vec::new();
        for id in ids.into_iter().collect::<HashSet<_>>() {
            let Some(product) = self.catalogue.get("product", &id)? else {
                if automatic {
                    continue;
                }
                bail!("product_missing");
            };
            if product["provider"] == "local" {
                ensure!(selected.is_null(), "local_source_required");
                if let Some(job_id) = self.retry_local_product_locked(&id, &jobs)? {
                    result.push(json!(job_id));
                }
                continue;
            }
            let (files, complete) = self.remote_files(&id)?;
            let files = if automatic {
                self.enabled_files(files)?
            } else {
                files
            };
            if automatic && files.is_empty() {
                continue;
            }
            let files = selected_download_files(files, &selected)?;
            if selected.is_null() {
                writes.push((
                    "download_intent".into(),
                    id.clone(),
                    json!({"id":id,"wantDownload":true}),
                ));
            }
            if let Some(job) = active.iter().find(|job| {
                job["productId"] == id
                    && if selected.is_null() {
                        job["selectedFileIds"].is_null()
                    } else {
                        job["selectedFileIds"] == selected
                            || (job["selectedFileIds"].is_null()
                                && strings(&job["fileIds"]).contains(&strings(&selected)[0]))
                    }
            }) {
                ensure!(
                    args["redownload"] != true || job["redownload"] == true,
                    "storage_busy"
                );
                result.push(job["id"].clone());
                continue;
            }
            let fingerprint = fingerprint(&files);
            let job_id = uuid::Uuid::new_v4().to_string();
            let job = json!({"id":job_id,"type":"download","productId":id,"rootId":root["id"],"automatic":args["automatic"]==true,"selectedFileIds":selected,"redownload":args["redownload"]==true,"fileIds":files.iter().map(|f|f["id"].clone()).collect::<Vec<_>>(),"manifestComplete":complete,"manifestFingerprint":fingerprint,"state":if files.is_empty(){"needsAction"}else{"queued"},"errorCode":if files.is_empty(){Some("manifest_incomplete")}else{None},"progress":0,"createdAt":now(),"updatedAt":now()});
            writes.push(("job".into(), job_id.clone(), job));
            result.push(json!(job_id));
        }
        self.catalogue.put_many(writes)?;
        Ok(json!({"jobIds":result}))
    }
    #[cfg(test)]
    pub(crate) async fn enqueue_for_test(&self, args: &Value) -> Result<Value> {
        self.enqueue(args).await
    }
    async fn enqueue_file(&self, args: &Value) -> Result<Value> {
        let id = io::required_string(args, "fileId")?;
        ensure!(
            args.get("redownload").is_none() || args["redownload"].is_boolean(),
            "invalid_arguments"
        );
        let file = self
            .catalogue
            .get("remote_file", id)?
            .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
        self.enqueue(&json!({"productIds":[file["productId"]],"selectedFileIds":[id],"rootId":args["rootId"],"redownload":args["redownload"]==true})).await
    }
    pub async fn command(
        &self,
        app: &tauri::AppHandle,
        action: &str,
        args: Value,
    ) -> Result<Value> {
        let deleting = action == "delete_local_products"
            || (action == "retry_job"
                && args["jobId"].as_str().is_some_and(|id| {
                    self.catalogue
                        .get("job", id)
                        .ok()
                        .flatten()
                        .is_some_and(|job| job["type"] == "storage:delete_local_products")
                }));
        let _work = if deleting
            || matches!(
                action,
                "remap_root"
                    | "migrate_root"
                    | "cleanup_old_root"
                    | "verify_root"
                    | "export_backup"
                    | "restore_backup"
                    | "export_zip"
                    | "product_availability"
                    | "open_product_folder"
                    | "open_file"
                    | "open_root_folder"
                    | "cancel_job"
            ) {
            None
        } else {
            Some(self.catalogue.begin_catalogue_work()?)
        };
        let result = match action {
            "import_local" => self.enqueue_local(&args).await,
            "local_files" => self.mutate_local_files(&args).await,
            "delete_local_products" => self.delete_local_products(&args, None).await,
            "enqueue_products" => self.enqueue(&args).await,
            "enqueue_file" => self.enqueue_file(&args).await,
            "cancel_job" => {
                let id = io::required_string(&args, "jobId")?;
                self.cancel_job(id)
            }
            "retry_job" | "retry_file" => {
                let mut retry = args.clone();
                if let Some(id) = args["jobId"].as_str() {
                    let job = self
                        .catalogue
                        .get("job", id)?
                        .ok_or_else(|| anyhow::anyhow!("job_missing"))?;
                    if job["type"] == "storage:delete_local_products" {
                        let result = self.delete_local_products(&args, Some(id)).await;
                        self.changed(app, Some(id));
                        return result;
                    }
                    if job["type"] == "storage:import_local" {
                        let result = self.retry_local_import(id).await;
                        self.changed(app, Some(id));
                        return result;
                    }
                    ensure!(job["type"] == "download", "invalid_job");
                    retry["productIds"] = json!([job["productId"]]);
                    retry["rootId"] = job["rootId"].clone();
                    retry["selectedFileIds"] = job["selectedFileIds"].clone();
                    retry["redownload"] = job["redownload"].clone();
                } else if let Some(id) = args["fileId"].as_str() {
                    let _ = id;
                    return self.enqueue_file(&args).await;
                }
                self.enqueue(&retry).await
            }
            "configure_root" => self.configure_root(&args).await,
            "remap_root" | "migrate_root" | "cleanup_old_root" | "verify_root"
            | "export_backup" | "restore_backup" | "export_zip" => {
                self.launch_operation(app, action, args).await
            }
            "product_availability" => {
                let service = self.clone();
                tokio::task::spawn_blocking(move || service.product_availability(&args)).await?
            }
            "open_product_folder" | "open_file" | "open_root_folder" => {
                let service = self.clone();
                let action = action.to_owned();
                tokio::task::spawn_blocking(move || service.open_artifact(&action, &args)).await?
            }
            _ => bail!("unsupported_storage_action"),
        };
        if !matches!(
            action,
            "product_availability" | "open_product_folder" | "open_file" | "open_root_folder"
        ) {
            self.changed(app, None);
        }
        result
    }
    async fn configure_root(&self, args: &Value) -> Result<Value> {
        if let Some(path) = self.catalogue.library_root() {
            let selected = checked_library_root(Path::new(io::required_string(args, "path")?))?;
            ensure!(selected == path, "use_remap_or_migrate");
            let mut root = self.root(None)?;
            root["path"] = json!(path);
            return Ok(root);
        }
        let _lock = self.operations.write().await;
        let path = PathBuf::from(io::required_string(args, "path")?);
        let path = tokio::task::spawn_blocking(move || io::absolute_dir(&path, true)).await??;
        for root in self.catalogue.list("root")? {
            if Path::new(root["path"].as_str().unwrap_or("")) == path {
                return Ok(root);
            }
        }
        ensure!(
            self.catalogue.list("root")?.is_empty(),
            "use_remap_or_migrate"
        );
        let id = uuid::Uuid::new_v4().to_string();
        let root = json!({"id":id,"path":path,"status":"online","active":true,"freeBytes":fs2::available_space(&path)?,"usedBytes":0});
        self.catalogue.put("root", &id, root.clone())?;
        self.catalogue
            .command("preferences", json!({"patch":{"libraryRootId":id}}))?;
        Ok(root)
    }
}
