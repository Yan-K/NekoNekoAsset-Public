//! Public release metadata and signed installer updates. No catalogue or credential data is sent.
use anyhow::{ensure, Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;
use url::Url;

const BRIDGE_ENDPOINT: &str = "https://yan-k.github.io/NekoNekoAsset-Public/bridge/latest.json";
const RELEASE_ROOT: &str = "https://github.com/Yan-K/NekoNekoAsset-Public/releases";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppRelease {
    version: String,
    notes: String,
    release_url: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BridgeRelease {
    version: String,
    unitypackage_url: String,
    release_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppCheck {
    current_version: String,
    install_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    available: Option<AppRelease>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateCheck {
    app: AppCheck,
    #[serde(skip_serializing_if = "Option::is_none")]
    bridge: Option<BridgeRelease>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bridge_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct UpdateState {
    checked: Mutex<Option<Update>>,
    checking: Mutex<()>,
    installing: Arc<AtomicBool>,
}

/// The caller also holds the global command and idle-catalogue permits through installer exit.
pub(crate) struct InstallPermit(Arc<AtomicBool>);
impl Drop for InstallPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn portable_executable(executable: &Path) -> bool {
    executable
        .parent()
        .is_some_and(|parent| parent.join("portable.flag").is_file())
}

fn install_supported(app: &AppHandle) -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
        && !app.config().identifier.contains(".portable.")
        && std::env::current_exe().is_ok_and(|path| !portable_executable(&path))
}

fn release_url(version: &str) -> String {
    format!("{RELEASE_ROOT}/tag/v{version}")
}

fn valid_asset_url(value: &Url, version: &str, suffix: &str) -> bool {
    let prefix = format!("/Yan-K/NekoNekoAsset-Public/releases/download/v{version}/");
    value.scheme() == "https"
        && value.host_str() == Some("github.com")
        && value.username().is_empty()
        && value.password().is_none()
        && value.port().is_none()
        && value.query().is_none()
        && value.fragment().is_none()
        && value.path().strip_prefix(&prefix).is_some_and(|name| {
            !name.is_empty() && !name.contains('/') && name.to_ascii_lowercase().ends_with(suffix)
        })
}

fn validate_bridge(release: BridgeRelease) -> Result<BridgeRelease> {
    let version = Version::parse(&release.version).context("updates_invalid_manifest")?;
    ensure!(
        version.to_string() == release.version,
        "updates_invalid_manifest"
    );
    let asset = Url::parse(&release.unitypackage_url).context("updates_invalid_manifest")?;
    ensure!(
        valid_asset_url(&asset, &release.version, ".unitypackage")
            && release.release_url == release_url(&release.version),
        "updates_invalid_manifest"
    );
    Ok(release)
}

async fn check_bridge() -> Result<BridgeRelease> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("updates_unavailable")?;
    let mut response = client
        .get(BRIDGE_ENDPOINT)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .context("updates_unavailable")?
        .error_for_status()
        .context("updates_unavailable")?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context("updates_unavailable")? {
        ensure!(
            bytes.len() + chunk.len() <= 64 * 1024,
            "updates_invalid_manifest"
        );
        bytes.extend_from_slice(&chunk);
    }
    validate_bridge(serde_json::from_slice(&bytes).context("updates_invalid_manifest")?)
}

fn public_error(error: &anyhow::Error) -> String {
    if error.to_string().starts_with("updates_invalid_manifest") {
        "updates_invalid_manifest".into()
    } else {
        "updates_unavailable".into()
    }
}

fn download_error(error: tauri_plugin_updater::Error) -> anyhow::Error {
    use tauri_plugin_updater::Error;
    let code = match error {
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => "updates_signature_invalid",
        _ => "updates_download_failed",
    };
    anyhow::anyhow!(code)
}

fn progress(app: &AppHandle, stage: &str, downloaded: u64, total: Option<u64>) {
    let _ = app.emit_to(
        "main",
        "updates:progress",
        serde_json::json!({
            "stage":stage,"downloadedBytes":downloaded,"totalBytes":total
        }),
    );
}

impl UpdateState {
    pub(crate) fn begin_install(&self) -> Result<InstallPermit> {
        ensure!(
            !self.installing.swap(true, Ordering::AcqRel),
            "updates_busy"
        );
        Ok(InstallPermit(self.installing.clone()))
    }

    pub(crate) async fn check(&self, app: &AppHandle) -> Result<UpdateCheck> {
        ensure!(!self.installing.load(Ordering::Acquire), "updates_busy");
        let _check = self.checking.lock().await;
        let check_app = async {
            if !cfg!(all(windows, target_arch = "x86_64")) {
                return Ok(None);
            }
            let updater = app
                .updater_builder()
                // Windows launch can fail after this callback; leave the App usable for retry.
                .on_before_exit(|| {})
                .timeout(Duration::from_secs(15))
                .header("Cache-Control", "no-cache")
                .map_err(|_| anyhow::anyhow!("updates_unavailable"))?
                .build()
                .map_err(|_| anyhow::anyhow!("updates_unavailable"))?;
            let update = updater
                .check()
                .await
                .map_err(|_| anyhow::anyhow!("updates_unavailable"))?;
            if let Some(update) = &update {
                ensure!(
                    Version::parse(&update.version).is_ok()
                        && valid_asset_url(&update.download_url, &update.version, "-setup.exe")
                        && !update.signature.trim().is_empty(),
                    "updates_invalid_manifest"
                );
            }
            Ok::<_, anyhow::Error>(update)
        };
        let (app_result, bridge_result) = tokio::join!(check_app, check_bridge());
        let (update, app_error) = match app_result {
            Ok(update) => (update, None),
            Err(error) => (None, Some(public_error(&error))),
        };
        let available = update.as_ref().map(|update| AppRelease {
            version: update.version.clone(),
            notes: update.body.clone().unwrap_or_default(),
            release_url: release_url(&update.version),
        });
        *self.checked.lock().await = update;
        let (bridge, bridge_error) = match bridge_result {
            Ok(release) => (Some(release), None),
            Err(error) => (None, Some(public_error(&error))),
        };
        Ok(UpdateCheck {
            app: AppCheck {
                current_version: app.package_info().version.to_string(),
                install_supported: install_supported(app),
                available,
                error_code: app_error,
            },
            bridge,
            bridge_error,
        })
    }

    /// A signed download never mutates the catalogue. Windows exits only after launching NSIS.
    pub(crate) async fn install(&self, app: &AppHandle, expected_version: &str) -> Result<()> {
        ensure!(self.installing.load(Ordering::Acquire), "updates_busy");
        ensure!(install_supported(app), "updates_portable_unsupported");
        let mut update = self.checked.lock().await.clone().context("updates_stale")?;
        ensure!(update.version == expected_version, "updates_stale");
        ensure!(
            valid_asset_url(&update.download_url, &update.version, "-setup.exe"),
            "updates_invalid_manifest"
        );
        update.timeout = Some(Duration::from_secs(600));
        progress(app, "downloading", 0, None);
        let mut downloaded = 0u64;
        let mut last_progress = Instant::now();
        let bytes = update
            .download(
                |chunk, total| {
                    downloaded = downloaded.saturating_add(chunk as u64);
                    if last_progress.elapsed() >= Duration::from_millis(150)
                        || Some(downloaded) == total
                    {
                        progress(app, "downloading", downloaded, total);
                        last_progress = Instant::now();
                    }
                },
                || {
                    progress(app, "verifying", 0, None);
                },
            )
            .await
            .map_err(download_error)?;
        progress(
            app,
            "installing",
            bytes.len() as u64,
            Some(bytes.len() as u64),
        );
        update
            .install(&bytes)
            .map_err(|_| anyhow::anyhow!("updates_install_failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_assets_are_bound_to_repo_version_and_format() {
        let good = "https://github.com/Yan-K/NekoNekoAsset-Public/releases/download/v0.2.1/NekoNekoAsset_0.2.1_x64-setup.exe";
        assert!(valid_asset_url(
            &Url::parse(good).unwrap(),
            "0.2.1",
            "-setup.exe"
        ));
        for bad in [
            good.replace("https:", "http:"),
            good.replace("Yan-K/", "other/"),
            good.replace("v0.2.1/", "v0.2.0/"),
            format!("{good}?source=elsewhere"),
            good.replace("-setup.exe", "-portable.zip"),
        ] {
            assert!(!valid_asset_url(
                &Url::parse(&bad).unwrap(),
                "0.2.1",
                "-setup.exe"
            ));
        }
    }
    #[test]
    fn bridge_manifest_rejects_invalid_or_mismatched_release() {
        let fixture = BridgeRelease {
            version: "0.2.0".into(),
            unitypackage_url: format!(
                "{RELEASE_ROOT}/download/v0.2.0/NekoNekoAsset-Bridge-0.2.0.unitypackage"
            ),
            release_url: release_url("0.2.0"),
        };
        assert!(validate_bridge(fixture.clone()).is_ok());
        let mut changed = fixture.clone();
        changed.version = "0.3.0".into();
        assert!(validate_bridge(changed).is_err());
        let mut changed = fixture;
        changed.version = "beta".into();
        assert!(validate_bridge(changed).is_err());
    }
    #[test]
    fn failed_or_finished_install_releases_single_operation_slot() {
        let state = UpdateState::default();
        let first = state.begin_install().unwrap();
        assert!(state.begin_install().is_err());
        drop(first);
        assert!(state.begin_install().is_ok());
    }
}
