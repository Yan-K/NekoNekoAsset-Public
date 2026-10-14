mod ai;
mod ai_analysis;
mod ai_images;
mod ai_jobs;
mod ai_provider;
mod ai_suggestions;
mod app_reset;
mod booth;
mod catalogue;
mod library_lock;
mod native;
mod preview;
mod profile;
#[cfg(test)]
mod setup_tests;
mod storage;
mod stores;
mod taxonomy;
mod unity;
mod updates;
mod verification;
mod window_state;

use anyhow::Context;
use booth::BoothService;
use catalogue::Catalogue;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use storage::StorageService;
use stores::StoreService;
use tauri::{Emitter, Listener, Manager};

#[derive(Clone)]
struct Services {
    setup_only: bool,
    ai: Arc<ai::AiService>,
    ai_jobs: Arc<ai_jobs::AiJobs>,
    ai_suggestions: Arc<ai_suggestions::AiSuggestions>,
    catalogue: Catalogue,
    stores: Arc<StoreService>,
    storage: Arc<StorageService>,
    previews: Arc<preview::PreviewCache>,
    unity: Arc<unity::UnityService>,
}

impl Services {
    fn new(catalogue: Catalogue, device_dir: &std::path::Path) -> anyhow::Result<Self> {
        Self::build(catalogue, device_dir, None)
    }

    fn build(
        catalogue: Catalogue,
        device_dir: &std::path::Path,
        previous: Option<&StoreService>,
    ) -> anyhow::Result<Self> {
        let setup_only = catalogue
            .get("preferences", "app")?
            .is_none_or(|preferences| preferences["setupComplete"] != true);
        let ai = Arc::new(ai::AiService::new(device_dir)?);
        let ai_suggestions = Arc::new(ai_suggestions::AiSuggestions::new(
            catalogue.clone(),
            ai.clone(),
        )?);
        let stores = Arc::new(match previous {
            Some(previous) => previous.rebind_catalogue(catalogue.clone())?,
            None => StoreService::new(
                catalogue.clone(),
                Arc::new(BoothService::new(catalogue.clone())?),
            )?,
        });
        let storage = Arc::new(StorageService::new(catalogue.clone(), stores.clone())?);
        let unity = Arc::new(unity::UnityService::new(
            catalogue.clone(),
            storage.clone(),
        )?);
        let previews = Arc::new(preview::PreviewCache::new(catalogue.data_dir())?);
        let ai_jobs = Arc::new(ai_jobs::AiJobs::new(
            catalogue.clone(),
            ai.clone(),
            storage.clone(),
            previews.clone(),
        ));
        Ok(Self {
            setup_only,
            ai,
            ai_jobs,
            ai_suggestions,
            catalogue,
            stores,
            storage,
            previews,
            unity,
        })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryStatus {
    state: String,
    path: Option<PathBuf>,
    default_path: Option<PathBuf>,
    error_code: Option<String>,
    locale: String,
    missing_credentials: Vec<Value>,
}

struct LibraryHost {
    device_dir: PathBuf,
    status: LibraryStatus,
    services: Mutex<Option<Services>>,
    setup: Mutex<Option<SetupState>>,
    opening: AtomicBool,
    // Keep both the destination and source protected until restart ends all workers.
    next_catalogue: Mutex<Option<Catalogue>>,
    switching: Mutex<Option<catalogue::CatalogueWorkPermit>>,
}

#[derive(Clone)]
struct SetupState {
    catalogue: Catalogue,
    stores: Arc<StoreService>,
    candidate: Arc<Mutex<PathBuf>>,
    initial_preferences: Value,
}

impl SetupState {
    fn new(device_dir: &std::path::Path, candidate: PathBuf, locale: &str) -> anyhow::Result<Self> {
        let catalogue = Catalogue::open_setup(device_dir)?;
        catalogue.command("preferences", json!({"patch":{"locale":locale}}))?;
        let initial_preferences = catalogue
            .get("preferences", "app")?
            .context("catalogue.preferences_missing")?;
        let stores = Arc::new(StoreService::new(
            catalogue.clone(),
            Arc::new(BoothService::new(catalogue.clone())?),
        )?);
        Ok(Self {
            catalogue,
            stores,
            candidate: Arc::new(Mutex::new(candidate)),
            initial_preferences,
        })
    }

    fn candidate(&self) -> Result<PathBuf, UiError> {
        self.candidate
            .lock()
            .map(|candidate| candidate.clone())
            .map_err(|_| UiError {
                code: "library_unavailable".into(),
            })
    }
}

impl LibraryHost {
    fn begin_opening(&self) -> Result<(), UiError> {
        if self.opening.swap(true, Ordering::AcqRel) {
            return Err(UiError {
                code: "library_opening".into(),
            });
        }
        Ok(())
    }

    fn active_services(&self) -> Result<Option<Services>, UiError> {
        self.services
            .lock()
            .map(|services| services.clone())
            .map_err(|_| UiError {
                code: "library_unavailable".into(),
            })
    }

    fn active_setup(&self) -> Result<Option<SetupState>, UiError> {
        self.setup
            .lock()
            .map(|setup| setup.clone())
            .map_err(|_| UiError {
                code: "library_unavailable".into(),
            })
    }

    fn services(&self) -> Result<Services, UiError> {
        if self.opening.load(Ordering::Acquire) {
            return Err(UiError {
                code: "library_opening".into(),
            });
        }
        self.active_services()?.ok_or_else(|| UiError {
            code: self
                .status
                .error_code
                .clone()
                .unwrap_or_else(|| "library_not_open".into()),
        })
    }
}

#[derive(Debug, Serialize)]
struct UiError {
    code: String,
}
impl UiError {
    fn from_error(error: anyhow::Error) -> Self {
        let raw = error.to_string();
        let first = raw.split(':').next().unwrap_or("operation_failed").trim();
        let code = if !first.is_empty()
            && first.len() <= 80
            && first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            first.to_owned()
        } else {
            "operation_failed".to_owned()
        };
        Self { code }
    }
}
#[derive(Default)]
struct NativeOperationGate(Arc<tokio::sync::RwLock<()>>);

fn trusted_access(window: &tauri::WebviewWindow) -> Result<(), UiError> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err(UiError {
            code: "access_denied".into(),
        })
    }
}
fn trusted(window: &tauri::WebviewWindow) -> Result<tokio::sync::OwnedRwLockReadGuard<()>, UiError> {
    trusted_access(window)?;
    window.app_handle().state::<NativeOperationGate>().0.clone().try_read_owned()
        .map_err(|_| UiError { code: "updates_busy".into() })
}
fn changed(app: &tauri::AppHandle, scope: &str) {
    let _ = app.emit("nna:changed", json!({"scope":scope}));
}

async fn sync_connected_accounts(
    app: &tauri::AppHandle,
    catalogue: &Catalogue,
    stores: &StoreService,
) -> Result<(), UiError> {
    let _work = catalogue
        .begin_catalogue_work()
        .map_err(UiError::from_error)?;
    for account in catalogue.list("account").map_err(UiError::from_error)? {
        if account["disconnected"] == true
            || account["status"] != "connected"
            || !stores.has_local_session(&account)
        {
            continue;
        }
        let Some(id) = account["id"].as_str() else {
            continue;
        };
        let active = catalogue
            .list("job")
            .map_err(UiError::from_error)?
            .iter()
            .any(|job| {
                let kind = job["type"].as_str().unwrap_or("").to_ascii_lowercase();
                job["accountId"] == id
                    && (kind.contains("sync") || kind.contains("update"))
                    && matches!(job["state"].as_str(), Some("queued" | "running"))
            });
        if active {
            continue;
        }
        if let Err(error) = stores
            .command(
                app,
                "sync",
                json!({"accountId":id,"source":"all","mode":"incremental"}),
            )
            .await
        {
            // Setup has already committed. Retain an actionable sync failure
            // rather than falsely reporting that configuring the library failed.
            let provider = account["store"]
                .as_str()
                .unwrap_or("BOOTH")
                .to_ascii_lowercase();
            let (kind, store) = match provider.as_str() {
                "gumroad" => ("gumroadSync", "Gumroad"),
                "jinxxy" => ("jinxxySync", "Jinxxy"),
                _ => ("boothSync", "BOOTH"),
            };
            let job_id = uuid::Uuid::new_v4().to_string();
            let timestamp = chrono::Utc::now().to_rfc3339();
            catalogue.put("job", &job_id, json!({"id":job_id,"type":kind,"store":store,"accountId":id,"args":{"accountId":id,"source":"all","mode":"incremental"},"state":"failed","progress":{"stage":"failed"},"errorCode":UiError::from_error(error).code,"createdAt":timestamp,"updatedAt":timestamp,"completedAt":timestamp})).map_err(UiError::from_error)?;
        }
    }
    changed(app, "jobs");
    Ok(())
}

#[tauri::command]
async fn ai_action(
    window: tauri::WebviewWindow,
    services: tauri::State<'_, LibraryHost>,
    request: ai::AiRequest,
) -> Result<ai::AiResponse, UiError> {
    let _operation = trusted(&window)?;
    let services = services.services()?;
    if services
        .catalogue
        .get("preferences", "app")
        .map_err(UiError::from_error)?
        .is_none_or(|preferences| preferences["aiFeaturesEnabled"] != true)
    {
        return Err(UiError {
            code: "ai_features_disabled".into(),
        });
    }
    services
        .ai
        .command(request)
        .await
        .map_err(UiError::from_error)
}

#[tauri::command]
async fn ai_analysis_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    services: tauri::State<'_, LibraryHost>,
    request: ai_jobs::AiJobRequest,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let services = services.services()?;
    services
        .ai_jobs
        .command(&app, request)
        .await
        .map_err(UiError::from_error)
}

#[tauri::command]
async fn ai_suggestion_action(
    window: tauri::WebviewWindow,
    services: tauri::State<'_, LibraryHost>,
    request: ai_suggestions::AiSuggestionRequest,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let services = services.services()?;
    services
        .ai_suggestions
        .command(request)
        .await
        .map_err(UiError::from_error)
}

#[tauri::command]
async fn catalogue_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    services: tauri::State<'_, LibraryHost>,
    action: String,
    args: Value,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    if let Some(setup) = services.active_setup()? {
        if services.opening.load(Ordering::Acquire) {
            return Err(UiError {
                code: "library_opening".into(),
            });
        }
        if action == "preferences" && args["patch"]["setupComplete"] == true {
            return finish_staged_setup(app, &services, setup, args).await;
        }
        if !matches!(action.as_str(), "bootstrap" | "state" | "preferences") {
            return Err(UiError {
                code: "library_not_open".into(),
            });
        }
        let catalogue = setup.catalogue.clone();
        let mutation = action == "preferences";
        let _work = mutation
            .then(|| setup.catalogue.begin_catalogue_work())
            .transpose()
            .map_err(UiError::from_error)?;
        let mut result =
            tauri::async_runtime::spawn_blocking(move || catalogue.command(&action, args))
                .await
                .map_err(|_| UiError {
                    code: "worker_failed".into(),
                })?
                .map_err(UiError::from_error)?;
        if let Some(accounts) = result["accounts"].as_array() {
            result["accounts"] = json!(setup.stores.account_views(accounts.clone()));
        }
        if mutation {
            changed(&app, "state");
        }
        return Ok(result);
    }
    if services.status.state == "unconfigured"
        && services.active_services()?.is_none()
        && matches!(action.as_str(), "bootstrap" | "state")
    {
        let locale = services.status.locale.clone();
        return Ok(
            json!({"version":1,"products":[],"accounts":[],"roots":[],"jobs":[],"preferences":{"locale":locale,"setupComplete":false,"autoDownloadAll":false},"collections":[],"savedSearches":[],"sourceCoverage":[],"conflicts":[],"avatarOptions":[],"ownedBaseOptions":[],"tagOptions":[],"total":0,"counts":{"all":0,"downloaded":0,"attention":0}}),
        );
    }
    let host = services;
    let services = host.services()?;
    if services.setup_only && action == "preferences" && args["patch"]["setupComplete"] == true {
        return finish_setup(app, &host, services, args).await;
    }
    let catalogue = services.catalogue.clone();
    let invalidates = !matches!(
        action.as_str(),
        "bootstrap"
            | "query"
            | "conflicts"
            | "product_details"
            | "account_counts"
            | "manifest"
            | "export_csv"
            | "collections"
            | "state"
            | "spending_summary"
    );
    let _work = if action == "restore_manifest" {
        Some(
            services
                .catalogue
                .begin_restore_work()
                .map_err(UiError::from_error)?,
        )
    } else if invalidates {
        Some(
            services
                .catalogue
                .begin_catalogue_work()
                .map_err(UiError::from_error)?,
        )
    } else {
        None
    };
    let previous_preferences = if action == "preferences" {
        catalogue
            .get("preferences", "app")
            .map_err(UiError::from_error)?
            .unwrap_or(json!({}))
    } else {
        Value::Null
    };
    let mut reevaluate_automatic = action == "preferences"
        && args["patch"]["autoDownloadAll"]
            .as_bool()
            .is_some_and(|next| {
                next != previous_preferences["autoDownloadAll"]
                    .as_bool()
                    .unwrap_or(false)
            });
    let completes_setup = action == "preferences"
        && args["patch"]["setupComplete"] == true
        && previous_preferences["setupComplete"] != true;
    let avatar_settings_changed = action == "preferences"
        && args["patch"]
            .get("avatarSettings")
            .is_some_and(|next| *next != previous_preferences["avatarSettings"]);
    let mutation_scope = match action.as_str() {
        "collection" => "collections",
        "clear_activity" => "catalogue",
        "preferences" if !avatar_settings_changed => "state",
        _ => "catalogue",
    };
    let is_exchange = action == "exchange";
    let account_view = matches!(action.as_str(), "bootstrap" | "state");
    let mut result = tauri::async_runtime::spawn_blocking(move || catalogue.command(&action, args))
        .await
        .map_err(|_| UiError {
            code: "worker_failed".into(),
        })?
        .map_err(UiError::from_error)?;
    if account_view {
        if let Some(accounts) = result["accounts"].as_array() {
            result["accounts"] = json!(services.stores.account_views(accounts.clone()));
        }
    }
    if previous_preferences.is_object()
        && (previous_preferences["aiEnabled"] != result["aiEnabled"]
            || previous_preferences["aiFeaturesEnabled"] != result["aiFeaturesEnabled"])
    {
        services
            .ai_jobs
            .reset_automatic_baseline()
            .await
            .map_err(UiError::from_error)?;
    }
    if previous_preferences["aiFeaturesEnabled"] == true && result["aiFeaturesEnabled"] == false {
        services
            .ai_jobs
            .cancel_all(&app)
            .await
            .map_err(UiError::from_error)?;
        services.ai_suggestions.cancel_all().await;
    }
    reevaluate_automatic |= is_exchange && result["read"]["imported"].as_u64().unwrap_or(0) > 0;
    if invalidates {
        changed(&app, mutation_scope);
    }
    if completes_setup {
        sync_connected_accounts(&app, &services.catalogue, &services.stores).await?;
    }
    if reevaluate_automatic && !services.setup_only {
        let storage = services.storage.clone();
        tauri::async_runtime::spawn(async move {
            let _ = storage.reevaluate_automatic().await;
        });
    }
    Ok(result)
}
#[tauri::command]
async fn booth_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    services: tauri::State<'_, LibraryHost>,
    action: String,
    args: Value,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    if let Some(setup) = services.active_setup()? {
        if services.opening.load(Ordering::Acquire) {
            return Err(UiError {
                code: "library_opening".into(),
            });
        }
        if !matches!(
            action.as_str(),
            "account_add"
                | "account_login"
                | "login"
                | "account_rename"
                | "account_remove"
                | "account_logout"
                | "cancel_sync"
                | "retry_sync"
        ) {
            return Err(UiError {
                code: "library_not_open".into(),
            });
        }
        if matches!(action.as_str(), "cancel_sync" | "retry_sync") {
            let job = setup
                .catalogue
                .get("job", args["jobId"].as_str().unwrap_or(""))
                .map_err(UiError::from_error)?;
            if !job.is_some_and(|job| {
                job["type"]
                    .as_str()
                    .is_some_and(|kind| kind.ends_with("Login"))
            }) {
                return Err(UiError {
                    code: "library_not_open".into(),
                });
            }
        }
        let result = setup
            .stores
            .command(&app, &action, args)
            .await
            .map_err(UiError::from_error)?;
        changed(&app, "booth");
        return Ok(result);
    }
    let services = services.services()?;
    let result = {
        // Keep new downloads out between the idle check and session clearing.
        let _queue = if matches!(action.as_str(), "account_logout" | "account_remove") {
            Some(services.storage.pause_download_queue().await)
        } else {
            None
        };
        services
            .stores
            .command(&app, &action, args)
            .await
            .map_err(UiError::from_error)?
    };
    if !matches!(
        action.as_str(),
        "fetch_product_metadata"
            | "diagnose_ordering"
            | "diagnose_manifests"
            | "diagnose_file_structure"
            | "diagnose_free_item_entry"
            | "diagnose_free_item_identity"
            | "diagnose_receipt_template"
            | "diagnose_receipt_notice_envelope"
            | "diagnose_receipt_contact_envelope"
            | "diagnose_product_line_fields"
    ) {
        changed(
            &app,
            if matches!(
                action.as_str(),
                "sync" | "cancel_sync" | "retry_sync" | "update_products"
            ) {
                "jobs"
            } else {
                "booth"
            },
        );
        if !services.setup_only
            && matches!(
                action.as_str(),
                "account_add" | "account_remove" | "account_logout"
            )
        {
            let storage = services.storage.clone();
            tauri::async_runtime::spawn(async move {
                let _ = storage.reevaluate_automatic().await;
            });
        }
    }
    Ok(result)
}
#[tauri::command]
async fn storage_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    services: tauri::State<'_, LibraryHost>,
    action: String,
    args: Value,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let device_dir = services.device_dir.clone();
    if action == "reset_app" {
        let delete_library = args["deleteLibrary"].as_bool().ok_or_else(|| UiError {
            code: "reset_invalid_request".into(),
        })?;
        let active = services.services()?;
        if services.opening.swap(true, Ordering::AcqRel) {
            return Err(UiError {
                code: "library_opening".into(),
            });
        }
        let result = async {
            let permit = active.catalogue.begin_restore_work()?;
            profile::prepare_restart_handoff()?;
            let catalogue = active.catalogue.clone();
            tauri::async_runtime::spawn_blocking(move || {
                app_reset::schedule(&device_dir, &catalogue, delete_library)
            })
            .await
            .map_err(|_| anyhow::anyhow!("worker_failed"))??;
            *services
                .switching
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))? = Some(permit);
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            services.opening.store(false, Ordering::Release);
            return Err(UiError::from_error(error));
        }
        restart_for_library(app);
        return Ok(json!({"restartRequired":true}));
    }
    let services = services.services()?;
    if action == "migrate_root" && services.catalogue.library_root().is_some() {
        let storage = services.storage.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            storage.move_library(&args, &AtomicBool::new(false))
        })
        .await
        .map_err(|_| UiError {
            code: "worker_failed".into(),
        })?
        .map_err(UiError::from_error)?;
        let path = result["libraryPath"].as_str().ok_or_else(|| UiError {
            code: "library_unavailable".into(),
        })?;
        let locale = services
            .catalogue
            .get("preferences", "app")
            .map_err(UiError::from_error)?
            .unwrap_or(json!({}))["locale"]
            .as_str()
            .unwrap_or("en")
            .to_owned();
        if let Err(error) = profile::prepare_restart_handoff().and_then(|_| {
            profile::select_library(
                &device_dir,
                &profile::LibrarySelection {
                    path: PathBuf::from(path),
                    locale,
                },
            )
        }) {
            services
                .catalogue
                .release_move_hold()
                .map_err(UiError::from_error)?;
            return Err(UiError::from_error(error));
        }
        restart_for_library(app);
        return Ok(result);
    }
    let result = services
        .storage
        .command(&app, &action, args)
        .await
        .map_err(UiError::from_error)?;
    if !matches!(
        action.as_str(),
        "product_availability" | "open_file" | "open_product_folder" | "open_root_folder"
    ) {
        changed(
            &app,
            if matches!(
                action.as_str(),
                "enqueue_products" | "enqueue_file" | "cancel_job" | "retry_job" | "retry_file"
            ) {
                "jobs"
            } else {
                "storage"
            },
        );
        if matches!(action.as_str(), "configure_root" | "remap_root") {
            let storage = services.storage.clone();
            tauri::async_runtime::spawn(async move {
                let _ = storage.reevaluate_automatic().await;
            });
        }
    }
    Ok(result)
}
#[tauri::command]
async fn unity_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    services: tauri::State<'_, LibraryHost>,
    action: String,
    args: Value,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let services = services.services()?;
    services
        .unity
        .command(&app, &action, args)
        .await
        .map_err(UiError::from_error)
}
#[tauri::command]
async fn native_dialog(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    kind: String,
    options: Option<Value>,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    tauri::async_runtime::spawn_blocking(move || {
        native::dialog(&app, &kind, options.unwrap_or(json!({})))
    })
    .await
    .map_err(|_| UiError {
        code: "worker_failed".into(),
    })?
    .map_err(UiError::from_error)
}
#[tauri::command]
async fn native_file_action(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    services: tauri::State<'_, LibraryHost>,
    action: String,
    args: Value,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let services = services.services()?;
    let catalogue = services.catalogue.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        native::file_action(&app, &catalogue, &action, args)
    })
    .await
    .map_err(|_| UiError {
        code: "worker_failed".into(),
    })?
    .map_err(UiError::from_error)?;
    Ok(result)
}
#[tauri::command]
fn external_open(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    url: String,
) -> Result<(), UiError> {
    let _operation = trusted(&window)?;
    native::external_open(&app, &url).map_err(UiError::from_error)
}

#[tauri::command]
fn native_zoom(window: tauri::WebviewWindow, scale: f64) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let factor = native::zoom_factor(scale).map_err(UiError::from_error)?;
    window.set_zoom(factor).map_err(|_| UiError {
        code: "ui_scale_failed".into(),
    })?;
    Ok(json!({"scale":scale}))
}

#[tauri::command]
async fn preview_cache(
    window: tauri::WebviewWindow,
    services: tauri::State<'_, LibraryHost>,
    url: String,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    let services = services.services()?;
    let path = services
        .previews
        .get(url)
        .await
        .map_err(UiError::from_error)?;
    Ok(json!({"path":path}))
}

#[tauri::command]
fn library_status(
    window: tauri::WebviewWindow,
    host: tauri::State<'_, LibraryHost>,
) -> Result<LibraryStatus, UiError> {
    let _operation = trusted(&window)?;
    active_library_status(&host)
}

fn active_library_status(host: &LibraryHost) -> Result<LibraryStatus, UiError> {
    let mut status = host.status.clone();
    if let Some(setup) = host.active_setup()? {
        status.state = "setup".into();
        status.path = Some(setup.candidate()?);
        status.default_path = None;
        status.error_code = None;
        status.locale = setup
            .catalogue
            .get("preferences", "app")
            .map_err(UiError::from_error)?
            .and_then(|prefs| prefs["locale"].as_str().map(str::to_owned))
            .unwrap_or(status.locale);
        status.missing_credentials = vec![];
        return Ok(status);
    }
    if let Some(services) = host.active_services()? {
        status.state = "ready".into();
        status.path = services
            .catalogue
            .library_root()
            .map(std::path::Path::to_path_buf);
        status.default_path = None;
        status.error_code = None;
        if let Some(locale) = services
            .catalogue
            .get("preferences", "app")
            .map_err(UiError::from_error)?
            .and_then(|p| p["locale"].as_str().map(str::to_owned))
        {
            status.locale = locale;
        }
        status.missing_credentials = services.catalogue.list("account").map_err(UiError::from_error)?
            .into_iter().filter(|account| account["disconnected"] != true && account["status"] == "connected" && !services.stores.has_local_session(account))
            .map(|account| json!({"id":account["id"],"name":account["name"],"store":account["store"]})).collect();
    } else if status.state == "unconfigured" {
        status.default_path = profile::default_library_path().ok();
    }
    Ok(status)
}

fn validate_setup_candidate(path: PathBuf, new_library: bool) -> anyhow::Result<PathBuf> {
    let target = if new_library {
        profile::new_library_path(&path)
    } else {
        path
    };
    anyhow::ensure!(target.is_absolute(), "root_not_absolute");
    storage::validate_library_path(&target)?;
    if target.exists() {
        return storage::checked_library_root(&target);
    }
    let parent = storage::checked_library_root(target.parent().context("root_offline")?)?;
    Ok(parent.join(target.file_name().context("root_offline")?))
}

fn choose_setup_candidate(
    setup: &SetupState,
    path: PathBuf,
    new_library: bool,
    locale: Option<&str>,
) -> anyhow::Result<bool> {
    let candidate = validate_setup_candidate(path, new_library)?;
    let existing = if candidate.exists() {
        Catalogue::probe_setup_preferences(&candidate)?
    } else {
        None
    };
    if existing.is_some_and(|preferences| preferences["setupComplete"] == true) {
        return Ok(false);
    }
    if let Some(locale) = locale {
        setup
            .catalogue
            .command("preferences", json!({"patch":{"locale":locale}}))?;
    }
    *setup
        .candidate
        .lock()
        .map_err(|_| anyhow::anyhow!("library_unavailable"))? = candidate;
    Ok(true)
}

fn setup_finish_patch(setup: &SetupState, args: &Value) -> anyhow::Result<Value> {
    let mut patch = args.get("patch").unwrap_or(args).clone();
    let fields = patch
        .as_object_mut()
        .context("catalogue.invalid_preferences")?;
    fields.remove("setupComplete");
    fields.remove("libraryRootId");
    // Validate all user choices before touching the target. Failed Finish keeps
    // the same memory accounts and draft choices available for ordinary retry.
    setup
        .catalogue
        .command("preferences", json!({"patch":patch}))?;
    let preferences = setup
        .catalogue
        .get("preferences", "app")?
        .context("catalogue.preferences_missing")?;
    let mut patch = serde_json::Map::new();
    for (key, value) in preferences
        .as_object()
        .context("catalogue.invalid_preferences")?
    {
        if !matches!(key.as_str(), "id" | "libraryRootId" | "setupComplete")
            && setup.initial_preferences.get(key) != Some(value)
        {
            patch.insert(key.clone(), value.clone());
        }
    }
    // Explicit Finish values also apply when they equal this build's defaults.
    for (key, value) in args
        .get("patch")
        .unwrap_or(args)
        .as_object()
        .context("catalogue.invalid_preferences")?
    {
        if !matches!(key.as_str(), "id" | "libraryRootId" | "setupComplete") {
            patch.insert(key.clone(), value.clone());
        }
    }
    patch.insert("setupComplete".into(), json!(true));
    Ok(Value::Object(patch))
}

fn materialize_setup(
    setup: &SetupState,
    device_dir: &std::path::Path,
    patch: Value,
) -> anyhow::Result<(Catalogue, Value)> {
    let candidate = validate_setup_candidate(
        setup
            .candidate()
            .map_err(|error| anyhow::anyhow!(error.code))?,
        false,
    )?;
    if candidate.exists() {
        Catalogue::probe_setup_preferences(&candidate)?;
    }
    let previous_selection = profile::selected_library(device_dir)?;
    if !candidate.exists() {
        std::fs::create_dir(&candidate).context("storage.create_directory_failed")?;
    }
    let target = Catalogue::open_bound(&candidate, device_dir)?;
    let root = target
        .library_root()
        .context("library_unavailable")?
        .to_path_buf();
    let existing = target.get("preferences", "app")?;
    if let Some(preferences) = existing.filter(|preferences| preferences["setupComplete"] == true) {
        // An existing completed library has its own accounts and policy. Opening
        // it never applies the preceding new-library setup draft.
        profile::select_library(
            device_dir,
            &profile::LibrarySelection {
                path: root,
                locale: preferences["locale"].as_str().unwrap_or("en").into(),
            },
        )?;
        return Ok((target, preferences));
    }
    let accounts = setup.catalogue.list("account")?;
    let mut selected = false;
    let result = target.commit_setup(patch.clone(), accounts, || {
        profile::select_library(
            device_dir,
            &profile::LibrarySelection {
                path: root,
                locale: patch["locale"].as_str().unwrap_or("en").into(),
            },
        )?;
        selected = true;
        Ok(())
    });
    if result.is_err() && selected {
        // SQL commit failed after the locator write. Restore only this exact
        // device locator; the target transaction rolls back without publication.
        match previous_selection {
            Some(selection) => profile::select_library(device_dir, &selection)?,
            None => match std::fs::remove_file(device_dir.join("library.json")) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => anyhow::bail!("library_restart_failed"),
            },
        }
    }
    Ok((target, result?))
}

async fn finish_staged_setup(
    app: tauri::AppHandle,
    host: &LibraryHost,
    setup: SetupState,
    args: Value,
) -> Result<Value, UiError> {
    host.begin_opening()?;
    let result = async {
        let permit = setup
            .catalogue
            .begin_restore_work()
            .map_err(UiError::from_error)?;
        let patch = setup_finish_patch(&setup, &args).map_err(UiError::from_error)?;
        profile::prepare_restart_handoff().map_err(UiError::from_error)?;
        let device_dir = host.device_dir.clone();
        let (catalogue, preferences) = tauri::async_runtime::spawn_blocking(move || {
            materialize_setup(&setup, &device_dir, patch)
        })
        .await
        .map_err(|_| UiError {
            code: "worker_failed".into(),
        })?
        .map_err(UiError::from_error)?;
        *host.next_catalogue.lock().map_err(|_| UiError {
            code: "library_unavailable".into(),
        })? = Some(catalogue);
        let preferences = retain_setup_finish(host, preferences, permit)?;
        restart_for_library(app);
        Ok(preferences)
    }
    .await;
    if result.is_err() {
        host.opening.store(false, Ordering::Release);
    }
    result
}

async fn finish_setup(
    app: tauri::AppHandle,
    host: &LibraryHost,
    services: Services,
    args: Value,
) -> Result<Value, UiError> {
    host.begin_opening()?;
    let result = async {
        let permit = services
            .catalogue
            .begin_restore_work()
            .map_err(UiError::from_error)?;
        // Prepare the protected replacement before committing completion. A failed
        // handoff keeps setup incomplete and permits an ordinary retry.
        profile::prepare_restart_handoff().map_err(UiError::from_error)?;
        let catalogue = services.catalogue.clone();
        let preferences =
            tauri::async_runtime::spawn_blocking(move || catalogue.command("preferences", args))
                .await
                .map_err(|_| UiError {
                    code: "worker_failed".into(),
                })?
                .map_err(UiError::from_error)?;
        let preferences = retain_setup_finish(host, preferences, permit)?;
        restart_for_library(app);
        Ok(preferences)
    }
    .await;
    if result.is_err() {
        host.opening.store(false, Ordering::Release);
    }
    result
}

fn retain_setup_finish(
    host: &LibraryHost,
    mut preferences: Value,
    permit: catalogue::CatalogueWorkPermit,
) -> Result<Value, UiError> {
    *host.switching.lock().map_err(|_| UiError {
        code: "library_unavailable".into(),
    })? = Some(permit);
    preferences["restartRequired"] = json!(true);
    Ok(preferences)
}

fn open_selected_catalogue(
    path: PathBuf,
    device_dir: PathBuf,
    current: Option<Catalogue>,
    new_library: bool,
) -> anyhow::Result<Catalogue> {
    let path = if new_library {
        let destination = profile::new_library_path(&path);
        if !destination.exists() {
            let parent = destination.parent().context("root_offline")?;
            storage::checked_library_root(parent)?;
            storage::validate_library_path(&destination)?;
            std::fs::create_dir(&destination).context("storage.create_directory_failed")?;
        }
        destination
    } else {
        path
    };
    if let Some(current) = current {
        if std::fs::canonicalize(&path).ok().as_deref() == current.library_root() {
            current.check_library_available()?;
            return Ok(current);
        }
    }
    Catalogue::open_bound(&path, &device_dir)
}

fn restart_for_library(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        window_state::flush_app(&app).await;
        app.restart();
    });
}

#[tauri::command]
async fn library_open(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    host: tauri::State<'_, LibraryHost>,
    path: PathBuf,
    locale: Option<String>,
    new_library: Option<bool>,
) -> Result<Value, UiError> {
    let _operation = trusted(&window)?;
    host.begin_opening()?;
    let result = async {
        if app_reset::pending(&host.device_dir) {
            if host.status.path.as_ref() != Some(&path) {
                return Err(UiError {
                    code: "reset_pending".into(),
                });
            }
            let device = host.device_dir.clone();
            tauri::async_runtime::spawn_blocking(move || app_reset::apply_pending(&device))
                .await
                .map_err(|_| UiError {
                    code: "worker_failed".into(),
                })?
                .map_err(UiError::from_error)?;
            profile::prepare_restart_handoff().map_err(UiError::from_error)?;
            restart_for_library(app);
            return Ok(json!({"restarting":true}));
        }
        let setup = match host.active_setup()? {
            Some(setup) => Some(setup),
            None if host.active_services()?.is_none() && host.status.state == "unconfigured" => {
                let candidate = validate_setup_candidate(path.clone(), new_library == Some(true))
                    .map_err(UiError::from_error)?;
                let existing = if candidate.exists() {
                    Catalogue::probe_setup_preferences(&candidate).map_err(UiError::from_error)?
                } else {
                    None
                };
                if existing.is_some_and(|preferences| preferences["setupComplete"] == true) {
                    None
                } else {
                    let setup = SetupState::new(
                        &host.device_dir,
                        candidate,
                        locale.as_deref().unwrap_or("en"),
                    )
                    .map_err(UiError::from_error)?;
                    *host.setup.lock().map_err(|_| UiError {
                        code: "library_unavailable".into(),
                    })? = Some(setup.clone());
                    Some(setup)
                }
            }
            None => None,
        };
        if let Some(setup) = setup {
            if choose_setup_candidate(
                &setup,
                path.clone(),
                new_library == Some(true),
                locale.as_deref(),
            )
            .map_err(UiError::from_error)?
            {
                host.opening.store(false, Ordering::Release);
                changed(&app, "state");
                return Ok(json!({"restarting":false}));
            }
        }
        let active = host.active_services()?;
        let permit = if let Some(services) = active.as_ref() {
            Some(
                services
                    .catalogue
                    .begin_restore_work()
                    .map_err(UiError::from_error)?,
            )
        } else if let Some(setup) = host.active_setup()? {
            Some(
                setup
                    .catalogue
                    .begin_restore_work()
                    .map_err(UiError::from_error)?,
            )
        } else {
            None
        };
        let device_dir = host.device_dir.clone();
        let current = active.as_ref().map(|services| services.catalogue.clone());
        let catalogue = tauri::async_runtime::spawn_blocking(move || {
            open_selected_catalogue(path, device_dir, current, new_library == Some(true))
        })
        .await
        .map_err(|_| UiError {
            code: "worker_failed".into(),
        })?
        .map_err(UiError::from_error)?;
        let root = catalogue
            .library_root()
            .ok_or_else(|| UiError {
                code: "library_unavailable".into(),
            })?
            .to_path_buf();
        let previous = catalogue
            .get("preferences", "app")
            .map_err(UiError::from_error)?
            .unwrap_or(json!({}));
        let locale = locale
            .filter(|value| matches!(value.as_str(), "en" | "ja" | "ko" | "zh-Hans" | "zh-Hant"))
            .unwrap_or_else(|| previous["locale"].as_str().unwrap_or("en").into());
        if previous["setupComplete"] != true {
            catalogue
                .command("preferences", json!({"patch":{"locale":locale}}))
                .map_err(UiError::from_error)?;
        }
        profile::prepare_restart_handoff().map_err(UiError::from_error)?;
        profile::select_library(
            &host.device_dir,
            &profile::LibrarySelection { path: root, locale },
        )
        .map_err(UiError::from_error)?;
        *host.next_catalogue.lock().map_err(|_| UiError {
            code: "library_unavailable".into(),
        })? = Some(catalogue);
        *host.switching.lock().map_err(|_| UiError {
            code: "library_unavailable".into(),
        })? = permit;
        restart_for_library(app);
        Ok(json!({"restarting":true}))
    }
    .await;
    if result.is_err() {
        host.opening.store(false, Ordering::Release);
    }
    result
}
#[tauri::command]
async fn updates_check(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, updates::UpdateState>,
) -> Result<updates::UpdateCheck, UiError> {
    let _operation = trusted(&window)?;
    state.check(&app).await.map_err(UiError::from_error)
}

#[tauri::command]
async fn updates_install(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    host: tauri::State<'_, LibraryHost>,
    state: tauri::State<'_, updates::UpdateState>,
    gate: tauri::State<'_, NativeOperationGate>,
    version: String,
) -> Result<(), UiError> {
    trusted_access(&window)?;
    let _install = state.begin_install().map_err(UiError::from_error)?;
    let _commands = gate.0.clone().try_write_owned()
        .map_err(|_| UiError { code: "updates_busy".into() })?;
    if host.opening.load(Ordering::Acquire) || host.active_setup()?.is_some() {
        return Err(UiError { code: "updates_busy".into() });
    }
    let services = host.services()?;
    if services.setup_only {
        return Err(UiError { code: "updates_busy".into() });
    }
    // The catalogue's existing gate atomically rejects live and queued work.
    // Keep the library's OS lock until updater installation exits this process.
    let _idle = services.catalogue.begin_restore_work().map_err(|error| {
        if error.to_string() == "storage_busy" {
            UiError { code: "updates_busy".into() }
        } else { UiError::from_error(error) }
    })?;
    state.install(&app, &version).await.map_err(UiError::from_error)
}

pub fn run() {
    let mut context = tauri::generate_context!();
    profile::configure(&mut context).expect("NekoNekoAsset profile could not be selected");
    // Apply remembered bounds before revealing the main window. Authored hidden
    // verification windows keep their configured visibility and never gain focus.
    let main_visible = context.config().app.windows.iter()
        .find(|window| window.label == "main").is_some_and(|window| window.visible);
    if let Some(window) = context.config_mut().app.windows.iter_mut()
        .find(|window| window.label == "main") {
        window.visible = false;
    }
    tauri::Builder::default()
        .manage(NativeOperationGate::default())
        .manage(updates::UpdateState::default())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .on_window_event(|window, event| {
            window_state::on_event(window, event);
            if window.label() == "main"
                && matches!(event, tauri::WindowEvent::CloseRequested { .. })
            {
                // Hidden authentication WebViews must not keep the desktop app
                // alive after its main window closes. Durable jobs resume on launch.
                window.app_handle().exit(0);
            }
        })
        .setup(move |app| {
            let data_dir = if cfg!(debug_assertions) {
                std::env::var_os("NNA_DATA_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or(app.path().app_local_data_dir()?)
            } else {
                app.path().app_local_data_dir()?
            };
            std::fs::create_dir_all(&data_dir)?;
            window_state::initialize(app, &data_dir, main_visible);
            // Explicit reset retains its unconfigured fallback; no unrelated
            // completed default is opened or changed while returning to setup.
            let applying_reset = app_reset::pending(&data_dir);
            let selection = app_reset::apply_pending(&data_dir)
                .map_err(|_| anyhow::anyhow!("reset_incomplete"))
                .and_then(|_| profile::selected_library(&data_dir));
            let mut setup = None;
            let (catalogue, mut status) = match selection {
                Ok(Some(selection)) => {
                    let opened: anyhow::Result<Option<Catalogue>> = (|| {
                        let preferences = Catalogue::probe_setup_preferences(&selection.path)?;
                        if preferences
                            .is_some_and(|preferences| preferences["setupComplete"] == true)
                        {
                            Catalogue::open_bound(&selection.path, &data_dir).map(Some)
                        } else {
                            let candidate =
                                validate_setup_candidate(selection.path.clone(), false)?;
                            setup = Some(SetupState::new(&data_dir, candidate, &selection.locale)?);
                            Ok(None)
                        }
                    })();
                    let error = opened
                        .as_ref()
                        .err()
                        .map(|error| UiError::from_error(anyhow::anyhow!(error.to_string())).code);
                    (
                        opened.ok().flatten(),
                        LibraryStatus {
                            state: if error.is_some() {
                                "blocked"
                            } else if setup.is_some() {
                                "setup"
                            } else {
                                "ready"
                            }
                            .into(),
                            path: Some(selection.path),
                            default_path: None,
                            error_code: error,
                            locale: selection.locale,
                            missing_credentials: vec![],
                        },
                    )
                }
                Ok(None) if !applying_reset => {
                    let default_path = profile::default_library_path().ok();
                    let opened = default_path
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("library_unavailable"))
                        .and_then(|path| validate_setup_candidate(path.clone(), false))
                        .and_then(|path| SetupState::new(&data_dir, path, "en"));
                    let error = opened
                        .as_ref()
                        .err()
                        .map(|error| UiError::from_error(anyhow::anyhow!(error.to_string())).code);
                    let path = opened
                        .as_ref()
                        .ok()
                        .and_then(|setup| setup.candidate().ok());
                    setup = opened.ok();
                    (
                        None,
                        LibraryStatus {
                            state: if error.is_some() {
                                "unconfigured"
                            } else {
                                "setup"
                            }
                            .into(),
                            path,
                            default_path,
                            error_code: error,
                            locale: "en".into(),
                            missing_credentials: vec![],
                        },
                    )
                }
                Ok(None) => (
                    None,
                    LibraryStatus {
                        state: "unconfigured".into(),
                        path: None,
                        default_path: None,
                        error_code: None,
                        locale: "en".into(),
                        missing_credentials: vec![],
                    },
                ),
                Err(error) => (
                    None,
                    LibraryStatus {
                        state: "blocked".into(),
                        path: profile::selected_library(&data_dir)
                            .ok()
                            .flatten()
                            .map(|selection| selection.path)
                            .or_else(|| app_reset::pending_library(&data_dir)),
                        default_path: None,
                        error_code: Some(UiError::from_error(error).code),
                        locale: "en".into(),
                        missing_credentials: vec![],
                    },
                ),
            };
            let Some(catalogue) = catalogue else {
                app.manage(LibraryHost {
                    device_dir: data_dir,
                    status,
                    services: Mutex::new(None),
                    setup: Mutex::new(setup),
                    opening: AtomicBool::new(false),
                    next_catalogue: Mutex::new(None),
                    switching: Mutex::new(None),
                });
                return Ok(());
            };
            if let Some(preferences) = catalogue.get("preferences", "app")? {
                if let Some(locale) = preferences["locale"].as_str() {
                    status.locale = locale.into();
                }
            }
            let services = Services::new(catalogue.clone(), &data_dir)?;
            let setup_only = services.setup_only;
            let stores = services.stores.clone();
            let storage = services.storage.clone();
            let ai_jobs = services.ai_jobs.clone();
            let unity = services.unity.clone();
            app.asset_protocol_scope()
                .allow_directory(catalogue.data_dir().join("media"), true)?;
            app.asset_protocol_scope()
                .allow_directory(catalogue.data_dir().join("cache").join("previews"), true)?;
            app.manage(LibraryHost {
                device_dir: data_dir,
                status,
                services: Mutex::new(Some(services)),
                setup: Mutex::new(None),
                opening: AtomicBool::new(false),
                next_catalogue: Mutex::new(None),
                switching: Mutex::new(None),
            });
            // Setup needs only explicit account/login commands. Starting the
            // long-lived workers here would retain an old library after a new
            // location choice and could begin downloads before Finish.
            if setup_only {
                return Ok(());
            }
            catalogue.start_sync_worker()?;
            let automatic_storage = storage.clone();
            app.listen("nna:changed", move |event| {
                let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                    return;
                };
                if payload["sessionChanged"] == true {
                    let storage = automatic_storage.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = storage.reevaluate_automatic().await;
                    });
                    return;
                }
                if payload["scope"] != "catalogue" {
                    return;
                }
                let Some(ids) = payload["productIds"].as_array() else {
                    return;
                };
                let ids: Vec<String> = ids
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                if ids.is_empty() {
                    return;
                }
                let storage = automatic_storage.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = storage.source_products_changed(&ids).await;
                });
            });
            let handle = app.handle().clone();
            let startup_gate = handle.state::<NativeOperationGate>().0.clone();
            tauri::async_runtime::spawn(async move {
                // A failed installer releases this gate and startup work resumes normally.
                let startup_permit = startup_gate.clone().read_owned().await;
                let mut previous = catalogue.change_domains().unwrap_or((0, 0, 0));
                let setup_was_complete = catalogue
                    .get("preferences", "app")
                    .ok()
                    .flatten()
                    .is_some_and(|preferences| preferences["setupComplete"] == true);
                if storage.start(handle.clone()).await.is_err() {
                    let _ = handle.emit("nna:changed", json!({"scope":"recovery"}));
                }
                // Explicit login/sync jobs need the workers even when isolated diagnostics
                // suppress automatic startup scans.
                let _ = stores.start(handle.clone()).await;
                let _ = ai_jobs.start(&handle).await;
                let _ = unity.start(&handle).await;
                drop(startup_permit);
                #[cfg(debug_assertions)]
                let skip_startup_sync = std::env::var_os("NNA_DATA_DIR").is_some()
                    && std::env::var("NNA_SKIP_STARTUP_SYNC").as_deref() == Ok("1");
                #[cfg(not(debug_assertions))]
                let skip_startup_sync = false;
                if !skip_startup_sync && setup_was_complete {
                    // Native local state renders before authenticated network work.
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    let _sync_permit = startup_gate.read_owned().await;
                    let _ = sync_connected_accounts(&handle, &catalogue, &stores).await;
                }
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if catalogue.check_library_available().is_err() {
                        // Never continue writing after a surfaced loss of the NAS lock handle.
                        handle.exit(1);
                        break;
                    }
                    if let Ok(revisions) = catalogue.change_domains() {
                        if revisions.1 != previous.1 {
                            changed(&handle, "products");
                        }
                        if revisions.2 != previous.2 {
                            changed(&handle, "collections");
                        }
                        if revisions.0 != previous.0 {
                            changed(&handle, "state");
                        }
                        previous = revisions;
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            library_status,
            library_open,
            ai_action,
            ai_analysis_action,
            ai_suggestion_action,
            catalogue_action,
            booth_action,
            storage_action,
            unity_action,
            native_dialog,
            native_file_action,
            external_open,
            native_zoom,
            preview_cache,
            updates_check,
            updates_install
        ])
        .run(context)
        .expect("NekoNekoAsset could not start");
}
