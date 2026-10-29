//! Small store dispatch boundary. Provider collectors retain their own source contracts.
pub mod gumroad;
mod gumroad_service;
pub mod http;
pub mod jinxxy;
mod public_metadata;
pub mod session;
pub(crate) mod session_cache;

use crate::{booth::BoothService, catalogue::Catalogue};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum StoreProvider {
    #[serde(rename = "BOOTH", alias = "booth")]
    Booth,
    #[serde(rename = "Gumroad", alias = "gumroad")]
    Gumroad,
    #[serde(rename = "Jinxxy", alias = "jinxxy")]
    Jinxxy,
}

impl StoreProvider {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "BOOTH" | "booth" => Ok(Self::Booth),
            "Gumroad" | "gumroad" => Ok(Self::Gumroad),
            "Jinxxy" | "jinxxy" => Ok(Self::Jinxxy),
            _ => bail!("STORE_UNAVAILABLE"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Booth => "BOOTH",
            Self::Gumroad => "Gumroad",
            Self::Jinxxy => "Jinxxy",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Booth => "booth",
            Self::Gumroad => "gumroad",
            Self::Jinxxy => "jinxxy",
        }
    }
}

/// Transient capability only. Signed URLs and clients never enter source records or IPC.
/// Intentionally has no Debug or Serialize implementation.
pub struct DownloadTicket {
    pub provider: StoreProvider,
    pub url: url::Url,
    pub client: reqwest::Client,
    pub expected_size: Option<u64>,
    /// Origin-only binding for a provider-issued compiled download; never persisted.
    pub expected_origin: Option<url::Url>,
}

pub fn validate_download_response(
    provider: StoreProvider,
    response: &reqwest::Response,
    expected_origin: Option<&url::Url>,
) -> Result<()> {
    match provider {
        StoreProvider::Booth => crate::booth::validate_download_response(response),
        StoreProvider::Gumroad => gumroad_service::validate_download_response(response),
        StoreProvider::Jinxxy => jinxxy::validate_download_response(response, expected_origin),
    }
}

pub(super) fn active_bound_account(account: &Value) -> bool {
    account["identityVerified"] == true
        && account["disconnected"] != true
        && account["status"] == "connected"
        && account["state"] == "connected"
        && account["identityHash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty())
}

/// A shared account binding is not a credential for the current device.
pub(crate) fn has_local_session(catalogue: &Catalogue, account: &Value) -> bool {
    if catalogue.library_root().is_none() {
        return true;
    }
    let Some(id) = account["id"].as_str() else {
        return false;
    };
    let Some(provider) = account["store"]
        .as_str()
        .and_then(|provider| StoreProvider::parse(provider).ok())
    else {
        return false;
    };
    session::profile_path(&catalogue.device_dir(), provider, id)
        .ok()
        .and_then(|profile| session_cache::load(&profile, provider, id).ok())
        .is_some_and(|cookies| !cookies.is_empty())
}

#[derive(Clone)]
pub struct StoreService {
    catalogue: Catalogue,
    booth: Arc<BoothService>,
    gumroad: gumroad_service::GumroadService,
    jinxxy: jinxxy::JinxxyService,
    command_gate: Arc<tokio::sync::Mutex<()>>,
    public_gumroad: http::MetadataHttp,
    public_jinxxy: http::MetadataHttp,
}

fn requested_provider(args: &Value) -> Result<Option<StoreProvider>> {
    let parse = |value: &Value| {
        StoreProvider::parse(
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("STORE_UNAVAILABLE"))?,
        )
    };
    let store = args.get("store").map(parse).transpose()?;
    let provider = args.get("provider").map(parse).transpose()?;
    ensure!(
        store.is_none() || provider.is_none() || store == provider,
        "STORE_ACCOUNT_MISMATCH"
    );
    Ok(store.or(provider))
}

fn require_provider(
    requested: Option<StoreProvider>,
    actual: StoreProvider,
) -> Result<StoreProvider> {
    ensure!(
        requested.is_none_or(|value| value == actual),
        "STORE_ACCOUNT_MISMATCH"
    );
    Ok(actual)
}

impl StoreService {
    pub fn has_local_session(&self, account: &Value) -> bool {
        has_local_session(&self.catalogue, account)
    }

    pub fn account_views(&self, accounts: Vec<Value>) -> Vec<Value> {
        accounts
            .into_iter()
            .map(|mut account| {
                let available = self.has_local_session(&account);
                account["localSessionAvailable"] = json!(available);
                if !available && account["status"] == "connected" {
                    account["status"] = json!("expired");
                    account["state"] = json!("expired");
                }
                account
            })
            .collect()
    }

    pub fn new(catalogue: Catalogue, booth: Arc<BoothService>) -> Result<Self> {
        let gumroad = gumroad_service::GumroadService::new(catalogue.clone())?;
        let jinxxy = jinxxy::JinxxyService::new(catalogue.clone())?;
        Ok(Self {
            catalogue,
            booth,
            gumroad,
            jinxxy,
            command_gate: Arc::new(tokio::sync::Mutex::new(())),
            public_gumroad: http::MetadataHttp::new(StoreProvider::Gumroad),
            public_jinxxy: http::MetadataHttp::new(StoreProvider::Jinxxy),
        })
    }

    pub(crate) fn rebind_catalogue(&self, catalogue: Catalogue) -> Result<Self> {
        ensure!(catalogue.device_dir() == self.catalogue.device_dir(), "STORE_DEVICE_MISMATCH");
        Ok(Self {
            booth: Arc::new(self.booth.rebind_catalogue(catalogue.clone())?),
            gumroad: self.gumroad.rebind_catalogue(catalogue.clone())?,
            jinxxy: jinxxy::JinxxyService::new(catalogue.clone())?,
            catalogue,
            command_gate: Arc::new(tokio::sync::Mutex::new(())),
            public_gumroad: http::MetadataHttp::new(StoreProvider::Gumroad),
            public_jinxxy: http::MetadataHttp::new(StoreProvider::Jinxxy),
        })
    }

    pub async fn start(&self, app: AppHandle) -> Result<()> {
        let _gate = self.command_gate.lock().await;
        let booth = self.booth.start(app.clone()).await;
        let gumroad = self.gumroad.start(&app).await;
        let jinxxy = self.jinxxy.start(&app).await;
        // A provider's failed recovery cannot prevent independent workers from
        // starting. Report the first real service error after trying all three.
        booth.and(gumroad).and(jinxxy)
    }

    pub fn provider_for_account(&self, account_id: &str) -> Result<StoreProvider> {
        uuid::Uuid::parse_str(account_id).map_err(|_| anyhow::anyhow!("INVALID_ACCOUNT_ID"))?;
        let account = self
            .catalogue
            .get("account", account_id)?
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
        StoreProvider::parse(
            account["store"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("STORE_UNAVAILABLE"))?,
        )
    }
    fn rename_account(&self, id: &str, name: &str) -> Result<()> {
        self.provider_for_account(id)?;
        for _ in 0..4 {
            let previous = self
                .catalogue
                .get("account", id)?
                .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
            let mut renamed = previous.clone();
            renamed["name"] = json!(name);
            renamed["label"] = json!(name);
            if self.catalogue.put_many_if_unchanged(
                vec![("account".into(), id.into(), renamed)],
                vec![("account".into(), id.into(), Some(previous))],
            )? {
                return Ok(());
            }
        }
        bail!("CONCURRENT_UPDATE")
    }

    fn provider_for_job(&self, job_id: &str) -> Result<StoreProvider> {
        let job = self
            .catalogue
            .get("job", job_id)?
            .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
        let booth_job = matches!(
            job["type"].as_str(),
            Some("boothSync" | "boothUpdate" | "boothLogin")
        );
        let provider = StoreProvider::parse(
            job["store"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("INVALID_JOB"))?,
        )?;
        if let Some(account) = job["accountId"].as_str() {
            require_provider(Some(provider), self.provider_for_account(account)?)?;
        }
        ensure!(
            !booth_job || provider == StoreProvider::Booth,
            "STORE_ACCOUNT_MISMATCH"
        );
        Ok(provider)
    }

    fn selected_groups(&self, args: &Value) -> Result<BTreeMap<StoreProvider, BTreeSet<String>>> {
        let ids = args["productIds"]
            .as_array()
            .or_else(|| args["ids"].as_array())
            .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))?;
        ensure!(!ids.is_empty(), "INVALID_PRODUCT_IDS");
        let mut providers: BTreeMap<StoreProvider, BTreeSet<String>> = BTreeMap::new();
        for id in ids {
            let id = id
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))?;
            let provider = match self.catalogue.get("product", id)? {
                Some(product) if product["provider"] == "local" => {
                    ensure!(
                        product["store"] == "Local Files"
                            && id.strip_prefix("local:").is_some_and(|id| !id.is_empty()),
                        "STORE_ACCOUNT_MISMATCH"
                    );
                    continue;
                }
                Some(product) => StoreProvider::parse(
                    product["store"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("STORE_UNAVAILABLE"))?,
                )?,
                // BOOTH allows explicitly selected numeric public products before ingestion.
                None if !id.is_empty()
                    && id.len() <= 24
                    && id.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    StoreProvider::Booth
                }
                None => bail!("PRODUCT_UNAVAILABLE"),
            };
            require_provider(requested_provider(args)?, provider)?;
            providers.entry(provider).or_default().insert(id.into());
        }
        Ok(providers)
    }

    fn preflight_updates(
        &self,
        groups: &BTreeMap<StoreProvider, BTreeSet<String>>,
        args: &Value,
    ) -> Result<()> {
        let entitlements = self.catalogue.list("entitlement")?;
        let mut accounts = BTreeSet::new();
        for (provider, products) in groups {
            if *provider == StoreProvider::Booth {
                continue;
            }
            for product in products {
                let mut found = false;
                for entitlement in entitlements
                    .iter()
                    .filter(|row| row["productId"] == *product)
                {
                    let account_id = entitlement["accountId"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
                    if args["accountId"]
                        .as_str()
                        .is_some_and(|scope| scope != account_id)
                    {
                        continue;
                    }
                    let account = self
                        .catalogue
                        .get("account", account_id)?
                        .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
                    if !active_bound_account(&account) || !self.has_local_session(&account) {
                        continue;
                    }
                    require_provider(Some(*provider), self.provider_for_account(account_id)?)?;
                    accounts.insert(account_id.to_owned());
                    found = true;
                }
                ensure!(found, "AUTH_REQUIRED");
            }
        }
        for job in self
            .catalogue
            .list("job")?
            .into_iter()
            .filter(|row| matches!(row["state"].as_str(), Some("queued" | "running")))
        {
            let booth_job = matches!(
                job["type"].as_str(),
                Some("boothSync" | "boothUpdate" | "boothLogin")
            );
            ensure!(
                !(booth_job && groups.contains_key(&StoreProvider::Booth))
                    && !job["accountId"]
                        .as_str()
                        .is_some_and(|id| accounts.contains(id)),
                "ACCOUNT_BUSY"
            );
        }
        Ok(())
    }

    pub async fn command(&self, app: &AppHandle, action: &str, mut args: Value) -> Result<Value> {
        if action == "fetch_product_metadata" {
            let raw = args["url"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
            if let Some((provider, url)) = public_metadata::product_url(raw)? {
                let transport = match provider {
                    StoreProvider::Gumroad => &self.public_gumroad,
                    StoreProvider::Jinxxy => &self.public_jinxxy,
                    StoreProvider::Booth => unreachable!(),
                };
                return public_metadata::fetch(transport, provider, url).await;
            }
        }
        // Finish interrupted-job recovery before accepting a new login/sync.
        // Provider start is idempotent; the same gate prevents startup from
        // mistaking a freshly created login for an interrupted one.
        self.start(app.clone()).await?;
        let _work = self.catalogue.begin_catalogue_work()?;
        // Serialize command preparation only; collectors run independently after job creation.
        let _gate = self.command_gate.lock().await;
        let requested = requested_provider(&args)?;
        if action == "update_products" {
            let groups = self.selected_groups(&args)?;
            self.preflight_updates(&groups, &args)?;
            let mut jobs = BTreeSet::new();
            for (provider, ids) in groups {
                let mut provider_args = json!({"productIds":ids});
                if let Some(scope) = args.get("accountId") {
                    provider_args["accountId"] = scope.clone();
                }
                let result = match provider {
                    StoreProvider::Booth => self.booth.command(app, action, provider_args).await?,
                    StoreProvider::Gumroad => {
                        self.gumroad.command(app, action, provider_args).await?
                    }
                    StoreProvider::Jinxxy => {
                        self.jinxxy.command(app, action, provider_args).await?
                    }
                };
                if let Some(ids) = result["jobIds"].as_array() {
                    jobs.extend(ids.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                if let Some(id) = result["jobId"].as_str() {
                    jobs.insert(id.into());
                }
            }
            return Ok(json!({"jobId":jobs.first(),"jobIds":jobs}));
        }
        let provider = if matches!(action, "cancel_sync" | "retry_sync") {
            let job = args["jobId"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            require_provider(requested, self.provider_for_job(job)?)?
        } else if let Some(account) = args["accountId"].as_str() {
            require_provider(requested, self.provider_for_account(account)?)?
        } else {
            requested.unwrap_or(StoreProvider::Booth)
        };
        let account_id = args["accountId"].as_str().map(str::to_owned);
        let retried = if action == "retry_sync" {
            self.catalogue
                .get("job", args["jobId"].as_str().unwrap_or(""))?
        } else {
            None
        };
        if action == "sync"
            || (action == "retry_sync"
                && retried
                    .as_ref()
                    .is_some_and(|job| !job["type"].as_str().unwrap_or("").ends_with("Login")))
        {
            let id = if let Some(id) = account_id.as_deref() {
                Some(id.to_owned())
            } else {
                retried.and_then(|job| job["accountId"].as_str().map(str::to_owned))
            };
            if let Some(id) = id {
                let account = self
                    .catalogue
                    .get("account", &id)?
                    .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
                ensure!(self.has_local_session(&account), "AUTH_REQUIRED");
            }
        }
        if action == "account_rename" {
            let id = account_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
            let name = normalized_account_name(&args["name"])?;
            self.rename_account(id, &name)?;
            let _ = app.emit("nna:changed", json!({"scope":"booth"}));
            return Ok(json!({"accountId":id,"name":name}));
        }
        if matches!(action, "account_logout" | "account_remove") {
            self.catalogue.require_account_idle(
                account_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?,
            )?;
        }
        if action == "account_add" && provider != StoreProvider::Booth {
            let id = uuid::Uuid::new_v4().to_string();
            let name = args["name"]
                .as_str()
                .or_else(|| args["label"].as_str())
                .map(|name| {
                    name.chars()
                        .filter(|c| !c.is_control())
                        .take(200)
                        .collect::<String>()
                })
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| provider.label().into());
            self.catalogue.put(
                "account",
                &id,
                json!({
                    "id":id,"store":provider.label(),"name":name,"label":name,
                    "state":"needsAction","status":"disconnected","identityVerified":false,
                    "createdAt":chrono::Utc::now().to_rfc3339()
                }),
            )?;
            let _ = app.emit("nna:changed", json!({"scope":"booth"}));
            return Ok(json!({"accountId":id}));
        }
        let result = match provider {
            StoreProvider::Booth => {
                if let Some(object) = args.as_object_mut() {
                    object.remove("provider");
                    if action == "account_add" && object.contains_key("store") {
                        object.insert("store".into(), json!("BOOTH"));
                    } else {
                        object.remove("store");
                    }
                }
                self.booth.command(app, action, args).await
            }
            StoreProvider::Gumroad => self.gumroad.command(app, action, args).await,
            StoreProvider::Jinxxy => self.jinxxy.command(app, action, args).await,
        };
        if result.is_err() && matches!(action, "account_login" | "login") {
            if let Some(id) = account_id {
                if self.catalogue.discard_unverified_account(&id)? {
                    let _ = app.emit("nna:changed", json!({"scope":"booth"}));
                }
            }
        }
        result
    }

    pub async fn download_ticket(&self, app: &AppHandle, file_id: &str) -> Result<DownloadTicket> {
        let file = self
            .catalogue
            .get("remote_file", file_id)?
            .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
        let account = file["accountId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
        let provider = self.provider_for_account(account)?;
        let record = self
            .catalogue
            .get("account", account)?
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
        ensure!(self.has_local_session(&record), "AUTH_REQUIRED");
        let product_id = file["productId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("PRODUCT_UNAVAILABLE"))?;
        let product = self
            .catalogue
            .get("product", product_id)?
            .ok_or_else(|| anyhow::anyhow!("PRODUCT_UNAVAILABLE"))?;
        require_provider(
            Some(provider),
            StoreProvider::parse(
                product["store"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("STORE_UNAVAILABLE"))?,
            )?,
        )?;
        let result = match provider {
            StoreProvider::Booth => self
                .booth
                .download_ticket(app, file_id)
                .await
                .map(|ticket| DownloadTicket {
                    provider,
                    url: ticket.url,
                    client: ticket.client,
                    expected_size: ticket.expected_size,
                    expected_origin: None,
                }),
            StoreProvider::Gumroad => self.gumroad.download_ticket(app, file_id).await,
            StoreProvider::Jinxxy => self.jinxxy.download_ticket(app, file_id).await,
        };
        if self.catalogue.library_root().is_some()
            && result
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string() == "AUTH_REQUIRED")
        {
            session::forget(&self.catalogue.device_dir(), provider, account)?;
        }
        result
    }
}

fn normalized_account_name(value: &Value) -> Result<String> {
    let name: String = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("INVALID_ACCOUNT_NAME"))?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name: String = name.trim().chars().take(200).collect();
    ensure!(!name.is_empty(), "INVALID_ACCOUNT_NAME");
    Ok(name)
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn shared_account_views_follow_device_credentials_without_changing_ownership() {
        use tauri::webview::Cookie;
        let base = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "nna-authored-shared-session-{}",
                uuid::Uuid::new_v4()
            ));
        let library = base.join("library");
        let first_device = base.join("first-device");
        let second_device = base.join("second-device");
        std::fs::create_dir_all(&library).unwrap();
        let first = Catalogue::open_bound(&library, &first_device).unwrap();
        let service = StoreService::new(
            first.clone(),
            Arc::new(BoothService::new(first.clone()).unwrap()),
        )
        .unwrap();
        let mut expected = Vec::new();
        for provider in [
            StoreProvider::Booth,
            StoreProvider::Gumroad,
            StoreProvider::Jinxxy,
        ] {
            let id = uuid::Uuid::new_v4().to_string();
            let account = json!({"id":id,"store":provider.label(),"name":"Authored shared name","status":"connected","state":"connected","identityVerified":true,"identityHash":"authored-binding","disconnected":false});
            let entitlement = json!({"id":id,"accountId":id,"productId":"authored-product","fileIds":["authored-file"]});
            first.put("account", &id, account.clone()).unwrap();
            first.put("entitlement", &id, entitlement.clone()).unwrap();
            let host = match provider {
                StoreProvider::Booth => "booth.pm",
                StoreProvider::Gumroad => "gumroad.com",
                StoreProvider::Jinxxy => "jinxxy.com",
            };
            let profile = session::profile_path(&first.device_dir(), provider, &id).unwrap();
            session_cache::save(
                &profile,
                provider,
                &id,
                vec![Cookie::build(("authored_session", "AUTHORED_COOKIE_VALUE"))
                    .domain(host)
                    .path("/")
                    .secure(true)
                    .build()],
            )
            .unwrap();
            assert!(service.has_local_session(&account));
            expected.push((provider, id, account, entitlement));
        }
        assert!(!first.data_dir().join("auth").exists());
        drop(service);
        drop(first);
        let second = Catalogue::open_bound(&library, &second_device).unwrap();
        let service = StoreService::new(
            second.clone(),
            Arc::new(BoothService::new(second.clone()).unwrap()),
        )
        .unwrap();
        let views = service.account_views(second.list("account").unwrap());
        for (_, id, account, entitlement) in expected {
            let view = views.iter().find(|view| view["id"] == id).unwrap();
            assert_eq!(view["status"], "expired");
            assert_eq!(view["localSessionAvailable"], false);
            assert_eq!(view["disconnected"], false);
            assert_eq!(view["name"], account["name"]);
            assert_eq!(second.get("account", &id).unwrap(), Some(account));
            assert_eq!(second.get("entitlement", &id).unwrap(), Some(entitlement));
        }
        drop(service);
        drop(second);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn store_execution_queue_is_shared_by_accounts_but_not_by_other_providers() {
        let path = std::env::temp_dir().join(format!("store-queues-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&path).unwrap();
        let service = super::StoreService::new(
            catalogue.clone(),
            std::sync::Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap()),
        )
        .unwrap();
        // These are the same gates acquired by the actual provider executors.
        let first_booth_account = service.booth.execution_gate().lock().await;
        assert!(service.booth.execution_gate().try_lock().is_err());
        let first_gumroad_account = service.gumroad.execution_gate().try_lock().unwrap();
        assert!(service.gumroad.execution_gate().try_lock().is_err());
        drop(first_booth_account);
        assert!(service.booth.execution_gate().try_lock().is_ok());
        drop(first_gumroad_account);
        assert!(service.gumroad.execution_gate().try_lock().is_ok());
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    use super::*;
    #[test]
    fn account_display_name_validation_is_bounded() {
        assert_eq!(
            normalized_account_name(&json!("  Authored\n name  ")).unwrap(),
            "Authored name"
        );
        assert!(normalized_account_name(&json!(" \n\t ")).is_err());
        assert!(normalized_account_name(&Value::Null).is_err());
        assert_eq!(
            normalized_account_name(&json!("猫".repeat(201)))
                .unwrap()
                .chars()
                .count(),
            200
        );
    }
    #[test]
    fn account_rename_preserves_native_identity_and_entitlements_across_restart() {
        let root = std::env::var("NNA_STORE_EVIDENCE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let path = root.join(format!("rename-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = StoreService::new(
            catalogue.clone(),
            Arc::new(BoothService::new(catalogue.clone()).unwrap()),
        )
        .unwrap();
        let mut expected = Vec::new();
        for provider in [
            StoreProvider::Booth,
            StoreProvider::Gumroad,
            StoreProvider::Jinxxy,
        ] {
            let id = uuid::Uuid::new_v4().to_string();
            let mut account = json!({"id":id,"store":provider.label(),"name":"Before","label":"Before","identityHash":"authored-identity","identityVerified":true,"state":"connected","status":"connected","disconnected":false});
            catalogue.put("account", &id, account.clone()).unwrap();
            let entitlement = json!({"id":id,"accountId":id,"productId":"authored-product","manifestComplete":true,"fileIds":["authored-file"]});
            catalogue
                .put("entitlement", &id, entitlement.clone())
                .unwrap();
            service.rename_account(&id, "After").unwrap();
            account["name"] = json!("After");
            account["label"] = json!("After");
            expected.push((id, account, entitlement));
        }
        drop(service);
        drop(catalogue);
        let reopened = Catalogue::open(&path).unwrap();
        for (id, account, entitlement) in expected {
            assert_eq!(reopened.get("account", &id).unwrap(), Some(account));
            assert_eq!(reopened.get("entitlement", &id).unwrap(), Some(entitlement));
        }
    }

    #[test]
    fn explicit_store_cannot_override_persisted_account_or_job_provider() {
        for provider in [
            StoreProvider::Booth,
            StoreProvider::Gumroad,
            StoreProvider::Jinxxy,
        ] {
            assert_eq!(require_provider(None, provider).unwrap(), provider);
            for requested in [
                StoreProvider::Booth,
                StoreProvider::Gumroad,
                StoreProvider::Jinxxy,
            ] {
                assert_eq!(
                    require_provider(Some(requested), provider).is_ok(),
                    requested == provider
                );
            }
        }
        assert!(requested_provider(&json!({"store":null})).is_err());
        assert!(requested_provider(&json!({"store":"unknown"})).is_err());
        assert!(requested_provider(&json!({"store":"BOOTH","provider":"gumroad"})).is_err());
    }

    #[test]
    fn mixed_selected_update_groups_are_namespace_bound_and_preflight_all_accounts() {
        let path = std::env::temp_dir().join(format!(
            "nna-authored-store-groups-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        let catalogue = Catalogue::open(&path).unwrap();
        let service = StoreService::new(
            catalogue.clone(),
            Arc::new(BoothService::new(catalogue.clone()).unwrap()),
        )
        .unwrap();
        for (id, store) in [
            ("900001", "BOOTH"),
            ("gumroad:900001", "Gumroad"),
            ("jinxxy:900001", "Jinxxy"),
        ] {
            catalogue
                .put(
                    "product",
                    id,
                    json!({"id":id,"store":store,"title":"Authored product"}),
                )
                .unwrap();
        }
        let local_id = "local:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        catalogue.put("product", local_id, json!({"id":local_id,"provider":"local","store":"Local Files","title":"Authored local product"})).unwrap();
        let args =
            json!({"productIds":["900001","gumroad:900001","jinxxy:900001","900001",local_id]});
        let groups = service.selected_groups(&args).unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(
            groups[&StoreProvider::Booth],
            BTreeSet::from(["900001".into()])
        );
        assert_eq!(
            groups[&StoreProvider::Gumroad],
            BTreeSet::from(["gumroad:900001".into()])
        );
        assert_eq!(
            groups[&StoreProvider::Jinxxy],
            BTreeSet::from(["jinxxy:900001".into()])
        );
        let local_groups = service
            .selected_groups(&json!({"productIds":[local_id]}))
            .unwrap();
        assert!(local_groups.is_empty());
        assert!(service.preflight_updates(&local_groups, &json!({})).is_ok());
        for (id, provider, store) in [
            ("local:conflicting", "local", "Gumroad"),
            ("gumroad:conflicting", "local", "Local Files"),
            ("local:unknown", "unknown", "Local Files"),
            ("unknown:product", "unknown", "Unknown Store"),
        ] {
            catalogue.put("product", id, json!({"id":id,"provider":provider,"store":store,"title":"Authored invalid identity"})).unwrap();
            assert!(service
                .selected_groups(&json!({"productIds":[local_id,id,"900001"]}))
                .is_err());
        }
        let mut conflicting = args;
        conflicting["store"] = json!("BOOTH");
        assert!(service.selected_groups(&conflicting).is_err());
        assert!(service.preflight_updates(&groups, &json!({})).is_err());
        assert!(catalogue.list("job").unwrap().is_empty());
        let accounts = [
            (
                "ffffffff-ffff-4fff-8fff-ffffffffffff",
                "Gumroad",
                "gumroad:900001",
            ),
            (
                "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
                "Jinxxy",
                "jinxxy:900001",
            ),
        ];
        for (account, store, product) in accounts {
            catalogue.put("account", account, json!({"id":account,"store":store,"status":"connected","state":"connected","identityVerified":true,"identityHash":"authored-buyer-binding"})).unwrap();
            catalogue.put("entitlement", account, json!({"id":account,"accountId":account,"productId":product,"fileIds":[],"manifestComplete":false})).unwrap();
        }
        let historical = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        catalogue.put("account", historical, json!({"id":historical,"store":"Gumroad","status":"disconnected","state":"disconnected","identityVerified":false,"identityHash":"authored-old-binding"})).unwrap();
        catalogue.put("entitlement", historical, json!({"id":historical,"accountId":historical,"productId":"gumroad:900001","fileIds":[]})).unwrap();
        assert!(service.preflight_updates(&groups, &json!({})).is_ok());
        assert_eq!(
            service
                .preflight_updates(&groups, &json!({"accountId":historical}))
                .unwrap_err()
                .to_string(),
            "AUTH_REQUIRED"
        );
        catalogue.put("job", "authored-job", json!({"id":"authored-job","type":"gumroadUpdate","store":"Gumroad","accountId":accounts[0].0,"state":"running"})).unwrap();
        assert_eq!(
            service
                .preflight_updates(&groups, &json!({}))
                .unwrap_err()
                .to_string(),
            "ACCOUNT_BUSY"
        );
        drop(service);
        drop(catalogue);
        let _ = std::fs::remove_dir_all(path);
    }
}
