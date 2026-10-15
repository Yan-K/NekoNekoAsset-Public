mod auth;
mod batch;
pub(crate) use batch::RecordBatch;
mod diagnostics;
mod fetch;
mod free_item;
mod http;
mod incremental;
mod metadata;
pub(crate) use metadata::effective_public_page_availability;
pub(crate) use metadata::{listing_price_display, listing_starting_price, parse_jpy_price};
mod parser;
mod safety;
mod sync;
#[cfg(test)]
mod tests;

use crate::catalogue::Catalogue;
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Emitter, Manager};

/// Ephemeral capability. No Debug/Serialize implementation: never persist signed URLs.
pub struct DownloadTicket {
    pub url: url::Url,
    pub client: reqwest::Client,
    pub expected_size: Option<u64>,
}
/// Storage validates the actual final streamed response again; a ticket is not proof of bytes.
pub fn validate_download_response(response: &reqwest::Response) -> Result<()> {
    if !safety::download_destination(response.url()) {
        bail!("UNSAFE_DOWNLOAD_URL");
    }
    if response.url().path().contains("sign_in") || response.url().path().contains("login") {
        bail!("AUTH_REQUIRED");
    }
    match response.status().as_u16() {
        401 => bail!("AUTH_REQUIRED"),
        403 if response.url().host_str().is_some_and(safety::booth_host) => {
            bail!("PERMISSION_DENIED")
        }
        403 => bail!("SIGNED_LINK_EXPIRED"),
        404 | 410 => bail!("FILE_UNAVAILABLE"),
        429 => bail!("RATE_LIMITED"),
        500..=599 => bail!("DOWNLOAD_RESOLVER_FAILED"),
        _ => {}
    }
    if !response.status().is_success() {
        bail!("DOWNLOAD_RESOLVER_FAILED");
    }
    let attachment = response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("attachment"));
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/html"));
    if html && !attachment {
        bail!("DOWNLOAD_LINK_UNAVAILABLE");
    }
    Ok(())
}
#[derive(Clone)]
pub struct BoothService {
    inner: Arc<Inner>,
}
struct Inner {
    catalogue: Catalogue,
    http: http::MetadataHttp,
    running: Mutex<BTreeSet<String>>,
    execution: tokio::sync::Mutex<()>,
    recovering: Mutex<bool>,
    last_progress: Mutex<Option<(String, std::time::Instant)>>,
    _process_lock: Arc<std::fs::File>,
}
fn field<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("INVALID_ARGUMENT_{key}"))
}
fn diagnostic_source(args: &Value) -> Result<&'static str> {
    match args.get("source") {
        None => Ok("library"),
        Some(value) => match value.as_str() {
            Some("library") => Ok("library"),
            Some("purchase") => Ok("purchase"),
            Some("gift") => Ok("gift"),
            Some("free") => Ok("free"),
            _ => bail!("INVALID_SOURCE"),
        },
    }
}
fn entry_diagnostic_account(args: &Value) -> Result<&str> {
    if !args
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("accountId"))
    {
        bail!("INVALID_DIAGNOSTIC_ARGUMENTS");
    }
    args["accountId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("INVALID_DIAGNOSTIC_ARGUMENTS"))
}
fn product_line_diagnostic_args(args: &Value) -> Result<(&str, &str)> {
    if !args.as_object().is_some_and(|object| {
        object.len() == 2 && object.contains_key("accountId") && object.contains_key("receiptId")
    }) {
        bail!("INVALID_DIAGNOSTIC_ARGUMENTS");
    }
    let account = args["accountId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("INVALID_DIAGNOSTIC_ARGUMENTS"))?;
    let receipt = args["receiptId"]
        .as_str()
        .filter(|value| safety::numeric(value))
        .ok_or_else(|| anyhow::anyhow!("INVALID_DIAGNOSTIC_ARGUMENTS"))?;
    Ok((account, receipt))
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn code(error: &anyhow::Error) -> String {
    let value = error.to_string();
    if value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    {
        value
    } else {
        "BOOTH_OPERATION_FAILED".into()
    }
}
impl BoothService {
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
            .open(catalogue.device_dir().join("booth.lock"))
            .map_err(|_| anyhow::anyhow!("BOOTH_LOCK_UNAVAILABLE"))?;
        fs2::FileExt::try_lock_exclusive(&process_lock)
            .map_err(|_| anyhow::anyhow!("BOOTH_ALREADY_RUNNING"))?;
        Self::with_process_lock(catalogue, Arc::new(process_lock))
    }
    pub(crate) fn rebind_catalogue(&self, catalogue: Catalogue) -> Result<Self> {
        ensure!(catalogue.device_dir() == self.inner.catalogue.device_dir(), "BOOTH_DEVICE_MISMATCH");
        Self::with_process_lock(catalogue, self.inner._process_lock.clone())
    }
    fn with_process_lock(catalogue: Catalogue, process_lock: Arc<std::fs::File>) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                catalogue,
                http: http::MetadataHttp::new(),
                running: Mutex::new(BTreeSet::new()),
                execution: tokio::sync::Mutex::new(()),
                recovering: Mutex::new(false),
                last_progress: Mutex::new(None),
                _process_lock: process_lock,
            }),
        })
    }
    fn changed(&self, app: &AppHandle, job: Option<&str>) {
        let _ = app.emit("nna:changed", json!({"scope":"booth","jobId":job}));
    }
    fn known_account(&self, id: &str) -> Result<Value> {
        safety::account_id(id)?;
        self.inner
            .catalogue
            .get("account", id)?
            .filter(|a| a["store"] == "BOOTH")
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))
    }
    fn account(&self, id: &str) -> Result<Value> {
        let account = self.known_account(id)?;
        if account["disconnected"] == true {
            bail!("ACCOUNT_UNAVAILABLE");
        }
        Ok(account)
    }
    /// Lead calls this once at startup; interrupted syncs resume through checkpoint validation.
    pub async fn start(&self, app: AppHandle) -> Result<()> {
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
        for mut job in self.inner.catalogue.list("job")? {
            if !matches!(
                job["type"].as_str(),
                Some("boothSync" | "boothUpdate" | "boothLogin")
            ) || !matches!(job["state"].as_str(), Some("running" | "queued"))
            {
                continue;
            }
            let id = field(&job, "id")?.to_owned();
            if self.inner.catalogue.library_root().is_some()
                && job["originDeviceId"] != self.inner.catalogue.device_id()
            {
                job["state"] = json!("needsAction");
                job["errorCode"] = json!("RETRY_REQUIRED");
                job["updatedAt"] = json!(now());
                self.inner.catalogue.put("job", &id, job)?;
                continue;
            }
            if job["type"] == "boothLogin" {
                job["state"] = json!("needsAction");
                job["errorCode"] = json!("RETRY_REQUIRED");
                job["updatedAt"] = json!(now());
                self.inner.catalogue.put("job", &id, job.clone())?;
                if let Some(account) = job["accountId"].as_str() {
                    self.inner.catalogue.discard_unverified_account(account)?;
                }
                continue;
            }
            if let Some(account) = job["accountId"].as_str() {
                let record = self.known_account(account)?;
                if !crate::stores::has_local_session(&self.inner.catalogue, &record) {
                    job["state"] = json!("needsAction");
                    job["errorCode"] = json!("AUTH_REQUIRED");
                    job["updatedAt"] = json!(now());
                    self.inner.catalogue.put("job", &id, job)?;
                    continue;
                }
            }
            self.spawn_job(app.clone(), job)?;
        }
        Ok(())
    }
    pub async fn command(&self, app: &AppHandle, action: &str, args: Value) -> Result<Value> {
        let _work = if matches!(
            action,
            "account_add"
                | "login"
                | "account_login"
                | "account_logout"
                | "account_remove"
                | "sync"
                | "cancel_sync"
                | "retry_sync"
                | "update_products"
        ) {
            Some(self.inner.catalogue.begin_catalogue_work()?)
        } else {
            None
        };
        match action {
            "diagnose_product_line_fields" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                let (account, receipt) = product_line_diagnostic_args(&args)?;
                serde_json::to_value(
                    self.diagnose_product_line_fields(app, account, receipt)
                        .await?,
                )
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "diagnose_receipt_contact_envelope" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                serde_json::to_value(
                    self.diagnose_receipt_contact_envelope(app, entry_diagnostic_account(&args)?)
                        .await?,
                )
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "diagnose_receipt_notice_envelope" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                serde_json::to_value(
                    self.diagnose_receipt_notice_envelope(app, entry_diagnostic_account(&args)?)
                        .await?,
                )
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "diagnose_receipt_template" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                serde_json::to_value(
                    self.diagnose_receipt_template(app, entry_diagnostic_account(&args)?)
                        .await?,
                )
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "diagnose_free_item_identity" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                serde_json::to_value(
                    self.diagnose_free_item_identity(app, entry_diagnostic_account(&args)?)
                        .await?,
                )
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "diagnose_free_item_entry"
            | "diagnose_free_item_role"
            | "diagnose_free_item_controls" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                let issue = match action {
                    "diagnose_free_item_role" => sync::FreeItemDiagnostic::Role,
                    "diagnose_free_item_controls" => sync::FreeItemDiagnostic::Controls,
                    _ => sync::FreeItemDiagnostic::Entry,
                };
                serde_json::to_value(
                    self.diagnose_free_item_entry(app, entry_diagnostic_account(&args)?, issue)
                        .await?,
                )
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "fetch_product_metadata" => {
                serde_json::to_value(self.fetch_product_metadata(field(&args, "url")?).await?)
                    .map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))
            }
            "diagnose_ordering" | "diagnose_manifests" | "diagnose_file_structure" => {
                if !cfg!(debug_assertions) || std::env::var_os("NNA_DATA_DIR").is_none() {
                    bail!("DIAGNOSTICS_UNAVAILABLE");
                }
                let account = field(&args, "accountId")?;
                let source = if action == "diagnose_ordering" {
                    "library"
                } else {
                    diagnostic_source(&args)?
                };
                let route = format!("{}?page=1", safety::source_path(source)?);
                let client = self.client(app, account, false).await?;
                let (status, body) = self
                    .inner
                    .http
                    .get(&client, safety::route(&route, source)?)
                    .await?;
                if !status.is_success() {
                    bail!("SOURCE_HTTP_FAILED");
                }
                // The transient body and native session never cross the command boundary.
                if action == "diagnose_file_structure" {
                    serde_json::to_value(parser::diagnose_file_structure(&body, source, &route)?)
                } else if action == "diagnose_manifests" {
                    serde_json::to_value(parser::diagnose_manifests(&body, source, &route)?)
                } else {
                    serde_json::to_value(diagnostics::inspect(&body))
                }
                .map_err(|_| anyhow::anyhow!("DIAGNOSTICS_FAILED"))
            }
            "account_add" => {
                if args
                    .get("store")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s != "BOOTH")
                {
                    bail!("STORE_UNAVAILABLE");
                }
                let id = uuid::Uuid::new_v4().to_string();
                let name = args["name"]
                    .as_str()
                    .or_else(|| args["label"].as_str())
                    .map(safety::short)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "BOOTH".into());
                self.inner.catalogue.put("account",&id,json!({"id":id,"store":"BOOTH","name":name,"label":name,"state":"needsAction","status":"disconnected","identityVerified":false,"createdAt":now()}))?;
                self.changed(app, None);
                Ok(json!({"accountId":id}))
            }
            "login" | "account_login" => {
                let account = field(&args, "accountId")?.to_owned();
                self.known_account(&account)?;
                auth::window(app, &self.inner.catalogue.device_dir(), &account, true).await?;
                let job = self.make_job("boothLogin", json!({"accountId":account}))?;
                self.spawn_job(app.clone(), job.clone())?;
                Ok(json!({"jobId":job["id"]}))
            }
            "account_logout" | "account_remove" => {
                let id = field(&args, "accountId")?;
                self.known_account(id)?;
                if self
                    .inner
                    .running
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                    .contains(&format!("account:{id}"))
                {
                    bail!("ACCOUNT_BUSY");
                }
                auth::disconnect(app, &self.inner.catalogue.device_dir(), id).await?;
                if action == "account_remove" {
                    let result = self
                        .inner
                        .catalogue
                        .remove_account(id, args["removeProducts"] == true)?;
                    self.changed(app, None);
                    return Ok(result);
                }
                let mut a = self.known_account(id)?;
                if self.inner.catalogue.library_root().is_none() {
                    a["disconnected"] = json!(true);
                    a["state"] = json!("disconnected");
                    a["status"] = json!("disconnected");
                    self.inner.catalogue.put("account", id, a)?;
                }
                self.changed(app, None);
                Ok(json!({"accountId":id}))
            }
            "sync" => {
                let id = field(&args, "accountId")?;
                self.account(id)?;
                if let Some(existing) = self.inner.catalogue.list("job")?.into_iter().find(|job| {
                    job["accountId"] == id
                        && matches!(job["type"].as_str(), Some("boothSync" | "boothUpdate"))
                        && matches!(job["state"].as_str(), Some("queued" | "running"))
                }) {
                    return Ok(json!({"jobId":existing["id"]}));
                }
                let source = args["source"].as_str().unwrap_or("all");
                if source != "all" {
                    safety::source_name(source)?;
                }
                let mode = args["mode"].as_str().unwrap_or("incremental");
                if !matches!(mode, "incremental" | "full" | "forced") {
                    bail!("INVALID_SYNC_MODE");
                }
                let job = self.make_job(
                    "boothSync",
                    json!({"accountId":id,"source":source,"mode":mode}),
                )?;
                self.spawn_job(app.clone(), job.clone())?;
                Ok(json!({"jobId":job["id"]}))
            }
            "cancel_sync" => {
                let id = field(&args, "jobId")?;
                let mut job = self
                    .inner
                    .catalogue
                    .get("job", id)?
                    .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                if !matches!(
                    job["type"].as_str(),
                    Some("boothSync" | "boothUpdate" | "boothLogin")
                ) {
                    bail!("INVALID_JOB");
                }
                if matches!(job["state"].as_str(), Some("queued" | "running")) {
                    job["state"] = json!("cancelled");
                    job["updatedAt"] = json!(now());
                    job["completedAt"] = json!(now());
                    self.inner.catalogue.put("job", id, job)?;
                }
                self.changed(app, Some(id));
                Ok(json!({"jobId":id}))
            }
            "retry_sync" => {
                let id = field(&args, "jobId")?;
                let mut job = self
                    .inner
                    .catalogue
                    .get("job", id)?
                    .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                if !matches!(
                    job["type"].as_str(),
                    Some("boothSync" | "boothUpdate" | "boothLogin")
                ) {
                    bail!("INVALID_JOB");
                }
                if !matches!(
                    job["state"].as_str(),
                    Some("failed" | "needsAction" | "cancelled")
                ) {
                    bail!("JOB_BUSY");
                }
                let key = job["accountId"]
                    .as_str()
                    .map(|id| format!("account:{id}"))
                    .unwrap_or_else(|| "booth-global".into());
                if self
                    .inner
                    .running
                    .lock()
                    .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?
                    .contains(&key)
                {
                    bail!("ACCOUNT_BUSY");
                }
                if job["type"] == "boothLogin" {
                    let account = field(&job, "accountId")?;
                    self.known_account(account)?;
                    auth::window(app, &self.inner.catalogue.device_dir(), account, true).await?;
                }
                crate::support::reset_attempt(&mut job);
                job["state"] = json!("queued");
                job["errorCode"] = Value::Null;
                job["completedAt"] = Value::Null;
                job["updatedAt"] = json!(now());
                job["originDeviceId"] = json!(self.inner.catalogue.device_id());
                self.inner.catalogue.put("job", id, job.clone())?;
                self.spawn_job(app.clone(), job)?;
                Ok(json!({"jobId":id}))
            }
            "update_products" => {
                let ids = args["productIds"]
                    .as_array()
                    .or_else(|| args["ids"].as_array())
                    .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))?;
                let ids: Vec<_> = ids.iter().filter_map(Value::as_str).collect();
                if ids.is_empty() || ids.iter().any(|id| !safety::numeric(id)) {
                    bail!("INVALID_PRODUCT_IDS");
                }
                let job = self.make_job("boothUpdate", json!({"productIds":ids}))?;
                self.spawn_job(app.clone(), job.clone())?;
                Ok(json!({"jobId":job["id"]}))
            }
            _ => bail!("UNKNOWN_BOOTH_ACTION"),
        }
    }
    fn make_job(&self, kind: &str, args: Value) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let account_name = args["accountId"]
            .as_str()
            .and_then(|account| self.known_account(account).ok())
            .and_then(|account| account["name"].as_str().map(str::to_owned));
        let mut job = json!({"id":id,"type":kind,"store":"BOOTH","accountId":args["accountId"],"accountName":account_name,"args":args,"state":"queued","progress":{"stage":"collecting","pages":0,"products":0},"createdAt":now(),"updatedAt":now(),"originDeviceId":self.inner.catalogue.device_id()});
        crate::support::reset_attempt(&mut job);
        self.inner.catalogue.put("job", &id, job.clone())?;
        Ok(job)
    }
    fn spawn_job(&self, app: AppHandle, mut job: Value) -> Result<()> {
        let work = self.inner.catalogue.begin_catalogue_work()?;
        let id = field(&job, "id")?.to_owned();
        let key = job["accountId"]
            .as_str()
            .map(|a| format!("account:{a}"))
            .unwrap_or_else(|| "booth-global".into());
        {
            let mut running = self
                .inner
                .running
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            if running.contains(&key)
                || running.contains("booth-global")
                || (key == "booth-global" && !running.is_empty())
            {
                let login_finishing = job["type"] == "boothSync"
                    && self.inner.catalogue.list("job")?.iter().any(|active| {
                        active["accountId"] == job["accountId"]
                            && active["type"] == "boothLogin"
                            && matches!(
                                active["state"].as_str(),
                                Some("queued" | "running" | "completed")
                            )
                    });
                if login_finishing && running.contains(&key) {
                    let service = self.clone();
                    tauri::async_runtime::spawn(async move {
                        let _work = work;
                        loop {
                            match service.cancelled(&job) {
                                Ok(true) => {
                                    if let Err(error) = service.finish_job(&job, Some("CANCELLED"))
                                    {
                                        eprintln!("BOOTH terminal write failed: {}", code(&error));
                                    }
                                    service.changed(&app, Some(&id));
                                    return;
                                }
                                Err(_) => {
                                    if let Err(error) =
                                        service.finish_job(&job, Some("JOB_UNAVAILABLE"))
                                    {
                                        eprintln!("BOOTH terminal write failed: {}", code(&error));
                                    }
                                    service.changed(&app, Some(&id));
                                    return;
                                }
                                Ok(false) => {}
                            }
                            let busy = service
                                .inner
                                .running
                                .lock()
                                .map(|active| active.contains(&key))
                                .unwrap_or(true);
                            if !busy {
                                if let Err(error) = service.spawn_job(app.clone(), job.clone()) {
                                    if let Err(error) =
                                        service.finish_job(&job, Some(&code(&error)))
                                    {
                                        eprintln!("BOOTH terminal write failed: {}", code(&error));
                                    }
                                }
                                service.changed(&app, Some(&id));
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    });
                    return Ok(());
                }
                job["state"] = json!("needsAction");
                job["errorCode"] = json!("ACCOUNT_BUSY");
                job["completedAt"] = json!(now());
                self.inner.catalogue.put("job", &id, job)?;
                bail!("ACCOUNT_BUSY");
            }
            running.insert(key.clone());
        }
        let service = self.clone();
        tauri::async_runtime::spawn(async move {
            let _work = work;
            // Accounts of this store share one execution queue. Other providers
            // own independent gates and continue concurrently.
            let _execution = service.inner.execution.lock().await;
            crate::support::begin_attempt(&mut job);
            let (result, diagnostics) = crate::support_trace::capture(async {
                crate::support_trace::job_stage(&job);
                service.execute(&app, &mut job).await
            }).await;
            job["supportDiagnostics"] = diagnostics;
            crate::support::end_attempt(&mut job);
            let mut error_code = result.as_ref().err().map(code);
            // Complete login window handling before exposing the terminal job or
            // releasing this account. Verified credentials remain device-local.
            if job["type"] == "boothLogin" {
                if let Some(account) = job["accountId"].as_str() {
                    if crate::stores::session::finish_login(
                        &app,
                        crate::stores::StoreProvider::Booth,
                        account,
                        result.is_ok(),
                    )
                    .await
                    .is_err()
                    {
                        error_code = Some("AUTH_WINDOW_FAILED".into());
                    }
                }
            }
            if let Err(error) = service.finish_job(&job, error_code.as_deref()) {
                // Fixed code only: a failed durable terminal write is never
                // treated as successful completion or logged with HTTP data.
                eprintln!("BOOTH terminal write failed: {}", code(&error));
                let _ = app.emit(
                    "nna:changed",
                    json!({"scope":"jobs","jobId":id,"errorCode":"TERMINAL_WRITE_FAILED"}),
                );
            }
            if let Ok(mut running) = service.inner.running.lock() {
                running.remove(&key);
            }
            if job["type"] == "boothLogin" {
                if let Some(account) = job["accountId"].as_str() {
                    let _ = service.inner.catalogue.discard_unverified_account(account);
                }
                if service.inner.catalogue.get("job", &id).ok().flatten()
                    .is_some_and(|terminal| terminal["state"] == "completed")
                {
                    let _ = app.emit("nna:changed", json!({"scope":"booth","sessionChanged":true}));
                }
            }
            service.changed(&app, Some(&id));
        });
        Ok(())
    }
    fn finish_job(&self, job: &Value, error: Option<&str>) -> Result<()> {
        let id = field(job, "id")?;
        if error == Some("AUTH_REQUIRED") && self.inner.catalogue.library_root().is_some() {
            if let Some(account) = job["accountId"].as_str() {
                crate::stores::session::forget(
                    &self.inner.catalogue.device_dir(),
                    crate::stores::StoreProvider::Booth,
                    account,
                )?;
            }
        }
        for _ in 0..4 {
            let mut batch = batch::RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", id)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            // Source scanners report through local job copies. Keep the latest
            // durable counters when cancellation/failure ends the parent task.
            let mut terminal = current.clone();
            if let Some(diagnostics) = job.get("supportDiagnostics") {
                terminal["supportDiagnostics"] = diagnostics.clone();
            }
            if let Some(capture) = job.get("supportCapture") {
                terminal["supportCapture"] = capture.clone();
            }
            terminal["supportLastStage"] = json!(crate::support::operation_stage(terminal["progress"]["stage"].as_str().unwrap_or("")));
            let error = if current["state"] == "cancelled" {
                Some("CANCELLED")
            } else {
                error
            };
            terminal["state"] = json!(match error {
                None => "completed",
                Some("CANCELLED") => "cancelled",
                Some(
                    "AUTH_REQUIRED"
                    | "AUTH_CHALLENGE"
                    | "PERMISSION_DENIED"
                    | "RATE_LIMITED"
                    | "SYNC_INCOMPLETE"
                    | "COLLECTOR_UNSUPPORTED"
                    | "IMPORT_TOO_LARGE"
                    | "AUTH_TIMEOUT"
                    | "SOURCE_UNSUPPORTED"
                    | "CONCURRENT_UPDATE"
                    | "PUBLIC_METADATA_FAILED"
                    | "PUBLIC_METADATA_INVALID",
                ) => "needsAction",
                Some(_) => "failed",
            });
            terminal["errorCode"] = json!(error);
            terminal["updatedAt"] = json!(now());
            terminal["completedAt"] = json!(now());
            terminal["progress"]["stage"] = json!(terminal["state"].as_str().unwrap());
            if job["type"] == "boothSync" {
                if let Some(account_id) = job["accountId"].as_str() {
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
                        if error == Some("AUTH_REQUIRED")
                            && self.inner.catalogue.library_root().is_none()
                        {
                            account["state"] = json!("expired");
                            account["status"] = json!("expired");
                        }
                        batch.stage("account", account_id, account)?;
                    }
                    for source in ["library", "purchase", "gift", "free"] {
                        let key = format!("{account_id}:{source}");
                        if let Some(mut coverage) = batch.get("source_coverage", &key)? {
                            if error.is_some()
                                && matches!(
                                    coverage["status"].as_str(),
                                    Some("running" | "collected")
                                )
                            {
                                coverage["status"] = json!("incomplete");
                                coverage["coverageComplete"] = json!(false);
                                coverage["errorCode"] = json!(error);
                                coverage["completedAt"] = terminal["completedAt"].clone();
                                batch.stage("source_coverage", &key, coverage)?;
                            }
                        }
                    }
                }
            }
            batch.stage("job", id, terminal)?;
            if batch.try_commit()?.is_some() {
                if matches!(job["type"].as_str(), Some("boothSync" | "boothUpdate")) {
                    self.inner.catalogue.refresh_avatar_defaults()?;
                }
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }
    fn cancelled(&self, job: &Value) -> Result<bool> {
        Ok(self
            .inner
            .catalogue
            .get("job", field(job, "id")?)?
            .is_some_and(|j| j["state"] == "cancelled"))
    }
    fn progress(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        crate::support_trace::job_stage(job);
        if let Some(diagnostics) = crate::support_trace::snapshot() {
            job["supportDiagnostics"] = diagnostics;
        }
        job["state"] = json!("running");
        job["updatedAt"] = json!(now());
        let mut committed = false;
        for _ in 0..4 {
            let mut batch = batch::RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get("job", field(job, "id")?)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            if current["state"] == "cancelled" {
                bail!("CANCELLED");
            }
            // Concurrent source scanners hold local copies of this same job.
            // Their publication order must not move durable aggregate counters back.
            if job["type"] == "boothSync" {
                for field in [
                    "pages",
                    "detailsCompleted",
                    "requestsCompleted",
                    "published",
                ] {
                    if let Some(saved) = current["progress"][field].as_u64() {
                        if job["progress"][field].as_u64().unwrap_or(0) < saved {
                            job["progress"][field] = json!(saved);
                        }
                    }
                }
            }
            batch.stage("job", field(job, "id")?, job.clone())?;
            if batch.try_commit()?.is_some() {
                committed = true;
                break;
            }
        }
        if !committed {
            bail!("CONCURRENT_UPDATE");
        }
        let id = field(job, "id")?;
        let should_emit = {
            let mut last = self
                .inner
                .last_progress
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            let emit = last.as_ref().is_none_or(|(previous, at)| {
                previous != id || at.elapsed() >= std::time::Duration::from_millis(250)
            });
            if emit {
                *last = Some((id.to_owned(), std::time::Instant::now()));
            }
            emit
        };
        if should_emit {
            let _ = app.emit("nna:changed", json!({"scope":"jobs","jobId":id}));
        }
        Ok(())
    }
    async fn execute(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        self.progress(app, job)?;
        match job["type"].as_str().unwrap_or("") {
            "boothSync" => self.sync_job(app, job).await,
            "boothUpdate" => self.update_job(app, job).await,
            "boothLogin" => self.login_job(app, job).await,
            _ => bail!("INVALID_JOB"),
        }
    }
    async fn login_job(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        let id = field(&job["args"], "accountId")?.to_owned();
        let window = app
            .get_webview_window(&format!("booth-auth-{id}"))
            .ok_or_else(|| anyhow::anyhow!("CANCELLED"))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
        self.login_stage(app, job, "cookieAccess", None)?;
        while tokio::time::Instant::now() < deadline {
            if self.cancelled(job)? || app.get_webview_window(window.label()).is_none() {
                bail!("CANCELLED");
            }
            match auth::jar(window.clone()).await {
                Err(error) => {
                    let error_code = code(&error);
                    if error_code != "AUTH_REQUIRED" {
                        self.login_stage(app, job, "cookieAccess", Some("COOKIE_ACCESS_FAILED"))?;
                        bail!("COOKIE_ACCESS_FAILED");
                    }
                    self.login_stage(app, job, "authRequired", Some("AUTH_REQUIRED"))?;
                }
                Ok(jar) => {
                    let client = http::build_client(jar, false)?;
                    match self
                        .inner
                        .http
                        .get(&client, safety::route("/library?page=1", "library")?)
                        .await
                    {
                        Err(error) => {
                            let error_code = code(&error);
                            self.login_stage(
                                app,
                                job,
                                if matches!(error_code.as_str(), "AUTH_REQUIRED" | "AUTH_CHALLENGE")
                                {
                                    "authRequired"
                                } else {
                                    "cookieAccess"
                                },
                                Some(&error_code),
                            )?;
                            if !matches!(error_code.as_str(), "AUTH_REQUIRED" | "AUTH_CHALLENGE") {
                                return Err(error);
                            }
                        }
                        Ok((status, body)) => {
                            if !status.is_success() {
                                self.login_stage(
                                    app,
                                    job,
                                    "cookieAccess",
                                    Some("SOURCE_HTTP_FAILED"),
                                )?;
                                bail!("SOURCE_HTTP_FAILED");
                            }
                            match parser::parse(&body, "library", "/library?page=1") {
                                Ok(_) => {
                                    self.login_stage(app, job, "httpSuccess", None)?;
                                    self.login_stage(app, job, "parserRecognition", None)?;
                                    let mut batch = batch::RecordBatch::new(&self.inner.catalogue);
                                    let current_job = batch.get("job", field(job, "id")?)?;
                                    if current_job.is_none_or(|row| {
                                        !matches!(row["state"].as_str(), Some("queued" | "running"))
                                    }) || app.get_webview_window(window.label()).is_none()
                                    {
                                        bail!("CANCELLED");
                                    }
                                    let mut account = batch
                                        .get("account", &id)?
                                        .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
                                    account["disconnected"] = json!(false);
                                    account["state"] = json!("connected");
                                    account["status"] = json!("connected");
                                    account["authenticatedAt"] = json!(now());
                                    batch.stage("account", &id, account)?;
                                    if batch.try_commit()?.is_none() {
                                        if self.cancelled(job)? {
                                            bail!("CANCELLED");
                                        }
                                        bail!("CONCURRENT_UPDATE");
                                    }
                                    crate::stores::session::remember(
                                        window.clone(),
                                        &self.inner.catalogue.device_dir(),
                                        crate::stores::StoreProvider::Booth,
                                        &id,
                                    )
                                    .await?;
                                    return Ok(());
                                }
                                Err(error) => {
                                    let error_code = code(&error);
                                    if matches!(
                                        error_code.as_str(),
                                        "AUTH_REQUIRED" | "AUTH_CHALLENGE"
                                    ) {
                                        self.login_stage(
                                            app,
                                            job,
                                            "authRequired",
                                            Some(&error_code),
                                        )?;
                                    } else {
                                        // A successful HTTP response is not authentication evidence.
                                        // Unsupported DOM keeps the account unverified and exposes only a fixed code.
                                        self.login_stage(app, job, "httpSuccess", None)?;
                                        self.login_stage(
                                            app,
                                            job,
                                            "parserRecognition",
                                            Some("SOURCE_UNSUPPORTED"),
                                        )?;
                                        bail!("SOURCE_UNSUPPORTED");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        bail!("AUTH_TIMEOUT")
    }
    fn login_stage(
        &self,
        app: &AppHandle,
        job: &mut Value,
        stage: &str,
        error: Option<&str>,
    ) -> Result<()> {
        if job["progress"]["loginStage"] == stage
            && job["progress"]["lastErrorCode"].as_str() == error
        {
            return Ok(());
        }
        job["progress"]["loginStage"] = json!(stage);
        job["progress"]["lastErrorCode"] = json!(error);
        self.progress(app, job)
    }
    async fn client(
        &self,
        app: &AppHandle,
        account: &str,
        download: bool,
    ) -> Result<reqwest::Client> {
        let record = self.account(account)?;
        if !crate::stores::has_local_session(&self.inner.catalogue, &record) {
            bail!("AUTH_REQUIRED");
        }
        let window = auth::window(app, &self.inner.catalogue.device_dir(), account, false).await?;
        let jar = match auth::jar(window.clone()).await {
            Ok(jar) => jar,
            Err(error) if code(&error) == "AUTH_REQUIRED" => {
                // The reopened hidden profile may still be loading its persisted cookies.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                auth::jar(window).await?
            }
            Err(error) => return Err(error),
        };
        http::build_client(jar, download)
    }
    pub async fn download_ticket(&self, app: &AppHandle, file_id: &str) -> Result<DownloadTicket> {
        let file = self
            .inner
            .catalogue
            .get("remote_file", file_id)?
            .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
        let account = field(&file, "accountId")?;
        let id = field(&file, "sourceFileId")?;
        let (resolver_id, _, _) = safety::resolver(field(&file, "sourceUrl")?)
            .ok_or_else(|| anyhow::anyhow!("INVALID_RESOLVER"))?;
        let variant = file["variantId"].as_str();
        if !safety::numeric(id) || resolver_id != id || variant.is_some_and(|v| !safety::numeric(v))
        {
            bail!("INVALID_RESOLVER");
        }
        let route = format!(
            "https://booth.pm/downloadables/{id}{}",
            variant
                .map(|v| format!("?variation_id={v}"))
                .unwrap_or_default()
        );
        let client = self.client(app, account, true).await?;
        // Resolve anew for each attempt; signed destinations are never catalogue fields.
        let response = self
            .inner
            .http
            .resolve(&client, url::Url::parse(&route).unwrap())
            .await?;
        validate_download_response(&response)?;
        let final_url = response.url().clone();
        // The first body is dropped; storage streams a fresh GET to the ephemeral destination.
        // Only this response proves the current size; saved listing bytes can
        // describe an older offered revision when delivery is chunked.
        let expected_size = response.content_length();
        let current = self
            .inner
            .catalogue
            .get("remote_file", file_id)?
            .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
        if current["accountId"] != file["accountId"]
            || current["sourceFileId"] != file["sourceFileId"]
            || current["variantId"] != file["variantId"]
            || current["productId"] != file["productId"]
        {
            bail!("SOURCE_CHANGED");
        }
        if current["revisionBasis"] != "source" {
            if let Some(etag) = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.starts_with('"') && v.ends_with('"') && v.len() <= 1024)
            {
                // Strong server content validator is positive revision evidence; its opaque value is hashed.
                let mut current = current.clone();
                current["revision"] = json!(format!("etag:{}", safety::hash(etag)));
                current["revisionBasis"] = json!("etag");
                current["requiresContentVerification"] = json!(false);
                current["sizeBytes"] = json!(expected_size);
                self.inner.catalogue.put("remote_file", file_id, current)?;
            }
        }
        Ok(DownloadTicket {
            url: final_url,
            client,
            expected_size,
        })
    }
}
