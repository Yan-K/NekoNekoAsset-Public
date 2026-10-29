//! Native Gumroad buyer collector. Capability routes are transient, never persisted.
use super::{
    gumroad::{self, ContentObservation, FileAccess, LibraryPage, LibraryPurchase},
    http, session, DownloadTicket, StoreProvider,
};
use crate::{booth::RecordBatch, catalogue::Catalogue};
use anyhow::{bail, ensure, Result};
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt, StreamExt,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use url::Url;
struct BundleMembers {
    purchases: Vec<LibraryPurchase>,
    pages: u64,
    head_signatures: [String; 2],
    complete: bool,
}

type PublicRead<'a> =
    Shared<BoxFuture<'a, Result<Option<gumroad::PublicProductObservation>, String>>>;
fn shared_public_read<'a>(
    reads: &Mutex<BTreeMap<String, PublicRead<'a>>>,
    id: &str,
    make: impl FnOnce() -> PublicRead<'a>,
) -> Result<PublicRead<'a>> {
    let mut reads = reads
        .lock()
        .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
    Ok(reads.entry(id.into()).or_insert_with(make).clone())
}

#[derive(Clone)]
pub struct GumroadService {
    inner: Arc<Inner>,
}
struct Inner {
    catalogue: Catalogue,
    http: http::MetadataHttp,
    running: Mutex<BTreeSet<String>>,
    execution: tokio::sync::Mutex<()>,
    recovering: Mutex<bool>,
    downloads: Mutex<BTreeMap<String, DownloadSession>>,
    download_fill: tokio::sync::Mutex<()>,
    _process_lock: Arc<std::fs::File>,
}
#[derive(Clone)]
struct DownloadSession {
    client: reqwest::Client,
    jar: Arc<reqwest::cookie::Jar>,
    identity: String,
    verified_at: Instant,
    manifests: BTreeMap<String, DownloadManifest>,
    purchases: BTreeMap<String, LibraryPurchase>,
}
#[derive(Clone)]
struct DownloadManifest {
    purchase_ids: BTreeSet<String>,
    source_bundle_id: Option<String>,
    observed_at: Instant,
    files: Vec<gumroad::FileObservation>,
    revisions: BTreeMap<String, String>,
}
impl DownloadSession {
    fn valid(&self, account: &Value, now: Instant) -> bool {
        super::active_bound_account(account)
            && account["identityHash"] == self.identity
            && now.saturating_duration_since(self.verified_at) < Duration::from_secs(120)
    }
    fn snapshot(&self) -> Self {
        Self {
            client: self.client.clone(),
            jar: self.jar.clone(),
            identity: self.identity.clone(),
            verified_at: self.verified_at,
            manifests: BTreeMap::new(),
            purchases: BTreeMap::new(),
        }
    }
    fn index_purchases(&mut self, rows: &[LibraryPurchase]) {
        for row in rows {
            if self.purchases.len() < 10000 || self.purchases.contains_key(&row.purchase_id) {
                self.purchases.insert(row.purchase_id.clone(), row.clone());
            }
        }
    }
    fn indexed_purchases(
        &self,
        account: &Value,
        ids: &BTreeSet<&str>,
        now: Instant,
    ) -> Vec<LibraryPurchase> {
        if !self.valid(account, now) {
            return Vec::new();
        }
        ids.iter()
            .filter_map(|id| self.purchases.get(*id))
            .filter(|row| !row.excluded_non_asset())
            .cloned()
            .collect()
    }
}
fn source_fingerprint(file: &gumroad::FileObservation) -> String {
    fingerprint(&json!([file.name, file.folders, file.size_bytes]))
}
fn remote_file_id(account: &str, product: &str, file: &gumroad::FileObservation) -> String {
    format!(
        "gumroad:{account}:{}",
        fingerprint(&json!([product, file.source_product_id, file.file_id]))
    )
}
impl DownloadManifest {
    fn offered<'a>(
        &'a self,
        account: &str,
        product: &str,
        file: &Value,
        now: Instant,
    ) -> Option<&'a gumroad::FileObservation> {
        if now.saturating_duration_since(self.observed_at) >= Duration::from_secs(60)
            || file["productId"] != format!("gumroad:{product}")
            || file["accountId"] != account
            || file["sourceBundleId"].as_str() != self.source_bundle_id.as_deref()
            || !file["purchaseIds"].as_array().is_some_and(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .any(|id| self.purchase_ids.contains(id))
            })
        {
            return None;
        }
        self.files.iter().find(|offered| {
            offered.access == FileAccess::Downloadable
                && file["sourceFileId"] == offered.file_id
                && file["sourceProductId"] == offered.source_product_id
                && file["sourcePurchaseId"] == offered.source_purchase_id
                && file["id"] == remote_file_id(account, product, offered)
                && file["sourceFingerprint"] == source_fingerprint(offered)
                && self
                    .revisions
                    .get(file["id"].as_str().unwrap_or(""))
                    .is_some_and(|revision| file["revision"] == *revision)
        })
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("INVALID_ARGUMENT"))
}
fn fingerprint(value: &Value) -> String {
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}
fn safe_code(error: &anyhow::Error) -> String {
    let value = error.to_string();
    if value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        value
    } else {
        "GUMROAD_OPERATION_FAILED".into()
    }
}
fn library_url(page: u64, archived: bool) -> Url {
    let mut url = Url::parse("https://gumroad.com/library").unwrap();
    url.query_pairs_mut()
        .append_pair("sort", "purchase_date")
        .append_pair(
            "show_archived_only",
            if archived { "true" } else { "false" },
        )
        .append_pair("page", &page.to_string());
    url
}
fn signature(page: &LibraryPage) -> String {
    fingerprint(
        &json!({"count":page.count,"pages":page.pages,"archivedCount":page.archived_count,"unarchivedCount":page.unarchived_count,"purchaseIds":page.purchases.iter().map(|row| &row.purchase_id).collect::<Vec<_>>(),"bundleIds":page.bundles.iter().map(|row| &row.product_id).collect::<Vec<_>>()}),
    )
}

async fn enumerate_bundle_members<'a>(
    buyer: &str,
    page_limit: Option<u64>,
    fetch: impl Fn(u64, bool) -> BoxFuture<'a, Result<LibraryPage>>,
) -> Result<BundleMembers> {
    let mut members = BundleMembers {
        purchases: Vec::new(),
        pages: 0,
        head_signatures: Default::default(),
        complete: true,
    };
    let mut seen = BTreeSet::new();
    for (tab, archived) in [false, true].into_iter().enumerate() {
        let first = fetch(1, archived).await?;
        let count = first.count;
        let pages = first.pages;
        members.head_signatures[tab] = signature(&first);
        let mut first = Some(first);
        let observed_pages = page_limit.map_or(pages, |limit| pages.min(limit));
        members.complete &= observed_pages == pages;
        for index in 1..=observed_pages {
            let page = if index == 1 {
                first.take().unwrap()
            } else {
                fetch(index, archived).await?
            };
            ensure!(page.buyer_identity == buyer, "ACCOUNT_IDENTITY_MISMATCH");
            ensure!(
                page.pages == pages && page.count == count,
                "GUMROAD_LIBRARY_CHANGED"
            );
            members.pages += 1;
            for row in page.purchases {
                ensure!(
                    seen.insert(row.purchase_id.clone()),
                    "GUMROAD_PURCHASE_AMBIGUOUS"
                );
                members.purchases.push(row);
            }
        }
    }
    Ok(members)
}
fn assemble_observed_bundle(
    bundle: &gumroad::LibraryBundle,
    members: &BundleMembers,
    reuse: &BTreeMap<String, Option<ContentObservation>>,
) -> (LibraryPurchase, ContentObservation) {
    let (card, mut parent) = bundle_parent(bundle);
    for row in &members.purchases {
        parent.member_purchase_ids.insert(row.purchase_id.clone());
        if row.excluded_non_asset() {
            continue;
        }
        if let Some(Some(child)) = reuse.get(&row.purchase_id) {
            parent.unsupported_items += child.unsupported_items;
            merge_member_files(&mut parent, child.files.clone());
        } else {
            parent.unsupported_items += 1;
        }
    }
    parent.unsupported_items += usize::from(!members.complete);
    parent.inventory_complete = parent.unsupported_items == 0;
    (card, parent)
}

impl GumroadService {
    #[cfg(test)]
    pub(crate) fn execution_gate(&self) -> &tokio::sync::Mutex<()> {
        &self.inner.execution
    }
    pub fn new(catalogue: Catalogue) -> Result<Self> {
        let process_lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(catalogue.device_dir().join("gumroad.lock"))
            .map_err(|_| anyhow::anyhow!("GUMROAD_LOCK_UNAVAILABLE"))?;
        fs2::FileExt::try_lock_exclusive(&process_lock)
            .map_err(|_| anyhow::anyhow!("GUMROAD_ALREADY_RUNNING"))?;
        Self::with_process_lock(catalogue, Arc::new(process_lock))
    }
    pub(crate) fn rebind_catalogue(&self, catalogue: Catalogue) -> Result<Self> {
        ensure!(catalogue.device_dir() == self.inner.catalogue.device_dir(), "GUMROAD_DEVICE_MISMATCH");
        Self::with_process_lock(catalogue, self.inner._process_lock.clone())
    }
    fn with_process_lock(catalogue: Catalogue, process_lock: Arc<std::fs::File>) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                catalogue,
                http: http::MetadataHttp::new(StoreProvider::Gumroad),
                running: Mutex::new(BTreeSet::new()),
                execution: tokio::sync::Mutex::new(()),
                recovering: Mutex::new(false),
                downloads: Mutex::new(BTreeMap::new()),
                download_fill: tokio::sync::Mutex::new(()),
                _process_lock: process_lock,
            }),
        })
    }
    fn account(&self, id: &str) -> Result<Value> {
        uuid::Uuid::parse_str(id).map_err(|_| anyhow::anyhow!("INVALID_ACCOUNT_ID"))?;
        self.inner
            .catalogue
            .get("account", id)?
            .filter(|row| row["store"] == "Gumroad")
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))
    }
    pub async fn start(&self, app: &AppHandle) -> Result<()> {
        let _work = self.inner.catalogue.begin_catalogue_work()?;
        {
            let mut recovering = self
                .inner
                .recovering
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            if *recovering {
                return Ok(());
            }
            *recovering = true;
        }
        for job in self.inner.catalogue.list("job")? {
            if job["store"] != "Gumroad" || !matches!(job["state"].as_str(), Some("running" | "queued")) { continue; }
            let Some(job) = self.prepare_recovered_job(job)? else { continue };
            if let Err(error) = self.spawn(app.clone(), job) {
                if safe_code(&error) != "ACCOUNT_BUSY" { return Err(error); }
            }
        }
        Ok(())
    }
    fn prepare_recovered_job(&self, mut job: Value) -> Result<Option<Value>> {
        let id = field(&job, "id")?.to_owned();
        let result = (|| -> Result<Option<Value>> {
            let account_id = field(&job, "accountId")?;
            let account = self.account(account_id)?;
            if self.inner.catalogue.library_root().is_some()
                && (job["originDeviceId"] != self.inner.catalogue.device_id()
                    || (job["type"] != "gumroadLogin" && !super::has_local_session(&self.inner.catalogue, &account)))
            {
                job["state"] = json!("needsAction");
                job["errorCode"] = json!("RETRY_REQUIRED");
                job["updatedAt"] = json!(now());
                self.inner.catalogue.put("job", &id, job)?;
                return Ok(None);
            }
            if job["type"] == "gumroadLogin" {
                job["state"] = json!("needsAction");
                job["errorCode"] = json!("RETRY_REQUIRED");
                self.inner.catalogue.put("job", &id, job.clone())?;
                session::discard_unverified_account(&self.inner.catalogue, StoreProvider::Gumroad, field(&job, "accountId")?)?;
                return Ok(None);
            }
            Ok(Some(job))
        })();
        match result {
            Ok(job) => Ok(job),
            Err(error) => {
                if let Some(before) = self.inner.catalogue.get("job", &id)?
                    .filter(|job| matches!(job["state"].as_str(), Some("queued" | "running")))
                {
                    let mut failed = before.clone();
                    failed["state"] = json!("needsAction");
                    failed["errorCode"] = json!(safe_code(&error));
                    failed["updatedAt"] = json!(now());
                    self.inner.catalogue.put_many_if_unchanged(
                        vec![("job".into(), id.clone(), failed)],
                        vec![("job".into(), id, Some(before))],
                    )?;
                }
                Ok(None)
            }
        }
    }
    pub async fn command(&self, app: &AppHandle, action: &str, args: Value) -> Result<Value> {
        let _work = self.inner.catalogue.begin_catalogue_work()?;
        match action {
            "login" | "account_login" => {
                let account = field(&args, "accountId")?;
                self.account(account)?;
                let active = self.inner.catalogue.list("job")?.into_iter().find(|job| {
                    job["accountId"] == account
                        && matches!(job["state"].as_str(), Some("queued" | "running"))
                });
                ensure!(
                    active
                        .as_ref()
                        .is_none_or(|job| job["type"] == "gumroadLogin"),
                    "ACCOUNT_BUSY"
                );
                self.clear_downloads(account, None);
                let opening = session::window(
                    app,
                    &self.inner.catalogue.device_dir(),
                    StoreProvider::Gumroad,
                    account,
                    Url::parse("https://gumroad.com/login").unwrap(),
                    true,
                )
                .await;
                if let Err(error) = opening {
                    session::discard_unverified_account(&self.inner.catalogue, StoreProvider::Gumroad, account)?;
                    return Err(error);
                }
                if let Some(job) = active {
                    return Ok(json!({"jobId":job["id"]}));
                }
                let job = self.make_job("gumroadLogin", json!({"accountId":account}))?;
                self.spawn(app.clone(), job.clone())?;
                Ok(json!({"jobId":job["id"]}))
            }
            "account_remove" | "account_logout" => {
                let account = field(&args, "accountId")?;
                let mut row = self.account(account)?;
                let remove_products = if action == "account_remove" {
                    Some(
                        args["removeProducts"]
                            .as_bool()
                            .ok_or_else(|| anyhow::anyhow!("INVALID_ACCOUNT_ACTION"))?,
                    )
                } else {
                    None
                };
                self.clear_downloads(account, None);
                ensure!(
                    !self
                        .inner
                        .running
                        .lock()
                        .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                        .contains(account),
                    "ACCOUNT_BUSY"
                );
                let window = session::window(
                    app,
                    &self.inner.catalogue.device_dir(),
                    StoreProvider::Gumroad,
                    account,
                    Url::parse("https://gumroad.com/login").unwrap(),
                    false,
                )
                .await?;
                session::forget(
                    &self.inner.catalogue.device_dir(),
                    StoreProvider::Gumroad,
                    account,
                )?;
                session::disconnect(window).await?;
                if let Some(remove_products) = remove_products {
                    let result = self
                        .inner
                        .catalogue
                        .remove_account(account, remove_products)?;
                    let _ = app.emit("nna:changed", json!({"scope":"catalogue"}));
                    return Ok(result);
                }
                if self.inner.catalogue.library_root().is_none() {
                    row["disconnected"] = json!(true);
                    row["state"] = json!("disconnected");
                    row["status"] = json!("disconnected");
                    row["identityVerified"] = json!(false);
                    self.inner.catalogue.put("account", account, row)?;
                }
                let _ = app.emit("nna:changed", json!({"scope":"booth"}));
                Ok(json!({"accountId":account}))
            }
            "sync" => {
                let account = field(&args, "accountId")?;
                self.account(account)?;
                if let Some(existing) = self.inner.catalogue.list("job")?.into_iter().find(|job| {
                    job["accountId"] == account
                        && matches!(job["type"].as_str(), Some("gumroadSync" | "gumroadUpdate"))
                        && matches!(job["state"].as_str(), Some("queued" | "running"))
                }) {
                    return Ok(json!({"jobId":existing["id"]}));
                }
                ensure!(
                    matches!(args["source"].as_str(), None | Some("all" | "library")),
                    "INVALID_SOURCE"
                );
                ensure!(
                    matches!(
                        args["mode"].as_str(),
                        None | Some("incremental" | "full" | "forced")
                    ),
                    "INVALID_SYNC_MODE"
                );
                let job = self.make_job(
                    "gumroadSync",
                    json!({"accountId":account,"mode":args["mode"]}),
                )?;
                self.spawn(app.clone(), job.clone())?;
                Ok(json!({"jobId":job["id"]}))
            }
            "update_products" => {
                let ids: BTreeSet<String> = args["productIds"]
                    .as_array()
                    .or_else(|| args["ids"].as_array())
                    .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .filter(|value| value.starts_with("gumroad:"))
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))
                    })
                    .collect::<Result<_>>()?;
                ensure!(!ids.is_empty(), "INVALID_PRODUCT_IDS");
                let mut accounts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                let mut matched = BTreeSet::new();
                for entitlement in self.inner.catalogue.list("entitlement")? {
                    if entitlement["store"] == "Gumroad"
                        && entitlement["productId"]
                            .as_str()
                            .is_some_and(|id| ids.contains(id))
                    {
                        let account = field(&entitlement, "accountId")?;
                        if args["accountId"]
                            .as_str()
                            .is_some_and(|scope| scope != account)
                        {
                            continue;
                        }
                        let record = self.account(account)?;
                        if !super::active_bound_account(&record)
                            || !super::has_local_session(&self.inner.catalogue, &record)
                        {
                            continue;
                        }
                        matched.insert(field(&entitlement, "productId")?.to_owned());
                        accounts
                            .entry(account.into())
                            .or_default()
                            .insert(field(&entitlement, "productId")?.into());
                    }
                }
                ensure!(matched.len() == ids.len(), "AUTH_REQUIRED");
                for account in accounts.keys() {
                    self.account(account)?;
                }
                let running = self
                    .inner
                    .running
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
                ensure!(
                    accounts.keys().all(|account| !running.contains(account)),
                    "ACCOUNT_BUSY"
                );
                drop(running);
                let mut jobs = Vec::new();
                for (account, ids) in accounts {
                    let job = self.make_job(
                        "gumroadUpdate",
                        json!({"accountId":account,"productIds":ids}),
                    )?;
                    self.spawn(app.clone(), job.clone())?;
                    jobs.push(job["id"].clone());
                }
                Ok(json!({"jobId":jobs.first(),"jobIds":jobs}))
            }
            "cancel_sync" | "retry_sync" => {
                let id = field(&args, "jobId")?;
                let job = self
                    .inner
                    .catalogue
                    .get("job", id)?
                    .filter(|job| job["store"] == "Gumroad")
                    .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                if action == "cancel_sync" {
                    for attempt in 0..4 {
                        let mut batch = RecordBatch::new(&self.inner.catalogue);
                        let mut latest = batch.get("job", id)?.ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                        if !matches!(latest["state"].as_str(), Some("queued" | "running")) { break; }
                        latest["state"] = json!("cancelled");
                        latest["errorCode"] = json!("CANCELLED");
                        latest["completedAt"] = json!(now());
                        latest["progress"]["stage"] = json!("cancelled");
                        latest["updatedAt"] = json!(now());
                        batch.stage("job", id, latest)?;
                        if batch.try_commit()?.is_some() { break; }
                        ensure!(attempt < 3, "CONCURRENT_UPDATE");
                    }
                } else {
                    ensure!(
                        matches!(
                            job["state"].as_str(),
                            Some("failed" | "needsAction" | "cancelled")
                        ),
                        "JOB_BUSY"
                    );
                    ensure!(
                        !self
                            .inner
                            .running
                            .lock()
                            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                            .contains(field(&job, "accountId")?),
                        "ACCOUNT_BUSY"
                    );
                    if job["type"] == "gumroadLogin" {
                        self.clear_downloads(field(&job, "accountId")?, None);
                        session::window(
                            app,
                            &self.inner.catalogue.device_dir(),
                            StoreProvider::Gumroad,
                            field(&job, "accountId")?,
                            Url::parse("https://gumroad.com/login").unwrap(),
                            true,
                        )
                        .await?;
                    }
                    let job = self.requeue_job(&job)?;
                    self.spawn(app.clone(), job)?;
                }
                let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":id}));
                Ok(json!({"jobId":id}))
            }
            _ => bail!("UNKNOWN_STORE_ACTION"),
        }
    }
    fn make_job(&self, kind: &str, args: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let mut job = json!({"id":id,"type":kind,"store":"Gumroad","accountId":args["accountId"],"args":args,"state":"queued","progress":{"stage":"collecting","pages":0,"products":0},"createdAt":now(),"updatedAt":now(),"originDeviceId":self.inner.catalogue.device_id()});
        job["accountName"] = self.account(field(&args, "accountId")?)?["name"].clone();
        self.inner.catalogue.put("job", &id, job.clone())?;
        Ok(job)
    }
    fn spawn(&self, app: AppHandle, mut job: Value) -> Result<()> {
        let work = self.inner.catalogue.begin_catalogue_work()?;
        let account = field(&job, "accountId")?.to_owned();
        let id = field(&job, "id")?.to_owned();
        if !self
            .inner
            .running
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
            .insert(account.clone())
        {
            if job["type"] == "gumroadSync"
                && self.inner.catalogue.list("job")?.iter().any(|active| {
                    active["accountId"] == account
                        && active["type"] == "gumroadLogin"
                        && matches!(
                            active["state"].as_str(),
                            Some("queued" | "running" | "completed")
                        )
                })
            {
                let service = self.clone();
                tauri::async_runtime::spawn(async move {
                    let _work = work;
                    loop {
                        if let Err(error) = service.cancelled(&job) {
                            if let Err(error) = service.finish_job(&job, Some(&safe_code(&error))) {
                                eprintln!("Gumroad terminal write failed: {}", safe_code(&error));
                            }
                            let _ =
                                app.emit("nna:changed", json!({"scope":"catalogue","jobId":id}));
                            return;
                        }
                        if service
                            .inner
                            .running
                            .lock()
                            .map(|active| !active.contains(&account))
                            .unwrap_or(false)
                        {
                            if let Err(error) = service.spawn(app.clone(), job.clone()) {
                                if let Err(error) =
                                    service.finish_job(&job, Some(&safe_code(&error)))
                                {
                                    eprintln!(
                                        "Gumroad terminal write failed: {}",
                                        safe_code(&error)
                                    );
                                }
                            }
                            let _ =
                                app.emit("nna:changed", json!({"scope":"catalogue","jobId":id}));
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                });
                return Ok(());
            }
            job["state"] = json!("needsAction");
            job["errorCode"] = json!("ACCOUNT_BUSY");
            self.inner.catalogue.put("job", &id, job)?;
            bail!("ACCOUNT_BUSY");
        }
        let service = self.clone();
        tauri::async_runtime::spawn(async move {
            let _work = work;
            let _execution = if job["type"] == "gumroadLogin" { None } else { Some(service.inner.execution.lock().await) };
            let result = service.execute(&app, &mut job).await;
            let error = result.as_ref().err().map(safe_code);
            if let Err(error) = service.finish_job(&job, error.as_deref()) {
                eprintln!("Gumroad terminal write failed: {}", safe_code(&error));
                let _ = app.emit(
                    "nna:changed",
                    json!({"scope":"jobs","jobId":id,"errorCode":"TERMINAL_WRITE_FAILED"}),
                );
            } else if error.as_deref() == Some("AUTH_REQUIRED") {
                let _ = app.emit("nna:changed", json!({"scope":"booth"}));
            }
            if job["type"] == "gumroadLogin" {
                let verified = service
                    .inner
                    .catalogue
                    .get("job", &id)
                    .ok()
                    .flatten()
                    .is_some_and(|terminal| terminal["state"] == "completed");
                if let Err(error) =
                    session::finish_login(&app, StoreProvider::Gumroad, &account, verified).await
                {
                    eprintln!("Gumroad login window close failed: {}", safe_code(&error));
                }
                if let Err(error) = session::discard_unverified_account(&service.inner.catalogue, StoreProvider::Gumroad, &account) {
                    eprintln!("Gumroad login cleanup failed: {}", safe_code(&error));
                }
                let _ = app.emit("nna:changed", if verified {
                    json!({"scope":"booth","sessionChanged":true})
                } else {
                    json!({"scope":"booth"})
                });
            }
            if let Ok(mut running) = service.inner.running.lock() {
                running.remove(&account);
            }
            let scope = if matches!(job["type"].as_str(), Some("gumroadSync" | "gumroadUpdate")) {
                "catalogue"
            } else {
                "jobs"
            };
            let _ = app.emit("nna:changed", json!({"scope":scope,"jobId":id}));
        });
        Ok(())
    }
    fn requeue_job(&self, previous: &Value) -> Result<Value> {
        let id = field(previous, "id")?;
        ensure!(matches!(previous["state"].as_str(), Some("failed" | "needsAction" | "cancelled")), "JOB_BUSY");
        let mut job = previous.clone();
        crate::support::reset_attempt(&mut job);
        job["state"] = json!("queued");
        job["errorCode"] = Value::Null;
        job["completedAt"] = Value::Null;
        job["updatedAt"] = json!(now());
        job["originDeviceId"] = json!(self.inner.catalogue.device_id());
        ensure!(self.inner.catalogue.put_many_if_unchanged(
            vec![("job".into(), id.into(), job.clone())],
            vec![("job".into(), id.into(), Some(previous.clone()))],
        )?, "JOB_BUSY");
        Ok(job)
    }
    fn finish_job(&self, job: &Value, error: Option<&str>) -> Result<()> {
        let id = field(job, "id")?;
        if error == Some("AUTH_REQUIRED") && self.inner.catalogue.library_root().is_some() {
            session::forget(
                &self.inner.catalogue.device_dir(),
                StoreProvider::Gumroad,
                field(job, "accountId")?,
            )?;
        }
        for _ in 0..3 {
            let mut batch = RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", id)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            let error = if current["state"] == "cancelled" {
                Some("CANCELLED")
            } else {
                error
            };
            let mut terminal = job.clone();
            terminal["supportLastStage"] = json!(crate::support::operation_stage(job["progress"]["stage"].as_str().unwrap_or("")));
            terminal["state"] = json!(match error {
                None => "completed",
                Some("CANCELLED") => "cancelled",
                Some(_) => "needsAction",
            });
            terminal["errorCode"] = json!(error);
            terminal["updatedAt"] = json!(now());
            terminal["completedAt"] = json!(now());
            terminal["progress"]["stage"] = terminal["state"].clone();
            if job["type"] == "gumroadSync" {
                let account_id = field(job, "accountId")?;
                if let Some(mut account) = batch.get("account", account_id)? {
                    account["lastSyncAttemptAt"] = terminal["completedAt"].clone();
                    if error.is_none() {
                        account["lastSync"] = terminal["completedAt"].clone();
                        account["lastSyncAt"] = terminal["completedAt"].clone();
                    }
                    account["syncState"] = json!(if error.is_none() {
                        "complete"
                    } else {
                        "incomplete"
                    });
                    account["lastSyncErrorCode"] = json!(error);
                    batch.stage("account", account_id, account)?;
                }
                if error.is_some() {
                    let coverage_id = format!("gumroad:{account_id}:library");
                    if let Some(mut coverage) =
                        batch.get("source_coverage", &coverage_id)?.filter(|value| {
                            matches!(value["status"].as_str(), Some("running" | "collected"))
                        })
                    {
                        coverage["status"] = json!("incomplete");
                        coverage["coverageComplete"] = json!(false);
                        coverage["errorCode"] = json!(error);
                        coverage["completedAt"] = terminal["completedAt"].clone();
                        batch.stage("source_coverage", &coverage_id, coverage)?;
                    }
                }
            }
            if error == Some("AUTH_REQUIRED") && self.inner.catalogue.library_root().is_none() {
                let account_id = field(job, "accountId")?;
                if let Some(mut account) = batch.get("account", account_id)? {
                    account["state"] = json!("expired");
                    account["status"] = json!("expired");
                    batch.stage("account", account_id, account)?;
                }
            }
            batch.stage("job", id, terminal)?;
            if batch.try_commit()?.is_some() {
                if matches!(job["type"].as_str(), Some("gumroadSync" | "gumroadUpdate")) {
                    self.inner.catalogue.refresh_avatar_defaults()?;
                }
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    fn publish_journals(
        &self,
        app: &AppHandle,
        job: &mut Value,
        account: &str,
        checkpoint: &mut Value,
        known_purchases: &BTreeMap<String, String>,
    ) -> Result<()> {
        let (groups, blocked, unbound) = grouped_journals(
            &self.inner.catalogue,
            job,
            account,
            checkpoint,
            known_purchases,
        )?;
        let total = groups.len();
        for (product, records) in groups {
            if unbound || blocked.contains(&product) {
                continue;
            }
            self.cancelled(job)?;
            job["progress"]["stage"] = json!("publishing");
            job["progress"]["metadataTotal"] = json!(total);
            job["progress"]["metadataCompleted"] = json!(total);
            self.save_running_job(job)?;
            let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":job["id"]}));
            commit_gumroad_product(&self.inner.catalogue, job, &product, &records, checkpoint)?;
            self.clear_downloads(account, Some(&product));
            let _ = app.emit(
                "nna:changed",
                json!({"scope":"catalogue","productIds":[product]}),
            );
        }
        job["progress"]["stage"] = json!("finalizing");
        self.save_running_job(job)?;
        Ok(())
    }
    fn publish_complete_journal(
        &self,
        app: &AppHandle,
        job: &Value,
        checkpoint: &mut Value,
        journal_id: &str,
    ) -> Result<()> {
        for product in self.publish_complete_journal_records(job, checkpoint, journal_id)? {
            let _ = app.emit(
                "nna:changed",
                json!({"scope":"catalogue","productIds":[product]}),
            );
        }
        Ok(())
    }
    fn publish_complete_journal_records(
        &self,
        job: &Value,
        checkpoint: &mut Value,
        journal_id: &str,
    ) -> Result<Vec<String>> {
        let mut published = Vec::new();
        let journal = self
            .inner
            .catalogue
            .get("checkpoint", journal_id)?
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let account = field(&journal, "accountId")?;
        let mut single = checkpoint.clone();
        single["journalIds"] = json!([journal_id]);
        let (groups, blocked, unbound) = grouped_journals(
            &self.inner.catalogue,
            job,
            account,
            &single,
            &BTreeMap::new(),
        )?;
        if unbound {
            return Ok(published);
        }
        for (product, records) in groups {
            if blocked.contains(&product) {
                continue;
            }
            self.cancelled(job)?;
            commit_gumroad_product_mode(
                &self.inner.catalogue,
                job,
                &product,
                &records,
                checkpoint,
                false,
            )?;
            self.clear_downloads(account, Some(&product));
            published.push(product);
        }
        Ok(published)
    }
    fn consume_complete_observation(
        &self,
        job: &Value,
        account: &str,
        row: &LibraryPurchase,
        observation: &ContentObservation,
        public: Option<&gumroad::PublicProductObservation>,
        error: Option<&String>,
        unavailable: bool,
        checkpoint: &mut Value,
    ) -> Result<Vec<String>> {
        if !observation.inventory_complete {
            return Ok(vec![]);
        }
        let id = observation.catalogue_product_id();
        let mut prepared = RecordBatch::new(&self.inner.catalogue);
        project(&mut prepared, account, row, observation)?;
        project_public(&mut prepared, observation, public)?;
        if error.is_some() || unavailable {
            let mut product = prepared.get("product", &id)?.unwrap();
            if let Some(error) = error {
                product["priceRefreshErrorCode"] = json!(error);
            }
            if unavailable {
                product["publicPageAvailability"] = json!("unavailable");
            }
            prepared.stage("product", &id, product)?;
        }
        let grant_journal = format!(
            "gumroad:{account}:{}:grant:{}",
            field(job, "id")?,
            fingerprint(&json!(row.purchase_id))
        );
        append_journal(checkpoint, &grant_journal)?;
        self.inner.catalogue.put("checkpoint",&grant_journal,json!({"id":grant_journal,"store":"Gumroad","accountId":account,"runId":job["id"],"source":"library","observations":[{"productId":id,"purchaseId":row.purchase_id,"complete":true}],"records":prepared.staged_records()}))?;
        self.publish_complete_journal_records(job, checkpoint, &grant_journal)
    }
    fn cancelled(&self, job: &Value) -> Result<()> {
        ensure!(
            self.inner
                .catalogue
                .get("job", field(job, "id")?)?
                .is_some_and(|row| row["state"] != "cancelled"),
            "CANCELLED"
        );
        Ok(())
    }
    fn save_running_job(&self, job: &Value) -> Result<()> {
        let id = field(job, "id")?;
        for _ in 0..3 {
            let current = self
                .inner
                .catalogue
                .get("job", id)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            ensure!(current["state"] != "cancelled", "CANCELLED");
            if self.inner.catalogue.put_many_if_unchanged(
                vec![("job".into(), id.into(), job.clone())],
                vec![("job".into(), id.into(), Some(current))],
            )? {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    async fn client(&self, app: &AppHandle, account: &str) -> Result<reqwest::Client> {
        Ok(self.client_with_jar(app, account).await?.0)
    }
    async fn client_with_jar(
        &self,
        app: &AppHandle,
        account: &str,
    ) -> Result<(reqwest::Client, Arc<reqwest::cookie::Jar>)> {
        let window = session::window(
            app,
            &self.inner.catalogue.device_dir(),
            StoreProvider::Gumroad,
            account,
            Url::parse("https://gumroad.com/library").unwrap(),
            false,
        )
        .await?;
        let jar = match session::jar(window.clone(), StoreProvider::Gumroad).await {
            Ok(jar) => jar,
            Err(error) if safe_code(&error) == "AUTH_REQUIRED" => {
                // A newly reopened hidden WebView may not have loaded its profile yet.
                tokio::time::sleep(Duration::from_secs(1)).await;
                session::jar(window, StoreProvider::Gumroad).await?
            }
            Err(error) => return Err(error),
        };
        Ok((
            http::build_client(StoreProvider::Gumroad, jar.clone())?,
            jar,
        ))
    }
    async fn public_product(
        &self,
        client: &reqwest::Client,
        url: &Url,
        product_id: &str,
    ) -> Result<Option<gumroad::PublicProductObservation>> {
        let mut url = url.clone();
        url.set_path(&format!("{}.json", url.path().trim_end_matches('/')));
        let (status, body) = self
            .inner
            .http
            .get_classified(client, url, http::RequestKind::Public)
            .await?;
        if matches!(status.as_u16(), 404 | 410) {
            return Ok(None);
        }
        ensure!(status.is_success(), "GUMROAD_PUBLIC_METADATA_UNAVAILABLE");
        let value: Value = serde_json::from_str(&body)
            .map_err(|_| anyhow::anyhow!("GUMROAD_PUBLIC_SCHEMA_UNSUPPORTED"))?;
        gumroad::parse_public_product(&value, Some(product_id)).map(Some)
    }
    async fn library(
        &self,
        client: &reqwest::Client,
        page: u64,
        archived: bool,
    ) -> Result<LibraryPage> {
        self.library_scope(client, page, archived, None).await
    }
    async fn library_scope(
        &self,
        client: &reqwest::Client,
        page: u64,
        archived: bool,
        bundle: Option<&str>,
    ) -> Result<LibraryPage> {
        let mut url = library_url(page, archived);
        if let Some(bundle) = bundle {
            url.query_pairs_mut().append_pair("bundles", bundle);
        }
        let (status, html) = self.inner.http.get(client, url.clone()).await?;
        ensure!(status.is_success(), "GUMROAD_LIBRARY_UNAVAILABLE");
        let page_data = gumroad::inertia_page(&html)?;
        ensure!(page_data["component"] == "Library/Index", "SOURCE_UNSUPPORTED");
        gumroad::parse_library_scope(&page_data["props"], &url, page, archived, bundle)
    }
    async fn content(
        &self,
        client: &reqwest::Client,
        row: &LibraryPurchase,
        expected_product: Option<&str>,
    ) -> Result<ContentObservation> {
        let url = row
            .content_route
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("GUMROAD_CONTENT_UNAVAILABLE"))?
            .url()
            .clone();
        let (status, html) = self
            .inner
            .http
            .get_classified(client, url.clone(), http::RequestKind::Content)
            .await?;
        ensure!(status.is_success(), "GUMROAD_CONTENT_UNAVAILABLE");
        let page = gumroad::inertia_page(&html)?;
        ensure!(
            page["component"] == "UrlRedirects/DownloadPage",
            "GUMROAD_CONTENT_UNAVAILABLE"
        );
        gumroad::parse_content_props(&page["props"], &url, &row.purchase_id, expected_product)
    }
    /// A bundle remains one product unit. Member identity is established by its parent listing
    /// and fresh bound content page; neither a generated ZIP nor a bearer token is identity.
    async fn expanded_content(
        &self,
        client: &reqwest::Client,
        row: &LibraryPurchase,
        expected_product: Option<&str>,
    ) -> Result<ContentObservation> {
        let mut parent = self.content(client, row, expected_product).await?;
        let ancestors = BTreeSet::from([parent.product_id.clone()]);
        let mut queue: VecDeque<_> = std::mem::take(&mut parent.bundles)
            .into_iter()
            .map(|member| (member, ancestors.clone()))
            .collect();
        let mut visited = 0usize;
        while let Some((member, mut ancestors)) = queue.pop_front() {
            visited += 1;
            ensure!(visited <= 10000, "GUMROAD_INVENTORY_TOO_LARGE");
            if ancestors.len() >= 32 || !ancestors.insert(member.product_id.clone()) {
                continue;
            }
            let Some(route) = member.content_route else {
                continue;
            };
            let url = route.url().clone();
            let result = async {
                let (status, html) = self
                    .inner
                    .http
                    .get_classified(client, url.clone(), http::RequestKind::Content)
                    .await?;
                ensure!(status.is_success(), "GUMROAD_CONTENT_UNAVAILABLE");
                let page = gumroad::inertia_page(&html)?;
                ensure!(
                    page["component"] == "UrlRedirects/DownloadPage",
                    "GUMROAD_CONTENT_UNAVAILABLE"
                );
                let purchase = field(&page["props"]["purchase"], "id")?;
                gumroad::parse_content_props(
                    &page["props"],
                    &url,
                    purchase,
                    Some(&member.product_id),
                )
            }
            .await;
            match result {
                Ok(mut child) => {
                    // Each listed member contributes one unresolved placeholder until bound.
                    parent.unsupported_items =
                        parent.unsupported_items.saturating_sub(1) + child.unsupported_items;
                    for nested in std::mem::take(&mut child.bundles) {
                        queue.push_back((nested, ancestors.clone()));
                    }
                    merge_member_files(&mut parent, child.files);
                }
                Err(error)
                    if matches!(
                        safe_code(&error).as_str(),
                        "AUTH_REQUIRED" | "PERMISSION_DENIED" | "RATE_LIMITED"
                    ) =>
                {
                    return Err(error)
                }
                Err(_) => {} // Preserve parent financial facts and previously verified files.
            }
        }
        parent.inventory_complete = parent.unsupported_items == 0;
        Ok(parent)
    }
    async fn authenticate(
        &self,
        app: &AppHandle,
        account: &str,
        client: &reqwest::Client,
        login_job: Option<&Value>,
    ) -> Result<LibraryPage> {
        self.clear_downloads(account, None);
        let page = self.library(client, 1, false).await?;
        let identity = fingerprint(&json!(["gumroad", page.buyer_identity]));
        let mut batch = RecordBatch::new(&self.inner.catalogue);
        let mut row = batch
            .get("account", account)?
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
        if let Some(job) = login_job {
            ensure!(
                session::existing_window(app, StoreProvider::Gumroad, account)?.is_some(),
                "CANCELLED"
            );
            ensure!(
                batch
                    .get("job", field(job, "id")?)?
                    .is_some_and(|job| matches!(job["state"].as_str(), Some("queued" | "running"))),
                "CANCELLED"
            );
        }
        if let Some(saved) = row["identityHash"].as_str() {
            ensure!(saved == identity, "ACCOUNT_IDENTITY_MISMATCH");
        }
        let unverified = row["identityVerified"] != true
            && row["authenticatedAt"].as_str().is_none_or(str::is_empty)
            && row["identityHash"].as_str().is_none_or(str::is_empty);
        if login_job.is_some() {
            let window = session::existing_window(app, StoreProvider::Gumroad, account)?
                .ok_or_else(|| anyhow::anyhow!("CANCELLED"))?;
            session::remember(window, &self.inner.catalogue.device_dir(), StoreProvider::Gumroad, account).await?;
            if session::existing_window(app, StoreProvider::Gumroad, account)?.is_none() {
                if unverified { session::forget(&self.inner.catalogue.device_dir(), StoreProvider::Gumroad, account)?; }
                bail!("CANCELLED");
            }
        }
        row["identityHash"] = json!(identity);
        row["identityVerified"] = json!(true);
        row["disconnected"] = json!(false);
        row["state"] = json!("connected");
        row["status"] = json!("connected");
        row["updatedAt"] = json!(now());
        row["authenticatedAt"] = json!(now());
        batch.stage("account", account, row)?;
        match batch.try_commit() {
            Ok(Some(_)) => {}
            result => {
                if login_job.is_some() && unverified { session::forget(&self.inner.catalogue.device_dir(), StoreProvider::Gumroad, account)?; }
                result?;
                bail!("CONCURRENT_UPDATE");
            }
        }
        let _ = app.emit("nna:changed", json!({"scope":"booth"}));
        Ok(page)
    }
    async fn bundle_members(
        &self,
        client: &reqwest::Client,
        bundle: &gumroad::LibraryBundle,
        buyer: &str,
        job: Option<&Value>,
    ) -> Result<BundleMembers> {
        let page_limit = job
            .and_then(|_| crate::verification::source_page_limit())
            .map(|v| v as u64);
        enumerate_bundle_members(buyer, page_limit, |index, archived| {
            async move {
                if let Some(job) = job {
                    self.cancelled(job)?;
                }
                self.library_scope(client, index, archived, Some(&bundle.product_id))
                    .await
            }
            .boxed()
        })
        .await
    }
    async fn assemble_bundle(
        &self,
        client: &reqwest::Client,
        bundle: &gumroad::LibraryBundle,
        buyer: &str,
        members: BundleMembers,
        reuse: &mut BTreeMap<String, Option<ContentObservation>>,
        job: Option<&Value>,
    ) -> Result<(LibraryPurchase, ContentObservation, u64)> {
        let missing: Vec<_> = members
            .purchases
            .iter()
            .filter(|row| !row.excluded_non_asset() && !reuse.contains_key(&row.purchase_id))
            .cloned()
            .collect();
        let mut reads = futures_util::stream::iter(missing.into_iter().map(|row| async move {
            if let Some(job) = job {
                self.cancelled(job)?;
            }
            Ok::<_, anyhow::Error>((
                row.purchase_id.clone(),
                self.expanded_content(client, &row, None).await,
            ))
        }))
        .buffer_unordered(2);
        while let Some(result) = reads.next().await {
            let (purchase, result) = result?;
            match result {
                Ok(child) => {
                    reuse.insert(purchase, Some(child));
                }
                Err(error)
                    if matches!(
                        safe_code(&error).as_str(),
                        "AUTH_REQUIRED"
                            | "PERMISSION_DENIED"
                            | "RATE_LIMITED"
                            | "ACCOUNT_IDENTITY_MISMATCH"
                    ) =>
                {
                    return Err(error)
                }
                Err(_) => {
                    reuse.insert(purchase, None);
                }
            }
        }
        // A single fresh scoped head check per tab, after all member content.
        for (tab, archived) in [false, true].into_iter().enumerate() {
            if let Some(job) = job {
                self.cancelled(job)?;
            }
            let head = self
                .library_scope(client, 1, archived, Some(&bundle.product_id))
                .await?;
            ensure!(head.buyer_identity == buyer, "ACCOUNT_IDENTITY_MISMATCH");
            ensure!(
                signature(&head) == members.head_signatures[tab],
                "GUMROAD_LIBRARY_CHANGED"
            );
        }
        let (card, parent) = assemble_observed_bundle(bundle, &members, reuse);
        Ok((card, parent, members.pages))
    }
    async fn bundle_content(
        &self,
        client: &reqwest::Client,
        bundle: &gumroad::LibraryBundle,
        buyer: &str,
        reuse: &mut BTreeMap<String, Option<ContentObservation>>,
        job: Option<&Value>,
    ) -> Result<(LibraryPurchase, ContentObservation, u64)> {
        let members = self.bundle_members(client, bundle, buyer, job).await?;
        self.assemble_bundle(client, bundle, buyer, members, reuse, job)
            .await
    }
    async fn execute(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        let account = field(job, "accountId")?.to_owned();
        let existing = self.account(&account)?;
        job["state"] = json!("running");
        self.save_running_job(job)?;
        if job["type"] == "gumroadLogin" {
            let started = Instant::now();
            loop {
                self.cancelled(job)?;
                let window = session::existing_window(app, StoreProvider::Gumroad, &account)?
                    .ok_or_else(|| anyhow::anyhow!("CANCELLED"))?;
                let result = async {
                    let jar = session::jar(window, StoreProvider::Gumroad).await?;
                    let client = http::build_client(StoreProvider::Gumroad, jar)?;
                    self.authenticate(app, &account, &client, Some(job)).await
                }
                .await;
                match result {
                    Ok(_) => return Ok(()),
                    Err(error) if safe_code(&error) == "AUTH_REQUIRED" => {}
                    Err(error) => return Err(error),
                }
                ensure!(started.elapsed() < Duration::from_secs(600), "AUTH_TIMEOUT");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        ensure!(
            existing["identityVerified"] == true && existing["disconnected"] != true,
            "AUTH_REQUIRED"
        );
        let client = self.client(app, &account).await?;
        // The identity-verified response is also the first library page.
        let first = self.authenticate(app, &account, &client, None).await?;
        self.collect(app, job, &account, &client, first).await
    }
    async fn collect(
        &self,
        app: &AppHandle,
        job: &mut Value,
        account: &str,
        client: &reqwest::Client,
        first: LibraryPage,
    ) -> Result<()> {
        let metrics_before = self.inner.http.metrics();
        let classified_before = self.inner.http.classified_metrics();
        let page_limit = crate::verification::source_page_limit().map(|v| v as u64);
        let mut verification_capped = false;
        if let Some(limit) = page_limit {
            job["verification"] = json!({"pageLimit":limit,"sourcePages":{"unarchived":0,"archived":0},"bundlePages":0,"capped":false});
        }
        let selected: Option<BTreeSet<String>> = job["args"]["productIds"].as_array().map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        let mut selected_purchases: BTreeSet<String> = if let Some(ids) = &selected {
            self.inner
                .catalogue
                .list("acquisition")?
                .into_iter()
                .filter(|row| {
                    row["accountId"] == account
                        && row["store"] == "Gumroad"
                        && row["productId"].as_str().is_some_and(|id| ids.contains(id))
                })
                .filter_map(|row| row["purchaseId"].as_str().map(str::to_owned))
                .collect()
        } else {
            BTreeSet::new()
        };
        let selected_bundles: BTreeSet<String> = self
            .inner
            .catalogue
            .list("acquisition")?
            .into_iter()
            .filter(|row| {
                row["accountId"] == account
                    && row["store"] == "Gumroad"
                    && selected.as_ref().is_some_and(|ids| {
                        row["productId"].as_str().is_some_and(|id| ids.contains(id))
                    })
            })
            .filter_map(|row| row["sourceBundleId"].as_str().map(str::to_owned))
            .collect();
        ensure!(
            selected.is_none() || !selected_purchases.is_empty() || !selected_bundles.is_empty(),
            "ENTITLEMENT_UNAVAILABLE"
        );
        let heads = [first, self.library(client, 1, true).await?];
        let buyer_identity = heads[0].buyer_identity.clone();
        ensure!(
            heads[1].buyer_identity == buyer_identity,
            "ACCOUNT_IDENTITY_MISMATCH"
        );
        let signatures = [signature(&heads[0]), signature(&heads[1])];
        let mut owned_bundles = BTreeMap::new();
        for bundle in heads.iter().flat_map(|head| &head.bundles) {
            owned_bundles
                .entry(bundle.product_id.clone())
                .or_insert_with(|| bundle.clone());
        }
        ensure!(
            selected_bundles
                .iter()
                .all(|id| owned_bundles.contains_key(id)),
            "GUMROAD_BUNDLE_OWNERSHIP_UNPROVEN"
        );
        let checkpoint_id = if selected.is_some() {
            format!("gumroad:{account}:selected:{}", field(job, "id")?)
        } else {
            format!("gumroad:{account}:library")
        };
        let saved = self.inner.catalogue.get("checkpoint", &checkpoint_id)?;
        let mut checkpoint = saved.filter(|row| selected.is_none() && row["publicationVersion"] == 1 && row["jobId"] == job["id"] && row["headSignatures"] == json!(signatures) && row["unresolvedRows"].as_u64() == Some(0))
            .unwrap_or_else(|| json!({"id":checkpoint_id,"publicationVersion":1,"journalIds":[],"publishedProducts":[],"store":"Gumroad","accountId":account,"jobId":job["id"],"headSignatures":signatures,"nextPages":[1,1],"seenPurchaseIds":[],"productIds":[],"unresolvedRows":0,"pages":0,"complete":false}));
        self.inner.catalogue.prune_store_checkpoints("Gumroad", account)?;
        let mut seen: BTreeSet<String> = checkpoint["seenPurchaseIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut products: BTreeSet<String> = checkpoint["productIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut unresolved = checkpoint["unresolvedRows"].as_u64().unwrap_or(0);
        let mut found = BTreeSet::new();
        let known_purchases: BTreeMap<String, String> = self
            .inner
            .catalogue
            .list("acquisition")?
            .into_iter()
            .filter(|row| row["store"] == "Gumroad" && row["accountId"] == account)
            .filter_map(|row| {
                Some((
                    row["purchaseId"].as_str()?.to_owned(),
                    row["productId"].as_str()?.to_owned(),
                ))
            })
            .collect();
        let public_client = http::build_client(
            StoreProvider::Gumroad,
            Arc::new(reqwest::cookie::Jar::default()),
        )?;
        let mut public_products = BTreeMap::new();
        let public_reads: Mutex<BTreeMap<String, PublicRead<'_>>> = Mutex::new(BTreeMap::new());
        let mut public_errors = BTreeMap::new();
        let mut public_unavailable = BTreeSet::new();
        let metadata_total = heads.iter().map(|head| head.count).sum::<u64>()
            + owned_bundles
                .values()
                .filter(|bundle| {
                    selected.is_none() || selected_bundles.contains(&bundle.product_id)
                })
                .count() as u64;
        let mut bundle_completed: BTreeSet<String> = checkpoint["completedBundles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut bundled_purchases: BTreeSet<String> = checkpoint["bundlePurchaseIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut bundle_memberships = BTreeMap::new();
        for bundle in owned_bundles
            .values()
            .filter(|bundle| selected.is_none() || selected_bundles.contains(&bundle.product_id))
        {
            if bundle_completed.contains(&bundle.product_id) {
                continue;
            }
            let members = self
                .bundle_members(client, bundle, &buyer_identity, Some(job))
                .await?;
            verification_capped |= !members.complete;
            if page_limit.is_some() {
                job["verification"]["bundlePages"] =
                    json!(job["verification"]["bundlePages"].as_u64().unwrap_or(0) + members.pages);
            }
            bundled_purchases.extend(members.purchases.iter().map(|row| row.purchase_id.clone()));
            checkpoint["bundlePurchaseIds"] = json!(bundled_purchases);
            checkpoint["pages"] = json!(checkpoint["pages"].as_u64().unwrap_or(0) + members.pages);
            bundle_memberships.insert(bundle.product_id.clone(), members);
            job["progress"] = json!({"stage":"collecting","pages":checkpoint["pages"],"products":products.len(),"published":checkpoint["publishedProducts"].as_array().map_or(0,Vec::len),"metadataCompleted":seen.len()+bundle_completed.len(),"metadataTotal":metadata_total,"unresolvedRows":unresolved});
            self.set_collection_metrics(job, metrics_before, classified_before);
            self.save_running_job(job)?;
            let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":job["id"]}));
        }
        selected_purchases.retain(|id| !bundled_purchases.contains(id));
        for (tab, head) in heads.into_iter().enumerate() {
            let archived = tab == 1;
            let pages = head.pages;
            let observed_pages = page_limit.map_or(pages, |limit| pages.min(limit));
            verification_capped |= observed_pages != pages;
            let count = head.count;
            let mut first = Some(head);
            for page_index in checkpoint["nextPages"][tab].as_u64().unwrap_or(1)..=observed_pages {
                self.cancelled(job)?;
                let page = if page_index == 1 {
                    // The initial response has already been validated.
                    first.take().unwrap()
                } else {
                    self.library(client, page_index, archived).await?
                };
                job["progress"] = json!({"stage":"collecting","pages":checkpoint["pages"],"products":products.len(),"published":checkpoint["publishedProducts"].as_array().map_or(0,Vec::len),"metadataCompleted":seen.len()+bundle_completed.len(),"metadataTotal":metadata_total,"unresolvedRows":unresolved});
                self.save_running_job(job)?;
                let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":job["id"]}));
                ensure!(
                    page.pages == pages && page.count == count,
                    "GUMROAD_LIBRARY_CHANGED"
                );
                ensure!(
                    page.buyer_identity == buyer_identity,
                    "ACCOUNT_IDENTITY_MISMATCH"
                );
                let mut observations = Vec::new();
                let mut page_unresolved = 0;
                let mut journal = Vec::new();
                let completed_before_page = seen.len() + bundle_completed.len();
                let mut page_processed = 0usize;
                // Poll two complete content/public acquisitions together. The consumer
                // only stages finished results, retaining page order and grant grouping.
                let mut eligible = Vec::new();
                for row in page.purchases {
                    ensure!(
                        seen.insert(row.purchase_id.clone()),
                        "GUMROAD_PURCHASE_AMBIGUOUS"
                    );
                    if !parentless_purchase(&row, &bundled_purchases) {
                        page_processed += 1;
                        continue;
                    }
                    if selected.as_ref().is_some_and(|ids| {
                        known_purchases
                            .get(&row.purchase_id)
                            .is_some_and(|id| !ids.contains(id))
                    }) {
                        page_processed += 1;
                        continue;
                    }
                    found.insert(row.purchase_id.clone());
                    eligible.push(row);
                }
                let content_job = job.clone();
                let mut contents = futures_util::stream::iter(eligible.into_iter().map(|row| {
                    let content_job = &content_job;
                    let public_reads = &public_reads;
                    let public_client = &public_client;
                    let selected = &selected;
                    async move {
                        self.cancelled(content_job)?;
                        let content = self.expanded_content(client, &row, None).await;
                        let public = if let Ok(observation) = &content {
                            if selected.as_ref().is_none_or(|ids| {
                                ids.contains(&observation.catalogue_product_id())
                            })
                            {
                                if let Some(url) = &observation.public_product_url {
                                    let future = shared_public_read(
                                        public_reads,
                                        &observation.product_id,
                                        || {
                                            let id = observation.product_id.clone();
                                            let url = url.clone();
                                            async move {
                                                self.public_product(public_client, &url, &id)
                                                    .await
                                                    .map_err(|error| safe_code(&error))
                                            }
                                            .boxed()
                                            .shared()
                                        },
                                    )?;
                                    Some(future.await)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        self.cancelled(content_job)?;
                        Ok::<_, anyhow::Error>((row, content, public))
                    }
                }))
                .buffer_unordered(2);
                while let Some(result) = contents.next().await {
                    let (row, content, public) = result?;
                    page_processed += 1;
                    self.cancelled(job)?;
                    match content {
                        Ok(observation) => {
                            let id = observation.catalogue_product_id();
                            if selected.as_ref().is_some_and(|ids| !ids.contains(&id)) {
                                continue;
                            }
                            if let Some(public) = public {
                                let public = match public {
                                    Ok(Some(public)) => Some(public),
                                    Ok(None) => {
                                        public_unavailable.insert(observation.product_id.clone());
                                        None
                                    }
                                    Err(code) => {
                                        public_errors.insert(observation.product_id.clone(), code);
                                        None
                                    }
                                };
                                public_products.insert(observation.product_id.clone(), public);
                            }
                            ensure!(
                                selected.as_ref().is_none_or(|ids| ids.contains(&id)),
                                "GUMROAD_PRODUCT_BINDING_UNPROVEN"
                            );
                            let complete = observation.inventory_complete;
                            if !complete {
                                page_unresolved += 1;
                            }
                            products.insert(id.clone());
                            journal.push(json!({"purchaseId":row.purchase_id,"productId":id,"complete":complete,"errorCode":public_errors.get(&observation.product_id).filter(|_|needs_public_quote(&observation)).cloned().or_else(|| (!observation.inventory_complete).then(|| "GUMROAD_INVENTORY_UNPROVEN".into()))}));
                            // Publish this complete acquisition while other content
                            // futures and later listing pages remain outstanding.
                            if complete {
                                let published = self.consume_complete_observation(
                                    job,
                                    account,
                                    &row,
                                    &observation,
                                    public_products
                                        .get(&observation.product_id)
                                        .and_then(Option::as_ref),
                                    public_errors
                                        .get(&observation.product_id)
                                        .filter(|_| needs_public_quote(&observation)),
                                    needs_public_quote(&observation)
                                        && public_unavailable.contains(&observation.product_id),
                                    &mut checkpoint,
                                )?;
                                for product in published {
                                    let _ = app.emit(
                                        "nna:changed",
                                        json!({"scope":"catalogue","productIds":[product]}),
                                    );
                                }
                            }
                            observations.push((row, observation));
                        }
                        Err(error) => {
                            let code = safe_code(&error);
                            if matches!(
                                code.as_str(),
                                "AUTH_REQUIRED" | "PERMISSION_DENIED" | "RATE_LIMITED"
                            ) {
                                return Err(error);
                            }
                            page_unresolved += 1;
                            journal.push(json!({"purchaseId":row.purchase_id,"complete":false,"errorCode":code}));
                        }
                    }
                    job["progress"]["stage"] = json!("enriching");
                    job["progress"]["metadataCompleted"] =
                        json!(completed_before_page + page_processed);
                    job["progress"]["published"] = json!(checkpoint["publishedProducts"]
                        .as_array()
                        .map_or(0, Vec::len));
                    self.save_running_job(job)?;
                    let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":job["id"]}));
                }
                unresolved += page_unresolved;
                checkpoint["nextPages"][tab] = json!(page_index + 1);
                checkpoint["seenPurchaseIds"] = json!(seen);
                checkpoint["productIds"] = json!(products);
                checkpoint["unresolvedRows"] = json!(unresolved);
                checkpoint["pages"] = json!(checkpoint["pages"].as_u64().unwrap_or(0) + 1);
                if page_limit.is_some() {
                    job["verification"]["sourcePages"]
                        [if archived { "archived" } else { "unarchived" }] = json!(page_index);
                }
                checkpoint["updatedAt"] = json!(now());
                let journal_id =
                    format!("gumroad:{account}:{}:{tab}:{page_index}", field(job, "id")?);
                append_journal(&mut checkpoint, &journal_id)?;
                let mut committed = false;
                for _ in 0..3 {
                    self.cancelled(job)?;
                    let mut batch = RecordBatch::new(&self.inner.catalogue);
                    let mut prepared = RecordBatch::new(&self.inner.catalogue);
                    for (row, observation) in &observations {
                        project(&mut prepared, account, row, observation)?;
                        project_public(
                            &mut prepared,
                            observation,
                            public_products
                                .get(&observation.product_id)
                                .and_then(Option::as_ref),
                        )?;
                        if let Some(error) = public_errors
                            .get(&observation.product_id)
                            .filter(|_| needs_public_quote(observation))
                        {
                            let id = observation.catalogue_product_id();
                            let mut product = prepared.get("product", &id)?.unwrap();
                            product["priceRefreshErrorCode"] = json!(error);
                            prepared.stage("product", &id, product)?;
                        }
                        if needs_public_quote(observation)
                            && public_unavailable.contains(&observation.product_id)
                        {
                            let id = observation.catalogue_product_id();
                            let mut product = prepared.get("product", &id)?.unwrap();
                            product["publicPageAvailability"] = json!("unavailable");
                            prepared.stage("product", &id, product)?;
                        }
                    }
                    batch.stage("checkpoint", &journal_id, json!({"id":journal_id,"store":"Gumroad","accountId":account,"runId":job["id"],"source":"library","archived":archived,"page":page_index,"observations":journal,"records":prepared.staged_records()}))?;
                    if selected.is_none() {
                        batch.stage("checkpoint", &checkpoint_id, checkpoint.clone())?;
                        batch.stage("source_coverage", &checkpoint_id, json!({"id":checkpoint_id,"store":"Gumroad","accountId":account,"source":"library","status":"running","coverageComplete":false,"pages":checkpoint["pages"],"unresolvedRows":unresolved,"updatedAt":now()}))?;
                    }
                    if batch.try_commit()?.is_some() {
                        committed = true;
                        break;
                    }
                }
                ensure!(committed, "CONCURRENT_UPDATE");
                job["progress"] = json!({"stage":"enriching","pages":checkpoint["pages"],"products":products.len(),"published":checkpoint["publishedProducts"].as_array().map_or(0,Vec::len),"metadataCompleted":seen.len()+bundle_completed.len(),"metadataTotal":metadata_total,"unresolvedRows":unresolved});
                job["updatedAt"] = json!(now());
                self.set_collection_metrics(job, metrics_before, classified_before);
                self.save_running_job(job)?;
                let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":job["id"]}));
            }
        }
        let mut reused_children = BTreeMap::new();
        for bundle in owned_bundles
            .values()
            .filter(|bundle| selected.is_none() || selected_bundles.contains(&bundle.product_id))
        {
            if bundle_completed.contains(&bundle.product_id) {
                continue;
            }
            let members = bundle_memberships
                .remove(&bundle.product_id)
                .ok_or_else(|| anyhow::anyhow!("GUMROAD_BUNDLE_OWNERSHIP_UNPROVEN"))?;
            let (card, observation, pages) = self
                .assemble_bundle(
                    client,
                    bundle,
                    &buyer_identity,
                    members,
                    &mut reused_children,
                    Some(job),
                )
                .await?;
            let id = observation.catalogue_product_id();
            bundled_purchases.extend(observation.member_purchase_ids.iter().cloned());
            products.insert(id.clone());
            unresolved += u64::from(!observation.inventory_complete);
            bundle_completed.insert(bundle.product_id.clone());
            checkpoint["completedBundles"] = json!(bundle_completed);
            checkpoint["bundlePurchaseIds"] = json!(bundled_purchases);
            checkpoint["productIds"] = json!(products);
            checkpoint["unresolvedRows"] = json!(unresolved);

            checkpoint["updatedAt"] = json!(now());
            let journal_id = format!(
                "gumroad:{account}:{}:bundle:{}",
                field(job, "id")?,
                fingerprint(&json!(bundle.product_id))
            );
            append_journal(&mut checkpoint, &journal_id)?;
            let mut committed = false;
            for _ in 0..3 {
                self.cancelled(job)?;
                let mut batch = RecordBatch::new(&self.inner.catalogue);
                let mut prepared = RecordBatch::new(&self.inner.catalogue);
                project(&mut prepared, account, &card, &observation)?;
                batch.stage("checkpoint", &journal_id, json!({"id":journal_id,"store":"Gumroad","accountId":account,"runId":job["id"],"source":"library","sourceBundleId":bundle.product_id,"purchaseIds":observation.member_purchase_ids,"complete":observation.inventory_complete,"pages":pages,"observations":[{"productId":id,"complete":observation.inventory_complete}],"records":prepared.staged_records()}))?;
                if selected.is_none() {
                    batch.stage("checkpoint", &checkpoint_id, checkpoint.clone())?;
                    batch.stage("source_coverage", &checkpoint_id, json!({"id":checkpoint_id,"store":"Gumroad","accountId":account,"source":"library","status":"running","coverageComplete":false,"pages":checkpoint["pages"],"unresolvedRows":unresolved,"updatedAt":now()}))?;
                }
                if batch.try_commit()?.is_some() {
                    committed = true;
                    break;
                }
            }
            ensure!(committed, "CONCURRENT_UPDATE");
            self.publish_complete_journal(app, job, &mut checkpoint, &journal_id)?;
            job["progress"] = json!({"stage":"enriching","pages":checkpoint["pages"],"products":products.len(),"published":checkpoint["publishedProducts"].as_array().map_or(0,Vec::len),"metadataCompleted":seen.len()+bundle_completed.len(),"metadataTotal":metadata_total,"unresolvedRows":unresolved});
            self.set_collection_metrics(job, metrics_before, classified_before);
            self.save_running_job(job)?;
            let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":job["id"]}));
        }
        if selected.is_some() {
            ensure!(selected_purchases.is_subset(&found), "SYNC_INCOMPLETE");
        }
        for (tab, expected) in signatures.iter().enumerate() {
            ensure!(
                &signature(&self.library(client, 1, tab == 1).await?) == expected,
                "GUMROAD_LIBRARY_CHANGED"
            );
        }
        self.set_collection_metrics(job, metrics_before, classified_before);
        if verification_capped {
            // Observed complete grants were already published additively. A
            // verification stop is not source closure or a replacement manifest.
            job["verification"]["capped"] = json!(true);
            self.save_running_job(job)?;
            bail!("VERIFICATION_PAGE_LIMIT");
        }
        self.publish_journals(app, job, account, &mut checkpoint, &known_purchases)?;
        if selected.is_some() {
            ensure!(unresolved == 0, "SYNC_INCOMPLETE");
            return Ok(());
        }
        let coverage_id = format!("gumroad:{account}:library");
        checkpoint["complete"] = json!(unresolved == 0);
        let coverage = json!({"id":coverage_id,"store":"Gumroad","accountId":account,"source":"library","coverageComplete":unresolved == 0,"status":if unresolved == 0 { "complete" } else { "incomplete" },"terminalSeen":true,"pages":checkpoint["pages"],"unresolvedRows":unresolved,"productCount":products.len(),"incrementalBoundary":"fullTraversalWhenOrderingUnproven","completedAt":now()});
        let mut finalized = false;
        for _ in 0..3 {
            let mut batch = RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", field(job, "id")?)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            ensure!(current["state"] != "cancelled", "CANCELLED");
            batch.stage("checkpoint", &checkpoint_id, checkpoint.clone())?;
            batch.stage("source_coverage", &coverage_id, coverage.clone())?;
            if batch.try_commit()?.is_some() {
                finalized = true;
                break;
            }
        }
        ensure!(finalized, "CONCURRENT_UPDATE");
        ensure!(unresolved == 0, "SYNC_INCOMPLETE");
        Ok(())
    }
    fn set_collection_metrics(
        &self,
        job: &mut Value,
        before: [u64; 3],
        classified_before: [[u64; 9]; 2],
    ) {
        let metrics = self.inner.http.metrics();
        let by_kind = self.inner.http.classified_since(classified_before);
        job["requestMetrics"] = json!({"requests":metrics[0]-before[0],"pacingWaitMs":metrics[1]-before[1],"responseWaitMs":metrics[2]-before[2],"activeRequestLimit":2,"startSpacingMs":self.inner.http.start_spacing_ms(),"byKind":by_kind,"scope":"providerCollectionInterval"});
        job["progress"]["requestsCompleted"] = json!(metrics[0] - before[0]);
        job["progress"]["detailsCompleted"] = by_kind["content"]["requests"].clone();
    }
    fn clear_downloads(&self, account: &str, product: Option<&str>) {
        if let Ok(mut cache) = self.inner.downloads.lock() {
            if let Some(product) = product {
                if let Some(session) = cache.get_mut(account) {
                    session.manifests.remove(product);
                    session.purchases.clear();
                }
            } else {
                cache.remove(account);
            }
        }
    }
    fn cached_download(
        &self,
        account: &str,
        product: &str,
        file: &Value,
    ) -> Result<Option<(DownloadSession, gumroad::FileObservation)>> {
        let record = self.account(account)?;
        let cache = self
            .inner
            .downloads
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        Ok(cache
            .get(account)
            .filter(|session| session.valid(&record, Instant::now()))
            .and_then(|session| {
                session
                    .manifests
                    .get(product)
                    .and_then(|manifest| {
                        manifest.offered(
                            account,
                            product.strip_prefix("gumroad:").unwrap_or(""),
                            file,
                            Instant::now(),
                        )
                    })
                    .map(|file| (session.snapshot(), file.clone()))
            }))
    }
    async fn fill_download(
        &self,
        app: &AppHandle,
        account: &str,
        product: &str,
        file: &Value,
    ) -> Result<(DownloadSession, gumroad::FileObservation)> {
        // Only cache misses wait here; map locks never cover native cookies or HTTP.
        let _fill = self.inner.download_fill.lock().await;
        if let Some(cached) = self.cached_download(account, product, file)? {
            return Ok(cached);
        }
        let record = self.account(account)?;
        ensure!(super::active_bound_account(&record), "AUTH_REQUIRED");
        let identity = field(&record, "identityHash")?.to_owned();
        let cached_session = {
            let cache = self
                .inner
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            cache
                .get(account)
                .filter(|session| session.valid(&record, Instant::now()))
                .map(DownloadSession::snapshot)
        };
        let mut session = if let Some(session) = cached_session {
            session
        } else {
            let (client, jar) = self.client_with_jar(app, account).await?;
            self.authenticate(app, account, &client, None).await?;
            let record = self.account(account)?;
            ensure!(
                super::active_bound_account(&record) && record["identityHash"] == identity,
                "ACCOUNT_IDENTITY_MISMATCH"
            );
            DownloadSession {
                client,
                jar,
                identity,
                verified_at: Instant::now(),
                manifests: BTreeMap::new(),
                purchases: BTreeMap::new(),
            }
        };
        let source_product = product
            .strip_prefix("gumroad:")
            .ok_or_else(|| anyhow::anyhow!("PRODUCT_UNAVAILABLE"))?;
        let purchase_ids: BTreeSet<_> = file["purchaseIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        ensure!(!purchase_ids.is_empty(), "ENTITLEMENT_UNAVAILABLE");
        if let Some(bundle) = file["sourceBundleId"].as_str() {
            ensure!(bundle == source_product, "GUMROAD_PRODUCT_BINDING_UNPROVEN");
            let head = self.library(&session.client, 1, false).await?;
            ensure!(
                fingerprint(&json!(["gumroad", head.buyer_identity])) == session.identity,
                "ACCOUNT_IDENTITY_MISMATCH"
            );
            let owned = head
                .bundles
                .into_iter()
                .find(|owned| owned.product_id == bundle)
                .ok_or_else(|| anyhow::anyhow!("GUMROAD_BUNDLE_OWNERSHIP_UNPROVEN"))?;
            let (_, observation, _) = self
                .bundle_content(
                    &session.client,
                    &owned,
                    &head.buyer_identity,
                    &mut BTreeMap::new(),
                    None,
                )
                .await?;
            session.verified_at = Instant::now();
            return self
                .retain_download(account, product, file, &session, &observation)?
                .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"));
        }
        let indexed = {
            let cache = self
                .inner
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            cache
                .get(account)
                .map(|session| session.indexed_purchases(&record, &purchase_ids, Instant::now()))
                .unwrap_or_default()
        };
        let mut attempted = BTreeSet::new();
        for row in indexed {
            match self
                .expanded_content(&session.client, &row, Some(source_product))
                .await
            {
                Ok(observation) => {
                    attempted.insert(row.purchase_id.clone());
                    if let Some(ticket) =
                        self.retain_download(account, product, file, &session, &observation)?
                    {
                        return Ok(ticket);
                    }
                }
                Err(error)
                    if matches!(
                        safe_code(&error).as_str(),
                        "AUTH_REQUIRED"
                            | "PERMISSION_DENIED"
                            | "RATE_LIMITED"
                            | "ACCOUNT_IDENTITY_MISMATCH"
                    ) =>
                {
                    return Err(error)
                }
                Err(_) => {
                    self.clear_downloads(account, Some(product));
                }
            }
        }
        for archived in [false, true] {
            let first = self.library(&session.client, 1, archived).await?;
            let pages = first.pages;
            let mut first = Some(first);
            for index in 1..=pages {
                let page = if index == 1 {
                    first.take().unwrap()
                } else {
                    self.library(&session.client, index, archived).await?
                };
                ensure!(
                    fingerprint(&json!(["gumroad", page.buyer_identity])) == session.identity,
                    "ACCOUNT_IDENTITY_MISMATCH"
                );
                session.verified_at = Instant::now();
                self.index_download_page(account, &session, &page.purchases)?;
                for row in page.purchases.into_iter().filter(|row| {
                    !row.excluded_non_asset()
                        && purchase_ids.contains(row.purchase_id.as_str())
                        && !attempted.contains(&row.purchase_id)
                }) {
                    let observation = self
                        .expanded_content(&session.client, &row, Some(source_product))
                        .await?;
                    if let Some(ticket) =
                        self.retain_download(account, product, file, &session, &observation)?
                    {
                        return Ok(ticket);
                    }
                }
            }
        }
        bail!("FILE_UNAVAILABLE")
    }
    fn index_download_page(
        &self,
        account: &str,
        session: &DownloadSession,
        rows: &[LibraryPurchase],
    ) -> Result<()> {
        let record = self.account(account)?;
        ensure!(
            super::active_bound_account(&record) && record["identityHash"] == session.identity,
            "STORE_IDENTITY_CHANGED"
        );
        let mut cache = self
            .inner
            .downloads
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        if !cache.contains_key(account) && cache.len() >= 16 {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, session)| session.verified_at)
                .map(|(id, _)| id.clone())
            {
                cache.remove(&oldest);
            }
        }
        let cached = cache
            .entry(account.to_owned())
            .or_insert_with(|| session.snapshot());
        if !cached.valid(&record, Instant::now()) {
            *cached = session.snapshot();
        }
        cached.index_purchases(rows);
        Ok(())
    }
    fn retain_download(
        &self,
        account: &str,
        product: &str,
        file: &Value,
        session: &DownloadSession,
        observation: &ContentObservation,
    ) -> Result<Option<(DownloadSession, gumroad::FileObservation)>> {
        let source_product = &observation.product_id;
        ensure!(
            product == observation.catalogue_product_id(),
            "GUMROAD_PRODUCT_BINDING_UNPROVEN"
        );
        let mut manifest = DownloadManifest {
            purchase_ids: if observation.source_bundle_id.is_some() {
                observation.member_purchase_ids.clone()
            } else {
                BTreeSet::from([observation.purchase_id.clone()])
            },
            source_bundle_id: observation.source_bundle_id.clone(),
            observed_at: Instant::now(),
            files: observation.files.clone(),
            revisions: BTreeMap::new(),
        };
        for offered in &manifest.files {
            let id = remote_file_id(account, source_product, offered);
            if let Some(saved) = self.inner.catalogue.get("remote_file", &id)? {
                if saved["sourceFingerprint"] == source_fingerprint(offered) {
                    if let Some(revision) = saved["revision"]
                        .as_str()
                        .filter(|revision| !revision.is_empty())
                    {
                        manifest.revisions.insert(id, revision.to_owned());
                    }
                }
            }
        }
        let Some(offered) = manifest
            .offered(account, source_product, file, Instant::now())
            .cloned()
        else {
            return Ok(None);
        };
        let record = self.account(account)?;
        ensure!(
            super::active_bound_account(&record) && record["identityHash"] == session.identity,
            "STORE_IDENTITY_CHANGED"
        );
        let mut cache = self
            .inner
            .downloads
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        if !cache.contains_key(account) && cache.len() >= 16 {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, session)| session.verified_at)
                .map(|(id, _)| id.clone())
            {
                cache.remove(&oldest);
            }
        }
        let cached = cache
            .entry(account.to_owned())
            .or_insert_with(|| session.snapshot());
        if !cached.valid(&record, Instant::now()) {
            *cached = session.snapshot();
        }
        if observation.inventory_complete {
            if cached.manifests.len() >= 8 {
                if let Some(oldest) = cached
                    .manifests
                    .iter()
                    .min_by_key(|(_, manifest)| manifest.observed_at)
                    .map(|(id, _)| id.clone())
                {
                    cached.manifests.remove(&oldest);
                }
            }
            cached.manifests.insert(product.to_owned(), manifest);
        }
        Ok(Some((session.snapshot(), offered)))
    }
    pub async fn download_ticket(&self, app: &AppHandle, file_id: &str) -> Result<DownloadTicket> {
        let file = self
            .inner
            .catalogue
            .get("remote_file", file_id)?
            .filter(|file| file["store"] == "Gumroad")
            .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
        let account = field(&file, "accountId")?;
        let saved_account = self.account(account)?;
        let product = field(&file, "productId")?;
        let result = async {
            ensure!(super::active_bound_account(&saved_account), "AUTH_REQUIRED");
            let (session, offered) =
                if let Some(cached) = self.cached_download(account, product, &file)? {
                    cached
                } else {
                    self.fill_download(app, account, product, &file).await?
                };
            let head_client = http::build_head_client(StoreProvider::Gumroad, session.jar.clone())?;
            let url = offered
                .resolver
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("DOWNLOAD_LINK_UNAVAILABLE"))?
                .url()
                .clone();
            let response = self.inner.http.resolve_head(&head_client, url).await?;
            match response.status().as_u16() {
                401 => bail!("DOWNLOAD_RESOLVER_FAILED"),
                403 => bail!("SIGNED_LINK_EXPIRED"),
                404 | 410 => bail!("FILE_UNAVAILABLE"),
                _ => ensure!(
                    response.status() == reqwest::StatusCode::FOUND,
                    "DOWNLOAD_LINK_UNAVAILABLE"
                ),
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| response.url().join(value).ok())
                .filter(http::gumroad_file_destination)
                .ok_or_else(|| anyhow::anyhow!("UNSAFE_DOWNLOAD_URL"))?;
            let account_record = self.account(account)?;
            ensure!(
                super::active_bound_account(&account_record)
                    && account_record["identityHash"] == session.identity,
                "STORE_IDENTITY_CHANGED"
            );
            let current = self
                .inner
                .catalogue
                .get("remote_file", file_id)?
                .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
            ensure!(
                [
                    "id",
                    "productId",
                    "accountId",
                    "sourceProductId",
                    "sourcePurchaseId",
                    "sourceFileId",
                    "purchaseIds",
                    "sourceBundleId",
                    "revision",
                    "sourceFingerprint"
                ]
                .iter()
                .all(|key| current[*key] == file[*key]),
                "FILE_CHANGED"
            );
            Ok(DownloadTicket {
                provider: StoreProvider::Gumroad,
                expected_origin: None,
                url: location,
                client: http::build_download_client(StoreProvider::Gumroad, session.jar.clone())?,
                expected_size: offered.size_bytes,
            })
        }
        .await;
        if let Err(error) = &result {
            match safe_code(error).as_str() {
                "AUTH_REQUIRED"
                | "PERMISSION_DENIED"
                | "ACCOUNT_IDENTITY_MISMATCH"
                | "STORE_IDENTITY_CHANGED" => self.clear_downloads(account, None),
                _ => self.clear_downloads(account, Some(product)),
            }
        }
        result
    }
}

fn append_journal(checkpoint: &mut Value, id: &str) -> Result<()> {
    if checkpoint["journalIds"].is_null() {
        checkpoint["journalIds"] = json!([]);
    }
    let ids = checkpoint["journalIds"]
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
    if !ids.iter().any(|value| value == id) {
        ids.push(json!(id));
    }
    Ok(())
}
type PreparedRecords = Vec<(String, String, Value)>;
fn grouped_journals(
    catalogue: &Catalogue,
    job: &Value,
    account: &str,
    checkpoint: &Value,
    known_purchases: &BTreeMap<String, String>,
) -> Result<(BTreeMap<String, PreparedRecords>, BTreeSet<String>, bool)> {
    let mut groups = BTreeMap::<String, PreparedRecords>::new();
    let mut blocked = BTreeSet::new();
    let mut unbound = false;
    for id in checkpoint["journalIds"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?
    {
        let journal = catalogue
            .get("checkpoint", field_value(id)?)?
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        ensure!(
            journal["accountId"] == account
                && journal["runId"] == job["id"]
                && journal["store"] == "Gumroad",
            "CHECKPOINT_INVALID"
        );
        for observation in journal["observations"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?
        {
            if observation["complete"] != true {
                if let Some(product) = observation["productId"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| {
                        observation["purchaseId"]
                            .as_str()
                            .and_then(|id| known_purchases.get(id))
                            .cloned()
                    })
                {
                    blocked.insert(product);
                } else {
                    unbound = true;
                }
            }
        }
        let records: PreparedRecords = serde_json::from_value(journal["records"].clone())
            .map_err(|_| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        for (kind, id, value) in records {
            ensure!(
                matches!(
                    kind.as_str(),
                    "product" | "acquisition" | "remote_file" | "entitlement"
                ),
                "CHECKPOINT_INVALID"
            );
            let product = if kind == "product" {
                id.clone()
            } else {
                field(&value, "productId")?.to_owned()
            };
            groups.entry(product).or_default().push((kind, id, value));
        }
    }
    Ok((groups, blocked, unbound))
}
fn field_value(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))
}
fn commit_gumroad_product(
    catalogue: &Catalogue,
    job: &Value,
    product: &str,
    records: &PreparedRecords,
    checkpoint: &mut Value,
) -> Result<()> {
    commit_gumroad_product_mode(catalogue, job, product, records, checkpoint, true)
}
fn commit_gumroad_product_mode(
    catalogue: &Catalogue,
    job: &Value,
    product: &str,
    records: &PreparedRecords,
    checkpoint: &mut Value,
    authoritative: bool,
) -> Result<()> {
    let key = field(checkpoint, "id")?;
    let mut published: BTreeSet<String> = checkpoint["publishedProducts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let version = fingerprint(&json!([records, authoritative]));
    if checkpoint["publishedVersions"][product] == version {
        return Ok(());
    }
    published.insert(product.into());
    let mut updated = checkpoint.clone();
    updated["publishedProducts"] = json!(published);
    if !updated["publishedVersions"].is_object() {
        updated["publishedVersions"] = json!({});
    }
    updated["publishedVersions"][product] = json!(version);
    for _ in 0..3 {
        let mut batch = RecordBatch::new(catalogue);
        let current = batch
            .get("job", field(job, "id")?)?
            .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
        ensure!(current["state"] != "cancelled", "CANCELLED");
        for (kind, id, value) in records {
            let mut value = value.clone();
            if kind == "entitlement" && !authoritative {
                let unchanged = batch.get(kind, id)?.is_some_and(|previous| {
                    previous["manifestComplete"] == true
                        && previous["fileIds"] == value["fileIds"]
                        && previous["variantIds"] == value["variantIds"]
                });
                value["manifestComplete"] = json!(unchanged);
            }
            if kind == "remote_file" {
                if let Some(previous) = batch.get(kind, id)? {
                    let purchases: BTreeSet<_> = previous["purchaseIds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .chain(value["purchaseIds"].as_array().into_iter().flatten())
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    value["purchaseIds"] = json!(purchases);
                }
            }
            batch.stage(kind, id, value)?;
        }
        // A later base-product purchase must not overwrite an unresolved owned
        // variant quote from another grant of this same product.
        if batch
            .related("acquisition", product)?
            .iter()
            .any(|row| row["variantId"].as_str().is_some_and(|id| !id.is_empty()))
        {
            let mut value = batch
                .get("product", product)?
                .ok_or_else(|| anyhow::anyhow!("PRODUCT_UNAVAILABLE"))?;
            clear_unbound_variant_price(&mut value);
            batch.stage("product", product, value)?;
        }
        batch.stage("checkpoint", key, updated.clone())?;
        if batch.try_commit()?.is_some() {
            *checkpoint = updated;
            return Ok(());
        }
    }
    bail!("CONCURRENT_UPDATE")
}
fn parentless_purchase(card: &LibraryPurchase, bundle_purchase_ids: &BTreeSet<String>) -> bool {
    !card.excluded_non_asset() && !bundle_purchase_ids.contains(&card.purchase_id)
}

fn bundle_parent(bundle: &gumroad::LibraryBundle) -> (LibraryPurchase, ContentObservation) {
    (
        LibraryPurchase {
            purchase_id: String::new(),
            title: bundle.label.clone(),
            author: None,
            thumbnail_url: None,
            native_type: Some("bundle".into()),
            variant_name: None,
            archived: false,
            content_route: None,
        },
        ContentObservation {
            purchase_id: String::new(),
            receipt_purchase_id: None,
            product_id: bundle.product_id.clone(),
            variant_id: None,
            title: bundle.label.clone(),
            acquired_at: None,
            product_available: None,
            has_membership: false,
            files: Vec::new(),
            inventory_complete: false,
            unsupported_items: 0,
            public_product_url: None,
            bundles: Vec::new(),
            source_bundle_id: Some(bundle.product_id.clone()),
            member_purchase_ids: BTreeSet::new(),
        },
    )
}

fn project(
    batch: &mut RecordBatch<'_>,
    account: &str,
    card: &LibraryPurchase,
    observation: &ContentObservation,
) -> Result<()> {
    let product_id = observation.catalogue_product_id();
    let mut product = batch.get("product", &product_id)?.unwrap_or_else(|| json!({"id":product_id,"store":"Gumroad","provider":"gumroad","sourceTags":[],"tags":[]}));
    for (key, value) in [
        ("title", observation.title.as_ref().or(card.title.as_ref())),
        (
            "sourceTitle",
            observation.title.as_ref().or(card.title.as_ref()),
        ),
        ("author", card.author.as_ref()),
        ("shop", card.author.as_ref()),
        ("previewUrl", card.thumbnail_url.as_ref()),
    ] {
        if let Some(value) = value {
            product[key] = json!(value);
        }
    }
    if let Some(url) = &observation.public_product_url {
        product["sourceUrl"] = json!(url.as_str());
    }
    product["observedAt"] = json!(now());
    batch.stage("product", &product_id, product)?;
    let acquisition_id = format!(
        "gumroad:{account}:{}",
        fingerprint(&if let Some(bundle) = &observation.source_bundle_id {
            json!(["bundle", bundle])
        } else {
            json!(observation.purchase_id)
        })
    );
    let mut acquisition = batch.get("acquisition", &acquisition_id)?.unwrap_or_else(|| json!({"id":acquisition_id,"productId":product_id,"accountId":account,"store":"Gumroad","provider":"gumroad","source":"library"}));
    if let Some(bundle) = &observation.source_bundle_id {
        acquisition["sourceBundleId"] = json!(bundle);
        acquisition["purchaseIds"] = json!(observation.member_purchase_ids);
        acquisition.as_object_mut().unwrap().remove("purchaseId");
    } else {
        acquisition["purchaseId"] = json!(observation.purchase_id);
    }
    if let Some(date) = &observation.acquired_at {
        acquisition["acquiredDateText"] = json!(date.to_rfc3339());
        acquisition["acquiredAt"] = json!(date.to_rfc3339());
    }
    acquisition["variantId"] = json!(observation.variant_id);
    acquisition["variantName"] = json!(card.variant_name);
    acquisition["isArchived"] = json!(card.archived);
    acquisition["membership"] = json!(observation.has_membership);
    acquisition["observedAt"] = json!(now());
    batch.stage("acquisition", &acquisition_id, acquisition)?;
    let mut file_ids = BTreeSet::new();
    for file in &observation.files {
        if file.access != FileAccess::Downloadable || file.name.is_none() {
            continue;
        }
        let id = format!(
            "gumroad:{account}:{}",
            fingerprint(&json!([
                observation.product_id,
                file.source_product_id,
                file.file_id
            ]))
        );
        let previous = batch.get("remote_file", &id)?;
        let mut purchase_ids: BTreeSet<String> = previous
            .as_ref()
            .and_then(|row| row["purchaseIds"].as_array())
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        if observation.source_bundle_id.is_some() {
            purchase_ids.extend(observation.member_purchase_ids.iter().cloned());
        } else {
            purchase_ids.insert(observation.purchase_id.clone());
        }
        let evidence = fingerprint(&json!([file.name, file.folders, file.size_bytes]));
        let revision = previous
            .as_ref()
            .filter(|row| row["sourceFingerprint"] == evidence)
            .and_then(|row| row["revision"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("observed-{evidence}"));
        batch.stage("remote_file", &id, json!({"id":id,"productId":product_id,"accountId":account,"store":"Gumroad","provider":"gumroad","source":"library","sourceBundleId":observation.source_bundle_id,"sourceFileId":file.file_id,"sourceProductId":file.source_product_id,"sourcePurchaseId":file.source_purchase_id,"purchaseIds":purchase_ids,"name":file.name,"sourceFolders":file.folders,"sizeBytes":file.size_bytes,"revision":revision,"sourceFingerprint":evidence,"revisionBasis":"unverifiedSourceMetadata","requiresContentVerification":true,"observedAt":now()}))?;
        file_ids.insert(id);
    }
    let mut entitlement = batch
        .get("entitlement", &acquisition_id)?
        .unwrap_or_else(|| json!({"id":acquisition_id}));
    if !observation.inventory_complete {
        file_ids.extend(
            entitlement["fileIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    let mut fresh = json!({"id":acquisition_id,"productId":product_id,"accountId":account,"store":"Gumroad","provider":"gumroad","source":"library","fileIds":file_ids,"manifestComplete":observation.inventory_complete,"currentManifest":true,"unresolvedItems":observation.unsupported_items,"observedAt":now()});
    if let Some(bundle) = &observation.source_bundle_id {
        fresh["sourceBundleId"] = json!(bundle);
        fresh["purchaseIds"] = json!(observation.member_purchase_ids);
        entitlement.as_object_mut().unwrap().remove("purchaseId");
    } else {
        fresh["purchaseId"] = json!(observation.purchase_id);
    }
    entitlement
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("ENTITLEMENT_INVALID"))?
        .extend(fresh.as_object().unwrap().clone());
    batch.stage("entitlement", &acquisition_id, entitlement)
}

fn needs_public_quote(observation: &ContentObservation) -> bool {
    // Public bodies enrich every product. Listing quotes and billing flags do not
    // establish an owned option's current price. Paid evidence is independent.
    observation.variant_id.is_none()
}

fn project_public(
    batch: &mut RecordBatch<'_>,
    observation: &ContentObservation,
    public: Option<&gumroad::PublicProductObservation>,
) -> Result<()> {
    let id = observation.catalogue_product_id();
    let mut product = batch
        .get("product", &id)?
        .ok_or_else(|| anyhow::anyhow!("PRODUCT_UNAVAILABLE"))?;
    product["priceCheckedAt"] = json!(now());
    product["priceRefreshErrorCode"] = Value::Null;
    product["priceRefreshState"] = json!(if public.is_some() && needs_public_quote(observation) {
        "observed"
    } else {
        "unavailable"
    });
    if let Some(public) = public {
        ensure!(
            public.product_id == observation.product_id,
            "GUMROAD_PRODUCT_BINDING_UNPROVEN"
        );
        if let Some(description) = &public.description {
            product["sourceDescription"] = json!(description);
        }
    }
    if let Some(public) = public.filter(|_| needs_public_quote(observation)) {
        let amount = public
            .current_price
            .as_ref()
            .and_then(gumroad::PublicPrice::usd_amount);
        product["price"] = json!(amount);
        product["priceCurrency"] = json!(amount.map(|_| "USD"));
        product["priceBasis"] = json!(amount.map(|_| "gumroadPublicPriceCents"));
        product["priceFormatted"] = json!(public.price_formatted);
        product["priceSourceCurrency"] = json!(public.price_source_currency);
        product["sourcePriceCents"] = json!(public
            .current_price
            .as_ref()
            .filter(|price| price.price_cents <= 9_007_199_254_740_991)
            .map(|price| price.price_cents));
        product["isPayWhatYouWant"] = json!(public
            .current_price
            .as_ref()
            .map(|price| price.pay_what_you_want));
        product["isRecurringBilling"] =
            json!(public.current_price.as_ref().map(|price| price.recurring));
        product["priceObservedAt"] = json!(now());
    }
    // The current public observation contains only a product-level quote, not
    // an exact option-ID price. It cannot establish the owned variant's price.
    if observation.variant_id.is_some() {
        clear_unbound_variant_price(&mut product);
    }
    batch.stage("product", &id, product)
}
fn clear_unbound_variant_price(product: &mut Value) {
    for key in ["price", "priceCurrency", "priceBasis", "priceFormatted"] {
        product[key] = Value::Null;
    }
}

fn merge_member_files(parent: &mut ContentObservation, files: Vec<gumroad::FileObservation>) {
    for file in files {
        if let Some(previous) = parent.files.iter_mut().find(|previous| {
            previous.file_id == file.file_id && previous.source_product_id == file.source_product_id
        }) {
            if previous.name != file.name
                || previous.size_bytes != file.size_bytes
                || previous.access != file.access
            {
                previous.access = FileAccess::Unavailable;
                previous.resolver = None;
                parent.unsupported_items += 1;
            }
        } else {
            parent.files.push(file);
        }
    }
}

pub fn validate_download_response(response: &reqwest::Response) -> Result<()> {
    ensure!(
        http::gumroad_file_destination(response.url()),
        "UNSAFE_DOWNLOAD_URL"
    );
    match response.status().as_u16() {
        401 => bail!("AUTH_REQUIRED"),
        403 => bail!("SIGNED_LINK_EXPIRED"),
        404 | 410 => bail!("FILE_UNAVAILABLE"),
        429 => bail!("RATE_LIMITED"),
        _ => ensure!(response.status().is_success(), "DOWNLOAD_RESOLVER_FAILED"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn startup_recovery_isolates_orphan_jobs_and_keeps_later_jobs_resumable() {
        let path = std::env::temp_dir().join(format!("gumroad-startup-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = GumroadService::new(catalogue.clone()).unwrap();
        let missing = "d0140000-0000-4000-8000-000000000001";
        let healthy = "d0140000-0000-4000-8000-000000000002";
        catalogue.put("account", healthy, json!({"id":healthy,"store":"Gumroad"})).unwrap();
        for (id, owner) in [("authored-orphan-job", missing), ("authored-healthy-job", healthy)] {
            let job = json!({"id":id,"type":"gumroadSync","store":"Gumroad","accountId":owner,"state":"running"});
            catalogue.put("job", id, job.clone()).unwrap();
            let recovered = service.prepare_recovered_job(job).unwrap();
            assert_eq!(recovered.is_some(), owner == healthy);
        }
        let orphan = catalogue.get("job", "authored-orphan-job").unwrap().unwrap();
        assert_eq!(orphan["state"], "needsAction");
        assert_eq!(orphan["errorCode"], "ACCOUNT_UNAVAILABLE");
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn stale_retry_cannot_overwrite_the_first_retry_attempt() {
        let path = std::env::temp_dir().join(format!("gumroad-retry-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = GumroadService::new(catalogue.clone()).unwrap();
        let previous = json!({"id":"authored-retry","type":"gumroadLogin","store":"Gumroad","accountId":"authored-account","state":"needsAction","errorCode":"RETRY_REQUIRED"});
        catalogue.put("job", "authored-retry", previous.clone()).unwrap();
        let mut running = service.requeue_job(&previous).unwrap();
        assert_eq!(running["state"], "queued");
        running["state"] = json!("running");
        running["progress"] = json!({"pages":5});
        catalogue.put("job", "authored-retry", running.clone()).unwrap();
        assert_eq!(safe_code(&service.requeue_job(&previous).unwrap_err()), "JOB_BUSY");
        assert_eq!(catalogue.get("job", "authored-retry").unwrap().unwrap(), running);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn content_pipeline_publishes_completed_grant_before_pending_work_and_merges_next_grant()
    {
        let path =
            std::env::temp_dir().join(format!("gumroad-page-pipeline-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = GumroadService::new(catalogue.clone()).unwrap();
        let account = "authored-account";
        let job = json!({"id":"authored-content-job","state":"running","type":"gumroadSync","store":"Gumroad","accountId":account});
        catalogue
            .put("job", "authored-content-job", job.clone())
            .unwrap();
        let mut checkpoint = json!({"id":"authored-checkpoint","journalIds":[],"nextPages":[1,1],"publishedProducts":[],"complete":false});
        let mut first = content(
            "authored-product",
            "first-purchase",
            "first-file",
            "AUTHORED_CAPABILITY",
        );
        first.variant_id = Some("first-variant".into());
        let mut second = content(
            "authored-product",
            "second-purchase",
            "second-file",
            "AUTHORED_CAPABILITY",
        );
        second.variant_id = Some("second-variant".into());
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let mut wait = Some(wait);
        let mut stream = futures_util::stream::iter([first, second].into_iter().enumerate().map(
            |(index, observation)| {
                let blocked = (index == 0).then(|| wait.take().unwrap());
                async move {
                    if let Some(blocked) = blocked {
                        blocked.await.unwrap();
                    }
                    observation
                }
            },
        ))
        .buffer_unordered(2);
        let card = |observation: &ContentObservation| LibraryPurchase {
            purchase_id: observation.purchase_id.clone(),
            title: Some("Authored product".into()),
            author: None,
            thumbnail_url: None,
            native_type: Some("digital".into()),
            variant_name: None,
            archived: false,
            content_route: None,
        };
        let ready = stream.next().await.unwrap();
        assert_eq!(ready.purchase_id, "second-purchase");
        let ids = service
            .consume_complete_observation(
                &job,
                account,
                &card(&ready),
                &ready,
                None,
                None,
                false,
                &mut checkpoint,
            )
            .unwrap();
        assert_eq!(ids, vec!["gumroad:authored-product"]);
        assert_eq!(checkpoint["nextPages"], json!([1, 1]));
        assert_eq!(checkpoint["complete"], false);
        assert_eq!(
            catalogue
                .related("acquisition", "gumroad:authored-product")
                .unwrap()
                .len(),
            1
        );
        release.send(()).unwrap();
        let ready = stream.next().await.unwrap();
        service
            .consume_complete_observation(
                &job,
                account,
                &card(&ready),
                &ready,
                None,
                None,
                false,
                &mut checkpoint,
            )
            .unwrap();
        assert_eq!(
            catalogue
                .related("acquisition", "gumroad:authored-product")
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            catalogue
                .related("remote_file", "gumroad:authored-product")
                .unwrap()
                .len(),
            2
        );
        let entitlements = catalogue
            .related("entitlement", "gumroad:authored-product")
            .unwrap();
        assert_eq!(entitlements.len(), 2);
        assert!(entitlements
            .iter()
            .all(|row| row["manifestComplete"] == false));
        let (groups, blocked, unbound) =
            grouped_journals(&catalogue, &job, account, &checkpoint, &BTreeMap::new()).unwrap();
        assert!(blocked.is_empty() && !unbound);
        for (product, records) in groups {
            commit_gumroad_product(&catalogue, &job, &product, &records, &mut checkpoint).unwrap();
        }
        assert!(catalogue
            .related("entitlement", "gumroad:authored-product")
            .unwrap()
            .iter()
            .all(|grant| grant["manifestComplete"] == true));
        for journal in catalogue.list("checkpoint").unwrap() {
            assert!(!journal.to_string().contains("AUTHORED_CAPABILITY"));
        }
        catalogue
            .put(
                "job",
                "authored-content-job",
                json!({"id":"authored-content-job","state":"cancelled"}),
            )
            .unwrap();
        let third = content(
            "authored-product",
            "third-purchase",
            "third-file",
            "AUTHORED_CAPABILITY",
        );
        assert_eq!(
            service
                .consume_complete_observation(
                    &job,
                    account,
                    &card(&third),
                    &third,
                    None,
                    None,
                    false,
                    &mut checkpoint
                )
                .unwrap_err()
                .to_string(),
            "CANCELLED"
        );
        assert_eq!(
            catalogue
                .related("remote_file", "gumroad:authored-product")
                .unwrap()
                .len(),
            2
        );
        drop(stream);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    use super::*;
    #[tokio::test]
    async fn concurrent_grants_share_one_public_observation_including_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for fail in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let reads = Mutex::new(BTreeMap::new());
            let mut grants = futures_util::stream::iter(0..2)
                .map(|_| {
                    let calls = calls.clone();
                    let read = shared_public_read(&reads, "authored-product", || {
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::task::yield_now().await;
                            if fail {
                                Err("GUMROAD_PUBLIC_METADATA_UNAVAILABLE".into())
                            } else {
                                Ok(None)
                            }
                        }
                        .boxed()
                        .shared()
                    })
                    .unwrap();
                    async move { read.await }
                })
                .buffered(2);
            let mut results = Vec::new();
            while let Some(result) = grants.next().await {
                results.push(result);
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(results.len(), 2);
            assert!(results.iter().all(|result| result.is_err() == fail));
        }
    }
    #[test]
    fn skipping_unbound_option_quote_preserves_files_paid_evidence_and_source_fields() {
        let path =
            std::env::temp_dir().join(format!("nna-gumroad-option-skip-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let account = "authored-account";
        let product = "gumroad:authored-product";
        let acquisition_id = format!(
            "gumroad:{account}:{}",
            fingerprint(&json!("authored-purchase"))
        );
        let prior = json!({"id":product,"store":"Gumroad","price":12.5,"priceCurrency":"USD","priceBasis":"gumroadPublicPriceCents","sourcePriceCents":1250,"isPayWhatYouWant":false,"sourceDescription":"Preserved source description"});
        catalogue.put("product", product, prior).unwrap();
        catalogue.put("acquisition",&acquisition_id,json!({"id":acquisition_id,"accountId":account,"productId":product,"paidPrice":7.5,"paidCurrency":"USD","paidPriceBasis":"productLineTotal"})).unwrap();
        catalogue
            .put(
                "overlay",
                product,
                json!({"id":product,"note":"Manual note","title":"Manual title"}),
            )
            .unwrap();
        let card = LibraryPurchase {
            purchase_id: "authored-purchase".into(),
            title: Some("Authored product".into()),
            author: None,
            thumbnail_url: None,
            native_type: Some("digital".into()),
            variant_name: Some("Owned option".into()),
            archived: false,
            content_route: None,
        };
        let mut observation = content(
            "authored-product",
            "authored-purchase",
            "authored-file",
            "AUTHORED_CAPABILITY",
        );
        assert!(needs_public_quote(&observation));
        observation.variant_id = Some("authored-option".into());
        observation.public_product_url =
            Some(Url::parse("https://authored.gumroad.com/l/product").unwrap());
        assert!(!needs_public_quote(&observation));
        let mut batch = RecordBatch::new(&catalogue);
        project(&mut batch, account, &card, &observation).unwrap();
        project_public(&mut batch, &observation, None).unwrap();
        batch.commit().unwrap();
        let stored = catalogue.get("product", product).unwrap().unwrap();
        assert!(stored["price"].is_null());
        assert_eq!(stored["sourcePriceCents"], 1250);
        assert_eq!(stored["sourceDescription"], "Preserved source description");
        assert_eq!(catalogue.related("remote_file", product).unwrap().len(), 1);
        assert!(catalogue
            .related("entitlement", product)
            .unwrap()
            .iter()
            .any(|row| row["manifestComplete"] == true));
        assert_eq!(
            catalogue
                .get("acquisition", &acquisition_id)
                .unwrap()
                .unwrap()["variantId"],
            "authored-option"
        );
        assert_eq!(
            catalogue
                .get("acquisition", &acquisition_id)
                .unwrap()
                .unwrap()["paidPrice"],
            7.5
        );
        assert_eq!(
            catalogue.get("overlay", product).unwrap().unwrap()["note"],
            "Manual note"
        );
    }

    #[test]
    fn public_body_enriches_owned_option_without_promoting_listing_price_or_unknown_body() {
        let path = std::env::temp_dir().join(format!(
            "nna-gumroad-public-body-{}", uuid::Uuid::new_v4()
        ));
        let catalogue = Catalogue::open(&path).unwrap();
        let account = "authored-account";
        let product = "gumroad:authored-product";
        let acquisition_id = format!(
            "gumroad:{account}:{}", fingerprint(&json!("authored-purchase"))
        );
        catalogue.put("product", product, json!({
            "id":product, "store":"Gumroad", "provider":"gumroad",
            "sourceDescription":"Previously observed VRChat body", "sourcePriceCents":1250,
            "price":12.5, "priceCurrency":"USD", "priceBasis":"gumroadPublicPriceCents",
            "isPayWhatYouWant":false, "isRecurringBilling":false
        })).unwrap();
        catalogue.put("acquisition", &acquisition_id, json!({
            "id":acquisition_id, "accountId":account, "productId":product,
            "paidPrice":7.5, "paidCurrency":"USD", "paidPriceBasis":"productLineTotal"
        })).unwrap();
        let card = LibraryPurchase {
            purchase_id: "authored-purchase".into(), title: Some("Authored hair".into()),
            author: None, thumbnail_url: None, native_type: Some("digital".into()),
            variant_name: Some("Owned option".into()), archived: false, content_route: None,
        };
        let mut observation = content(
            "authored-product", "authored-purchase", "authored-file", "AUTHORED_CAPABILITY"
        );
        observation.variant_id = Some("authored-option".into());
        observation.public_product_url = Some(
            Url::parse("https://authored.gumroad.com/l/product").unwrap()
        );
        assert!(!needs_public_quote(&observation));
        for (body, expected) in [
            (Some(json!("<p>Hair &amp; accessories for VRChat</p>")), "Hair & accessories for VRChat"),
            (None, "Hair & accessories for VRChat"),
            (Some(Value::Null), "Hair & accessories for VRChat"),
            (Some(json!("")), ""),
        ] {
            let mut value = json!({"api_version":1,"id":"authored-product",
                "price_cents":9999,"currency_code":"usd","price_formatted":"$99.99",
                "is_pay_what_you_want":true,"is_recurring_billing":true});
            if let Some(body) = body { value["description"] = body; }
            let public = gumroad::parse_public_product(&value, Some("authored-product")).unwrap();
            let mut batch = RecordBatch::new(&catalogue);
            project(&mut batch, account, &card, &observation).unwrap();
            project_public(&mut batch, &observation, Some(&public)).unwrap();
            batch.commit().unwrap();
            let stored = catalogue.get("product", product).unwrap().unwrap();
            assert_eq!(stored["sourceDescription"], expected);
            for field in ["price", "priceCurrency", "priceBasis", "priceFormatted"] {
                assert!(stored[field].is_null(), "{field}");
            }
            assert_eq!(stored["sourcePriceCents"], 1250);
            assert_eq!(stored["isPayWhatYouWant"], false);
            assert_eq!(stored["isRecurringBilling"], false);
            assert_eq!(stored["priceRefreshState"], "unavailable");
            assert_eq!(catalogue.get("acquisition", &acquisition_id).unwrap().unwrap()["paidPrice"], 7.5);
            assert_eq!(catalogue.related("remote_file", product).unwrap().len(), 1);
        }
    }

    #[test]
    fn terminal_auth_failure_expires_account_without_erasing_binding_or_ownership() {
        let path =
            std::env::temp_dir().join(format!("nna-gumroad-terminal-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = GumroadService::new(catalogue.clone()).unwrap();
        let account_id = "11111111-1111-4111-8111-111111111111";
        let baseline = json!({"id":account_id,"store":"Gumroad","state":"connected","status":"connected","identityVerified":true,"identityHash":"authored-buyer-hash","authenticatedAt":"2026-10-08T01:02:03Z","disconnected":false,"lastSync":"2026-10-08T01:02:03Z","lastSyncAt":"2026-10-08T01:02:03Z"});
        let owned = json!({"id":"authored-acquisition","accountId":account_id,"productId":"gumroad:authored-product","purchaseId":"authored-purchase"});
        catalogue
            .put("acquisition", "authored-acquisition", owned.clone())
            .unwrap();
        for kind in ["gumroadSync", "gumroadUpdate"] {
            for error in [
                Some("AUTH_REQUIRED"),
                Some("NETWORK_UNAVAILABLE"),
                Some("CANCELLED"),
                None,
            ] {
                catalogue
                    .put("account", account_id, baseline.clone())
                    .unwrap();
                let job = json!({"id":"authored-terminal-job","type":kind,"store":"Gumroad","accountId":account_id,"state":"running","progress":{"pages":0,"stage":"collecting"}});
                catalogue
                    .put("job", "authored-terminal-job", job.clone())
                    .unwrap();
                if error == Some("CANCELLED") {
                    let mut cancellation = job.clone();
                    cancellation["state"] = json!("cancelled");
                    catalogue
                        .put("job", "authored-terminal-job", cancellation)
                        .unwrap();
                }
                catalogue
                    .command(
                        "preferences",
                        json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}}),
                    )
                    .unwrap();
                let other_pending = kind == "gumroadSync" && error == Some("AUTH_REQUIRED");
                if other_pending {
                    catalogue.put("job", "other-provider-sync", json!({"id":"other-provider-sync","type":"jinxxySync","state":"queued"})).unwrap();
                }
                let reported_error = if error == Some("CANCELLED") {
                    None
                } else {
                    error
                };
                service.finish_job(&job, reported_error).unwrap();
                if other_pending {
                    assert_eq!(
                        catalogue.get("preferences", "app").unwrap().unwrap()
                            ["avatarAutomaticEnabled"]["booth:3390957"],
                        true,
                        "A pending other-provider sync defers the terminal hook"
                    );
                    catalogue.put("job", "other-provider-sync", json!({"id":"other-provider-sync","type":"jinxxySync","state":"cancelled"})).unwrap();
                    service.finish_job(&job, error).unwrap();
                }
                assert_eq!(
                    catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"],
                    json!({}),
                    "The actual terminal hook recomputes automatic defaults"
                );
                let account = catalogue.get("account", account_id).unwrap().unwrap();
                let terminal = catalogue
                    .get("job", "authored-terminal-job")
                    .unwrap()
                    .unwrap();
                let expected_state = if error == Some("AUTH_REQUIRED") {
                    "expired"
                } else {
                    "connected"
                };
                assert_eq!(account["state"], expected_state);
                assert_eq!(account["status"], expected_state);
                for field in [
                    "identityVerified",
                    "identityHash",
                    "authenticatedAt",
                    "disconnected",
                ] {
                    assert_eq!(account[field], baseline[field]);
                }
                if error.is_some() || kind == "gumroadUpdate" {
                    assert_eq!(account["lastSync"], baseline["lastSync"]);
                    assert_eq!(account["lastSyncAt"], baseline["lastSyncAt"]);
                } else {
                    assert_eq!(account["lastSyncAt"], terminal["completedAt"]);
                }
                assert_eq!(
                    terminal["state"],
                    if error == Some("CANCELLED") {
                        "cancelled"
                    } else if error.is_some() {
                        "needsAction"
                    } else {
                        "completed"
                    }
                );
                assert_eq!(terminal["errorCode"], json!(error));
                assert_eq!(
                    catalogue
                        .get("acquisition", "authored-acquisition")
                        .unwrap(),
                    Some(owned.clone())
                );
            }
        }
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn download_cache_requires_current_identity_entitlement_revision_and_short_lifetimes() {
        let clock = Instant::now();
        let account = json!({"identityVerified":true,"identityHash":"authored-buyer-hash","status":"connected","state":"connected","disconnected":false});
        let mut session = DownloadSession {
            client: reqwest::Client::new(),
            jar: Arc::new(reqwest::cookie::Jar::default()),
            identity: "authored-buyer-hash".into(),
            verified_at: clock,
            manifests: BTreeMap::new(),
            purchases: BTreeMap::new(),
        };
        assert!(session.valid(&account, clock + Duration::from_secs(119)));
        assert!(!session.valid(&account, clock + Duration::from_secs(120)));
        let mut changed = account.clone();
        changed["identityHash"] = json!("another-authored-buyer");
        assert!(!session.valid(&changed, clock));
        let indexed_card = LibraryPurchase {
            purchase_id: "authored-indexed-purchase".into(),
            title: None,
            author: None,
            thumbnail_url: None,
            native_type: Some("digital".into()),
            variant_name: None,
            archived: false,
            content_route: None,
        };
        session.index_purchases(std::slice::from_ref(&indexed_card));
        let sought = BTreeSet::from([indexed_card.purchase_id.as_str()]);
        assert_eq!(session.indexed_purchases(&account, &sought, clock).len(), 1);
        assert!(session
            .indexed_purchases(&account, &BTreeSet::from(["unseen-purchase"]), clock)
            .is_empty());
        assert!(session
            .indexed_purchases(&changed, &sought, clock)
            .is_empty());
        assert!(session
            .indexed_purchases(&account, &sought, clock + Duration::from_secs(120))
            .is_empty());
        for index in 0..10000 {
            let mut card = indexed_card.clone();
            card.purchase_id = format!("authored-index-{index}");
            session.index_purchases(&[card]);
        }
        assert_eq!(session.purchases.len(), 10000);
        assert!(!session.purchases.contains_key("authored-index-9999"));
        assert!(session.snapshot().purchases.is_empty());
        changed = account.clone();
        changed["status"] = json!("disconnected");
        assert!(!session.valid(&changed, clock));
        let observation = content(
            "authored-product",
            "authored-purchase",
            "authored-file",
            "authored-capability",
        );
        let offered = &observation.files[0];
        let id = remote_file_id("authored-account", "authored-product", offered);
        let file = json!({"id":id,"accountId":"authored-account","productId":"gumroad:authored-product","sourceProductId":offered.source_product_id,"sourcePurchaseId":offered.source_purchase_id,"sourceFileId":offered.file_id,"purchaseIds":["authored-purchase"],"sourceFingerprint":source_fingerprint(offered),"revision":"authored-revision"});
        let manifest = DownloadManifest {
            purchase_ids: BTreeSet::from([observation.purchase_id]),
            source_bundle_id: None,
            observed_at: clock,
            files: observation.files,
            revisions: BTreeMap::from([(id, "authored-revision".into())]),
        };
        assert!(manifest
            .offered(
                "authored-account",
                "authored-product",
                &file,
                clock + Duration::from_secs(59)
            )
            .is_some());
        assert!(manifest
            .offered(
                "authored-account",
                "authored-product",
                &file,
                clock + Duration::from_secs(60)
            )
            .is_none());
        for (key, value) in [
            ("revision", json!("changed")),
            ("sourceProductId", json!("other-product")),
            ("sourcePurchaseId", json!("other-purchase")),
            ("sourceFileId", json!("other-file")),
            ("purchaseIds", json!(["other-purchase"])),
        ] {
            let mut changed = file.clone();
            changed[key] = value;
            assert!(manifest
                .offered("authored-account", "authored-product", &changed, clock)
                .is_none());
        }
        session
            .manifests
            .insert("gumroad:authored-product".into(), manifest);
        assert!(session.snapshot().manifests.is_empty());
        assert_eq!(session.manifests.len(), 1);
    }

    #[test]
    fn optional_public_failure_keeps_complete_bound_product_publishable() {
        let path =
            std::env::temp_dir().join(format!("nna-gumroad-public-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let product = "gumroad:authored-product";
        let account = "authored-account";
        let job = json!({"id":"authored-job","state":"running","accountId":account,"store":"Gumroad","type":"gumroadSync"});
        catalogue.put("job", "authored-job", job.clone()).unwrap();
        catalogue.put("product", product, json!({"id":product,"store":"Gumroad","price":12.5,"priceCurrency":"USD","priceBasis":"gumroadPublicPriceCents","sourceTags":["Authored source tag","Authored rejected tag"]})).unwrap();
        catalogue
            .put("account", account, json!({"id":account,"store":"Gumroad"}))
            .unwrap();
        let overlay = json!({"id":product,"title":"Human Gumroad title","note":"Human note","category":"Hair","tags":["Authored manual tag"],"rejectedTags":["Authored rejected tag"]});
        catalogue.put("overlay", product, overlay.clone()).unwrap();
        let card = LibraryPurchase {
            purchase_id: "authored-purchase".into(),
            title: Some("Authored complete product".into()),
            author: None,
            thumbnail_url: None,
            native_type: Some("digital".into()),
            variant_name: None,
            archived: false,
            content_route: None,
        };
        let observation = content(
            "authored-product",
            "authored-purchase",
            "authored-file",
            "AUTHORED_CAPABILITY",
        );
        assert!(observation.inventory_complete);
        // An already proven historical payment is independent of the fresh
        // public quote. The current Gumroad parser does not infer a new payment.
        let acquisition_id = format!(
            "gumroad:{account}:{}",
            fingerprint(&json!(observation.purchase_id))
        );
        catalogue.put("acquisition", &acquisition_id, json!({"id":acquisition_id,"productId":product,"accountId":account,"store":"Gumroad","provider":"gumroad","source":"library","purchaseId":observation.purchase_id,"paidPrice":7.5,"paidCurrency":"USD","paidPriceBasis":"productLineTotal"})).unwrap();
        let mut prepared = RecordBatch::new(&catalogue);
        project(&mut prepared, account, &card, &observation).unwrap();
        project_public(&mut prepared, &observation, None).unwrap();
        let records = prepared.staged_records();
        catalogue.put("checkpoint", "authored-journal", json!({"id":"authored-journal","accountId":account,"runId":job["id"],"store":"Gumroad","observations":[{"productId":product,"complete":observation.inventory_complete,"errorCode":"GUMROAD_PUBLIC_SCHEMA_UNSUPPORTED"}],"records":records})).unwrap();
        let mut checkpoint = json!({"id":"authored-checkpoint","journalIds":["authored-journal"],"publishedProducts":[]});
        let (groups, blocked, unbound) =
            grouped_journals(&catalogue, &job, account, &checkpoint, &BTreeMap::new()).unwrap();
        assert!(blocked.is_empty() && !unbound);
        commit_gumroad_product(&catalogue, &job, product, &groups[product], &mut checkpoint)
            .unwrap();
        let published = catalogue.get("product", product).unwrap().unwrap();
        assert_eq!(published["priceRefreshState"], "unavailable");
        assert_eq!(published["price"], 12.5);
        assert_eq!(published["priceCurrency"], "USD");
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 1);
        assert_eq!(
            catalogue.list("entitlement").unwrap()[0]["manifestComplete"],
            true
        );
        let assert_retained = |catalogue: &Catalogue, price: f64| {
            let published = catalogue.get("product", product).unwrap().unwrap();
            assert_eq!(published["price"], price);
            assert_eq!(published["priceCurrency"], "USD");
            assert_eq!(
                published["sourceTags"],
                json!(["Authored source tag", "Authored rejected tag"])
            );
            assert_eq!(catalogue.get("overlay", product).unwrap().unwrap(), overlay);
            let acquisition = catalogue
                .get("acquisition", &acquisition_id)
                .unwrap()
                .unwrap();
            assert_eq!(acquisition["paidPrice"], 7.5);
            assert_eq!(acquisition["paidCurrency"], "USD");
            assert_eq!(acquisition["paidPriceBasis"], "productLineTotal");
            assert_eq!(acquisition["accountId"], account);
            assert_eq!(acquisition["purchaseId"], "authored-purchase");
            assert_eq!(acquisition["acquiredAt"], "2026-10-08T01:02:03+00:00");
            let details = catalogue
                .command("product_details", json!({"id":product}))
                .unwrap();
            assert_eq!(details["price"], price);
            assert_eq!(details["paidPrice"], 7.5);
            assert_eq!(details["accountIds"], json!([account]));
            assert_eq!(details["title"], "Human Gumroad title");
            assert_eq!(details["note"], "Human note");
            assert_eq!(details["category"], "Hair");
            assert!(details["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tag| tag == "Authored manual tag"));
            assert!(!details["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tag| tag == "Authored rejected tag"));
            let grants = catalogue.related("entitlement", product).unwrap();
            assert_eq!(grants.len(), 1);
            assert_eq!(grants[0]["accountId"], account);
            assert_eq!(grants[0]["fileIds"].as_array().unwrap().len(), 1);
            assert_eq!(grants[0]["manifestComplete"], true);
        };
        assert_retained(&catalogue, 12.5);
        assert!(!published.to_string().contains("AUTHORED_CAPABILITY"));
        let service = GumroadService::new(catalogue.clone()).unwrap();
        let mut partial = observation.clone();
        partial.inventory_complete = false;
        partial.files.clear();
        assert!(service
            .consume_complete_observation(
                &job,
                account,
                &card,
                &partial,
                None,
                None,
                true,
                &mut checkpoint
            )
            .unwrap()
            .is_empty());
        assert_eq!(
            catalogue.get("product", product).unwrap().unwrap(),
            published
        );
        assert_retained(&catalogue, 12.5);
        drop(service);
        drop(catalogue);
        let catalogue = Catalogue::open(&path).unwrap();
        let service = GumroadService::new(catalogue.clone()).unwrap();
        assert_retained(&catalogue, 12.5);
        let mut exact_variant = observation.clone();
        exact_variant.variant_id = Some("authored-owned-variant".into());
        let listing = gumroad::parse_public_product(&json!({"api_version":1,"id":"authored-product","price_cents":2000,"currency_code":"usd","price_formatted":"$20","is_pay_what_you_want":false,"is_recurring_billing":false}),Some("authored-product")).unwrap();
        let mut prepared = RecordBatch::new(&catalogue);
        project(&mut prepared, account, &card, &exact_variant).unwrap();
        project_public(&mut prepared, &exact_variant, Some(&listing)).unwrap();
        let exact_variant_records = prepared.staged_records();
        commit_gumroad_product_mode(
            &catalogue,
            &job,
            product,
            &exact_variant_records,
            &mut checkpoint,
            false,
        )
        .unwrap();
        let exact_price = catalogue.get("product", product).unwrap().unwrap();
        assert!(exact_price["price"].is_null());
        assert!(exact_price["priceCurrency"].is_null());
        assert!(exact_price["priceBasis"].is_null());
        assert_eq!(
            catalogue
                .get("acquisition", &acquisition_id)
                .unwrap()
                .unwrap()["paidPrice"],
            7.5
        );
        let exact_details = catalogue
            .command("product_details", json!({"id":product}))
            .unwrap();
        assert!(exact_details["price"].is_null());
        assert_eq!(exact_details["paidPrice"], 7.5);
        assert_eq!(exact_details["accountIds"], json!([account]));
        assert_eq!(exact_details["title"], "Human Gumroad title");
        assert_eq!(exact_details["note"], "Human note");
        assert_eq!(exact_details["category"], "Hair");
        assert!(!exact_details["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == "Authored rejected tag"));
        assert_eq!(
            exact_details["rejectedTags"],
            json!(["Authored rejected tag"])
        );
        let mut later = observation;
        later.acquired_at = None;
        let public = gumroad::parse_public_product(&json!({"api_version":1,"id":"authored-product","price_cents":2000,"currency_code":"usd","price_formatted":"$20","is_pay_what_you_want":false,"is_recurring_billing":false}),Some("authored-product")).unwrap();
        let mut prepared = RecordBatch::new(&catalogue);
        project(&mut prepared, account, &card, &later).unwrap();
        project_public(&mut prepared, &later, Some(&public)).unwrap();
        let later_records = prepared.staged_records();
        commit_gumroad_product_mode(
            &catalogue,
            &job,
            product,
            &later_records,
            &mut checkpoint,
            false,
        )
        .unwrap();
        assert_retained(&catalogue, 20.0);
        commit_gumroad_product(&catalogue, &job, product, &later_records, &mut checkpoint).unwrap();
        assert_retained(&catalogue, 20.0);
        drop(prepared);
        drop(service);
        drop(catalogue);
        let reopened = Catalogue::open(&path).unwrap();
        assert_retained(&reopened, 20.0);
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn staged_product_groups_all_purchases_and_cancel_preserves_previous_snapshot() {
        let path =
            std::env::temp_dir().join(format!("nna-gumroad-staged-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let account = "authored-account";
        let job = json!({"id":"authored-job","state":"running","accountId":account,"store":"Gumroad","type":"gumroadSync"});
        catalogue.put("job", "authored-job", job.clone()).unwrap();
        let prior = json!({"id":"gumroad:authored-product","store":"Gumroad","title":"Previous published","price":5});
        catalogue
            .put("product", "gumroad:authored-product", prior.clone())
            .unwrap();
        catalogue
            .put(
                "overlay",
                "gumroad:authored-product",
                json!({"id":"gumroad:authored-product","note":"Manual note"}),
            )
            .unwrap();
        let mut checkpoint =
            json!({"id":"authored-checkpoint","journalIds":[],"publishedProducts":[]});
        let public = gumroad::parse_public_product(&json!({"api_version":1,"id":"authored-product","price_cents":19900,"currency_code":"usd","price_formatted":"$199","is_pay_what_you_want":false,"is_recurring_billing":false}),Some("authored-product")).unwrap();
        for (purchase, file) in [
            ("authored-purchase-1", "authored-file-1"),
            ("authored-purchase-2", "authored-file-2"),
        ] {
            let card = LibraryPurchase {
                purchase_id: purchase.into(),
                title: Some("Authored product".into()),
                author: None,
                thumbnail_url: None,
                native_type: Some("digital".into()),
                variant_name: None,
                archived: false,
                content_route: None,
            };
            let mut observation = content("authored-product", purchase, file, "SECRET_CAPABILITY");
            if purchase == "authored-purchase-1" {
                observation.variant_id = Some("authored-variant".into());
            }
            let mut prepared = RecordBatch::new(&catalogue);
            project(&mut prepared, account, &card, &observation).unwrap();
            project_public(&mut prepared, &observation, Some(&public)).unwrap();
            let records = prepared.staged_records();
            let journal = json!({"id":purchase,"accountId":account,"runId":job["id"],"store":"Gumroad","observations":[{"productId":"gumroad:authored-product","complete":true}],"records":records});
            assert!(!journal.to_string().contains("SECRET_CAPABILITY"));
            catalogue.put("checkpoint", purchase, journal).unwrap();
            append_journal(&mut checkpoint, purchase).unwrap();
        }
        assert_eq!(
            catalogue
                .get("product", "gumroad:authored-product")
                .unwrap(),
            Some(prior.clone())
        );
        assert!(catalogue.list("acquisition").unwrap().is_empty());
        let (groups, blocked, unbound) =
            grouped_journals(&catalogue, &job, account, &checkpoint, &BTreeMap::new()).unwrap();
        assert!(blocked.is_empty() && !unbound);
        assert_eq!(groups.len(), 1);
        let records = &groups["gumroad:authored-product"];
        let mut cancelled = job.clone();
        cancelled["state"] = json!("cancelled");
        catalogue.put("job", "authored-job", cancelled).unwrap();
        assert_eq!(
            commit_gumroad_product(
                &catalogue,
                &job,
                "gumroad:authored-product",
                records,
                &mut checkpoint
            )
            .unwrap_err()
            .to_string(),
            "CANCELLED"
        );
        assert_eq!(
            catalogue
                .get("product", "gumroad:authored-product")
                .unwrap(),
            Some(prior)
        );
        assert!(catalogue.list("acquisition").unwrap().is_empty());
        catalogue.put("job", "authored-job", job.clone()).unwrap();
        commit_gumroad_product(
            &catalogue,
            &job,
            "gumroad:authored-product",
            records,
            &mut checkpoint,
        )
        .unwrap();
        let published = catalogue
            .get("product", "gumroad:authored-product")
            .unwrap()
            .unwrap();
        assert!(published["price"].is_null() && published["priceFormatted"].is_null());
        assert_eq!(published["sourcePriceCents"], 19900);
        assert_eq!(catalogue.list("acquisition").unwrap().len(), 2);
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 2);
        let entitlements = catalogue.list("entitlement").unwrap();
        assert_eq!(entitlements.len(), 2);
        assert!(entitlements
            .iter()
            .all(|value| value["manifestComplete"] == true));
        assert_eq!(
            catalogue
                .get("overlay", "gumroad:authored-product")
                .unwrap()
                .unwrap()["note"],
            "Manual note"
        );
        assert_eq!(
            checkpoint["publishedProducts"],
            json!(["gumroad:authored-product"])
        );
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    fn content(product: &str, purchase: &str, file: &str, capability: &str) -> ContentObservation {
        gumroad::parse_content_props(&json!({"purchase":{"id":purchase,"product_id":product,"created_at":"2026-10-08T01:02:03Z"},"content":{"content_items":[{"type":"file","id":file,"file_name":"fixture.unitypackage","file_size":42,"pdf_stamp_enabled":false,"download_url":format!("/r/{capability}/product_files?product_file_ids[]={file}")}]}}),
            &Url::parse("https://gumroad.com/d/authored-capability").unwrap(), purchase, Some(product)).unwrap()
    }

    #[tokio::test]
    async fn verification_bundle_cap_does_not_claim_complete_membership() {
        for pages in [2, 6] {
            let fetched = Arc::new(Mutex::new(Vec::new()));
            let members = enumerate_bundle_members("authored-buyer", Some(5), |page, archived| {
                let fetched = fetched.clone();
                async move {
                    fetched.lock().unwrap().push((page, archived));
                    Ok(LibraryPage {
                        buyer_identity: "authored-buyer".into(),
                        pages: if archived { 1 } else { pages },
                        count: if archived { 1 } else { pages },
                        archived_count: 1,
                        unarchived_count: pages,
                        purchases: vec![LibraryPurchase {
                            purchase_id: format!("authored-{archived}-{page}"),
                            title: None,
                            author: None,
                            thumbnail_url: None,
                            native_type: Some("digital".into()),
                            variant_name: None,
                            archived,
                            content_route: None,
                        }],
                        bundles: vec![],
                    })
                }
                .boxed()
            })
            .await
            .unwrap();
            assert_eq!(members.pages, pages.min(5) + 1);
            assert!(fetched.lock().unwrap().iter().all(|(page, _)| *page <= 5));
            assert_eq!(members.complete, pages <= 5);
            let bundle = gumroad::LibraryBundle {
                product_id: "authored-parent".into(),
                label: None,
            };
            let reuse = members
                .purchases
                .iter()
                .map(|row| {
                    (
                        row.purchase_id.clone(),
                        Some(content(
                            "authored-child",
                            &row.purchase_id,
                            &row.purchase_id,
                            "AUTHORED_CAPABILITY",
                        )),
                    )
                })
                .collect();
            let (_, parent) = assemble_observed_bundle(&bundle, &members, &reuse);
            assert_eq!(parent.files.len(), members.purchases.len());
            assert_eq!(parent.inventory_complete, pages <= 5);
            assert_eq!(parent.unsupported_items, usize::from(pages > 5));
        }
    }

    #[tokio::test]
    async fn protected_bundle_lists_precede_ordinary_publication_then_parent_content() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let bundle = gumroad::LibraryBundle {
            product_id: "authored-parent".into(),
            label: Some("Authored parent".into()),
        };
        let members = enumerate_bundle_members("authored-buyer", None, |page, archived| {
            let events = events.clone();
            async move {
                events.lock().unwrap().push(format!("members:{archived}:{page}"));
                let purchase = if archived { "archived-member" } else if page == 1 { "first-member" } else { "second-member" };
                let pages = if archived { 1 } else { 2 };
                let props = json!({"buyerIdentity":"authored-buyer","bundles":[{"id":"authored-parent","label":"Authored parent"}],"results":[{"product":{"name":"Authored child","native_type":"digital"},"purchase":{"id":purchase,"is_archived":archived,"download_url":"/d/AUTHORED_CAPABILITY"}}],"pagination":{"page":page,"pages":pages,"count":pages,"from":page,"to":page},"archived_count":1,"unarchived_count":2,"search":{"show_archived_only":archived,"query":"","creators":[],"bundles":["authored-parent"],"sort":"purchase_date"},"bundle_downloads":[{"id":"authored-parent","download_url":null,"zip_unavailable_reason":null}]});
                gumroad::parse_library_scope(&props, &Url::parse("https://gumroad.com/library?bundles=authored-parent").unwrap(), page, archived, Some("authored-parent"))
            }.boxed()
        }).await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["members:false:1", "members:false:2", "members:true:1"]
        );
        assert_eq!(members.pages, 3);
        let bundled: BTreeSet<_> = members
            .purchases
            .iter()
            .map(|row| row.purchase_id.clone())
            .collect();
        assert_eq!(bundled.len(), 3);
        assert!(members
            .purchases
            .iter()
            .all(|row| !parentless_purchase(row, &bundled)));
        let mut ordinary = members.purchases[0].clone();
        ordinary.purchase_id = "separate-purchase".into();
        assert!(parentless_purchase(&ordinary, &bundled));
        let path =
            std::env::temp_dir().join(format!("gumroad-bundle-order-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = GumroadService::new(catalogue.clone()).unwrap();
        let account = "authored-account";
        let job = json!({"id":"authored-order-job","state":"running","accountId":account,"store":"Gumroad","type":"gumroadSync"});
        catalogue
            .put("job", "authored-order-job", job.clone())
            .unwrap();
        let mut checkpoint = json!({"id":"authored-order-checkpoint","journalIds":[],"publishedProducts":[],"bundlePurchaseIds":bundled});
        let observation = content(
            "authored-child",
            "separate-purchase",
            "ordinary-file",
            "AUTHORED_OTHER_CAPABILITY",
        );
        service
            .consume_complete_observation(
                &job,
                account,
                &ordinary,
                &observation,
                None,
                None,
                false,
                &mut checkpoint,
            )
            .unwrap();
        events.lock().unwrap().push("ordinary-published".into());
        assert!(catalogue
            .get("product", "gumroad:authored-child")
            .unwrap()
            .is_some());
        assert!(catalogue
            .get("product", "gumroad:authored-parent")
            .unwrap()
            .is_none());
        let mut reuse = BTreeMap::new();
        assert!(
            !assemble_observed_bundle(&bundle, &members, &reuse)
                .1
                .inventory_complete
        );
        for row in &members.purchases {
            events
                .lock()
                .unwrap()
                .push(format!("child-content:{}", row.purchase_id));
            reuse.insert(
                row.purchase_id.clone(),
                Some(content(
                    "authored-child",
                    &row.purchase_id,
                    &format!("file-{}", row.purchase_id),
                    "AUTHORED_CAPABILITY",
                )),
            );
        }
        let (card, parent) = assemble_observed_bundle(&bundle, &members, &reuse);
        assert!(parent.inventory_complete);
        assert_eq!(parent.member_purchase_ids, bundled);
        assert_eq!(parent.files.len(), 3);
        let journal_id = "authored-parent-journal";
        let mut prepared = RecordBatch::new(&catalogue);
        project(&mut prepared, account, &card, &parent).unwrap();
        catalogue.put("checkpoint", journal_id, json!({"id":journal_id,"store":"Gumroad","accountId":account,"runId":job["id"],"observations":[{"productId":"gumroad:authored-parent","complete":true}],"records":prepared.staged_records()})).unwrap();
        append_journal(&mut checkpoint, journal_id).unwrap();
        service
            .publish_complete_journal_records(&job, &mut checkpoint, journal_id)
            .unwrap();
        assert!(catalogue
            .get("product", "gumroad:authored-parent")
            .unwrap()
            .is_some());
        assert_eq!(events.lock().unwrap()[3], "ordinary-published");
        assert!(events.lock().unwrap()[4..]
            .iter()
            .all(|event| event.starts_with("child-content:")));
        let grants = catalogue
            .related("entitlement", "gumroad:authored-parent")
            .unwrap();
        assert_eq!(grants[0]["manifestComplete"], false);
        assert!(catalogue
            .related("acquisition", "gumroad:authored-parent")
            .unwrap()[0]["paidPrice"]
            .is_null());
        let (groups, blocked, unbound) =
            grouped_journals(&catalogue, &job, account, &checkpoint, &BTreeMap::new()).unwrap();
        assert!(blocked.is_empty() && !unbound);
        for (product, records) in groups {
            commit_gumroad_product(&catalogue, &job, &product, &records, &mut checkpoint).unwrap();
        }
        assert_eq!(
            catalogue
                .related("entitlement", "gumroad:authored-parent")
                .unwrap()[0]["manifestComplete"],
            true
        );
        assert!(
            !serde_json::to_string(&catalogue.list("remote_file").unwrap())
                .unwrap()
                .contains("CAPABILITY")
        );
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn hidden_bundle_parent_keeps_real_member_binding_and_separate_child_purchase() {
        let base = Url::parse("https://gumroad.com/library?bundles=authored-parent").unwrap();
        let props = json!({"buyerIdentity":"authored-buyer","bundles":[{"id":"authored-parent","label":"Authored bundle"}],"results":[{"product":{"name":"Authored child","native_type":"digital"},"purchase":{"id":"authored-member-purchase","is_archived":false,"download_url":"/d/AUTHORED_CAPABILITY"}}],"pagination":{"page":1,"pages":1,"count":1,"from":1,"to":1},"archived_count":0,"unarchived_count":2,"search":{"show_archived_only":false,"query":"","creators":[],"bundles":["authored-parent"],"sort":"purchase_date"},"bundle_downloads":[{"id":"authored-parent","download_url":null,"zip_unavailable_reason":null}]});
        assert!(
            gumroad::parse_library_scope(&props, &base, 1, false, Some("wrong-parent")).is_err()
        );
        assert!(gumroad::parse_library_props(&props, &base, 1, false).is_err());
        let page =
            gumroad::parse_library_scope(&props, &base, 1, false, Some("authored-parent")).unwrap();
        let (card, mut parent) = bundle_parent(&page.bundles[0]);
        let bundled_child = content(
            "authored-child",
            "authored-member-purchase",
            "authored-file",
            "AUTHORED_CAPABILITY",
        );
        parent
            .member_purchase_ids
            .insert(bundled_child.purchase_id.clone());
        merge_member_files(&mut parent, bundled_child.files);
        parent.inventory_complete = true;
        let mut separate_card = page.purchases[0].clone();
        separate_card.purchase_id = "authored-separate-purchase".into();
        assert!(!parentless_purchase(
            &page.purchases[0],
            &parent.member_purchase_ids
        ));
        assert!(parentless_purchase(
            &separate_card,
            &parent.member_purchase_ids
        ));
        let separate_child = content(
            "authored-child",
            "authored-separate-purchase",
            "authored-file",
            "AUTHORED_OTHER_CAPABILITY",
        );
        let path = std::env::temp_dir().join(format!(
            "nna-authored-gumroad-bundle-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let catalogue = Catalogue::open(&path).unwrap();
        let account = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        let mut batch = RecordBatch::new(&catalogue);
        project(&mut batch, account, &card, &parent).unwrap();
        project(&mut batch, account, &separate_card, &separate_child).unwrap();
        batch.commit().unwrap();
        let acquisitions = catalogue.list("acquisition").unwrap();
        assert_eq!(acquisitions.len(), 2);
        let bundle = acquisitions
            .iter()
            .find(|row| row["sourceBundleId"] == "authored-parent")
            .unwrap();
        assert!(bundle.get("purchaseId").is_none());
        assert!(bundle["acquiredAt"].is_null() && bundle["paidPrice"].is_null());
        assert!(acquisitions
            .iter()
            .any(|row| row["purchaseId"] == "authored-separate-purchase"));
        let files = catalogue.list("remote_file").unwrap();
        assert_eq!(files.len(), 2);
        let bundled_file = files
            .iter()
            .find(|file| file["sourceBundleId"] == "authored-parent")
            .unwrap();
        assert_eq!(bundled_file["productId"], "gumroad:authored-parent");
        assert_eq!(bundled_file["sourceProductId"], "authored-child");
        assert_eq!(bundled_file["sourcePurchaseId"], "authored-member-purchase");
        assert_eq!(
            bundled_file["purchaseIds"],
            json!(["authored-member-purchase"])
        );
        let entitlement = catalogue
            .list("entitlement")
            .unwrap()
            .into_iter()
            .find(|row| row["sourceBundleId"] == "authored-parent")
            .unwrap();
        assert_eq!(entitlement["manifestComplete"], true);
        assert!(entitlement.get("purchaseId").is_none());
        for kind in ["product", "acquisition", "remote_file", "entitlement"] {
            assert!(!serde_json::to_string(&catalogue.list(kind).unwrap())
                .unwrap()
                .contains("CAPABILITY"));
        }
        drop(catalogue);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn bundle_files_keep_member_identity_and_ambiguous_duplicates_are_unavailable() {
        let mut parent = content(
            "authored-parent",
            "authored-parent-purchase",
            "authored-parent-file",
            "parent-capability",
        );
        let child = content(
            "authored-member",
            "authored-member-purchase",
            "authored-shared-file",
            "child-capability",
        );
        merge_member_files(&mut parent, child.files);
        assert_eq!(parent.files.len(), 2);
        assert_eq!(parent.files[1].source_product_id, "authored-member");
        assert_eq!(
            parent.files[1].source_purchase_id,
            "authored-member-purchase"
        );
        let mut same_id_other_product = content(
            "authored-other-member",
            "authored-other-purchase",
            "authored-shared-file",
            "other-capability",
        );
        merge_member_files(
            &mut parent,
            std::mem::take(&mut same_id_other_product.files),
        );
        assert_eq!(parent.files.len(), 3);
        let mut ambiguous = content(
            "authored-member",
            "authored-member-purchase",
            "authored-shared-file",
            "fresh-capability",
        );
        ambiguous.files[0].name = Some("different.zip".into());
        merge_member_files(&mut parent, ambiguous.files);
        assert_eq!(parent.files[1].access, FileAccess::Unavailable);
        assert!(parent.files[1].resolver.is_none());
        assert_eq!(parent.unsupported_items, 1);
        assert!(parent.acquired_at.is_some());
    }

    #[test]
    fn projection_date_and_capability_changes_keep_identity_and_never_persist_routes() {
        let path =
            std::env::temp_dir().join(format!("nna-authored-gumroad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        let catalogue = Catalogue::open(&path).unwrap();
        let account = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        let card = LibraryPurchase {
            purchase_id: "authored-purchase".into(),
            title: Some("Authored product".into()),
            author: None,
            thumbnail_url: None,
            native_type: Some("digital".into()),
            variant_name: None,
            archived: false,
            content_route: None,
        };
        let mut observation = content(
            "authored-product",
            "authored-purchase",
            "authored-file",
            "AUTHORED_SECRET_CAPABILITY",
        );
        observation.acquired_at = None;
        let mut batch = RecordBatch::new(&catalogue);
        project(&mut batch, account, &card, &observation).unwrap();
        batch.commit().unwrap();
        let first = catalogue.list("acquisition").unwrap();
        let first_file = catalogue.list("remote_file").unwrap()[0]["id"].clone();
        observation = content(
            "authored-product",
            "authored-purchase",
            "authored-file",
            "AUTHORED_NEW_SECRET_CAPABILITY",
        );
        let mut batch = RecordBatch::new(&catalogue);
        project(&mut batch, account, &card, &observation).unwrap();
        batch.commit().unwrap();
        let acquisitions = catalogue.list("acquisition").unwrap();
        assert_eq!(acquisitions.len(), 1);
        assert_eq!(acquisitions[0]["id"], first[0]["id"]);
        assert_eq!(
            acquisitions[0]["acquiredDateText"],
            "2026-10-08T01:02:03+00:00"
        );
        assert!(acquisitions[0]["paidPrice"].is_null());
        let public = gumroad::parse_public_product(&json!({"api_version":1,"id":"authored-product","price_cents":19900,"currency_code":"usd","price_formatted":"$199","is_pay_what_you_want":false,"is_recurring_billing":false}), Some("authored-product")).unwrap();
        let mut batch = RecordBatch::new(&catalogue);
        project_public(&mut batch, &observation, Some(&public)).unwrap();
        batch.commit().unwrap();
        let product = catalogue
            .get("product", "gumroad:authored-product")
            .unwrap()
            .unwrap();
        assert_eq!(product["price"], 199.0);
        assert_eq!(product["priceCurrency"], "USD");
        assert_eq!(product["priceFormatted"], "$199");
        let mut batch = RecordBatch::new(&catalogue);
        project_public(&mut batch, &observation, None).unwrap();
        batch.commit().unwrap();
        let product = catalogue
            .get("product", "gumroad:authored-product")
            .unwrap()
            .unwrap();
        assert_eq!(product["price"], 199.0);
        assert_eq!(product["priceRefreshState"], "unavailable");
        observation.variant_id = Some("authored-variant".into());
        let mut batch = RecordBatch::new(&catalogue);
        project_public(&mut batch, &observation, None).unwrap();
        batch.commit().unwrap();
        let variant = catalogue
            .get("product", "gumroad:authored-product")
            .unwrap()
            .unwrap();
        assert!(variant["price"].is_null() && variant["priceFormatted"].is_null());
        assert_eq!(variant["sourcePriceCents"], 19900);
        observation.variant_id = None;
        assert!(catalogue.list("acquisition").unwrap()[0]["paidPrice"].is_null());
        let files = catalogue.list("remote_file").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["id"], first_file);
        let mut reference = observation.files[0].clone();
        reference.file_id = "authored-external-reference".into();
        reference.access = FileAccess::ExternalReference;
        reference.resolver = None;
        observation.files.push(reference.clone());
        let mut batch = RecordBatch::new(&catalogue);
        project(&mut batch, account, &card, &observation).unwrap();
        batch.commit().unwrap();
        let entitlement = catalogue.list("entitlement").unwrap().remove(0);
        assert_eq!(entitlement["manifestComplete"], true);
        assert_eq!(entitlement["fileIds"], json!([first_file]));
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 1);

        let mut external_card = card.clone();
        external_card.purchase_id = "authored-external-purchase".into();
        observation.purchase_id = external_card.purchase_id.clone();
        observation.product_id = "authored-external-product".into();
        reference.source_purchase_id = observation.purchase_id.clone();
        reference.source_product_id = observation.product_id.clone();
        observation.files = vec![reference];
        let mut batch = RecordBatch::new(&catalogue);
        project(&mut batch, account, &external_card, &observation).unwrap();
        batch.commit().unwrap();
        let entitlement = catalogue
            .list("entitlement")
            .unwrap()
            .into_iter()
            .find(|row| row["productId"] == "gumroad:authored-external-product")
            .unwrap();
        assert_eq!(entitlement["manifestComplete"], true);
        assert_eq!(entitlement["fileIds"], json!([]));
        assert_eq!(catalogue.list("remote_file").unwrap().len(), 1);
        assert!(catalogue
            .get("product", "gumroad:authored-external-product")
            .unwrap()
            .is_some());
        assert!(catalogue
            .list("acquisition")
            .unwrap()
            .iter()
            .any(
                |row| row["productId"] == "gumroad:authored-external-product"
                    && row["acquiredAt"].is_string()
            ));
        for kind in ["product", "acquisition", "entitlement", "remote_file"] {
            let values = catalogue.list(kind).unwrap();
            let serialized = serde_json::to_string(&values).unwrap();
            assert!(!serialized.contains("SECRET_CAPABILITY"));
            assert!(!serialized.contains("/d/") && !serialized.contains("/r/"));
        }
        drop(catalogue);
        let _ = std::fs::remove_dir_all(path);
    }
}
