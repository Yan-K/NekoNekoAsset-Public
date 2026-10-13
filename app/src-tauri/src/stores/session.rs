//! Native device-local login profiles. Cookies never cross IPC or catalogue records.
use super::StoreProvider;
use anyhow::{bail, ensure, Result};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use url::Url;

// The deployed first-party language switch declares these hosts explicitly.
pub const JINXXY_HOSTS: &[&str] = &[
    "jinxxy.com",
    "www.jinxxy.com",
    "ja.jinxxy.com",
    "de.jinxxy.com",
    "es.jinxxy.com",
    "fr.jinxxy.com",
    "zh.jinxxy.com",
    "ko.jinxxy.com",
];

pub fn jinxxy_host(host: &str) -> bool {
    JINXXY_HOSTS.contains(&host)
}

fn account_id(account: &str) -> Result<String> {
    Ok(uuid::Uuid::parse_str(account)
        .map_err(|_| anyhow::anyhow!("INVALID_ACCOUNT_ID"))?
        .to_string())
}

fn secure(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}

fn cookie_host(provider: StoreProvider, host: &str) -> bool {
    match provider {
        StoreProvider::Booth => host == "booth.pm" || host.ends_with(".booth.pm"),
        StoreProvider::Gumroad => host == "gumroad.com" || host.ends_with(".gumroad.com"),
        StoreProvider::Jinxxy => jinxxy_host(host) || host == "api.jinxxy.com",
    }
}

/// Proven login origins permit human authentication only; cookie extraction remains first-party.
pub fn navigation_allowed(provider: StoreProvider, url: &Url) -> bool {
    if provider == StoreProvider::Jinxxy && url.host_str() == Some("api.jinxxy.com") {
        return false;
    }
    if !secure(url)
        || !url.host_str().is_some_and(|host| {
            cookie_host(provider, host)
                || match provider {
                    StoreProvider::Gumroad => matches!(
                        host,
                        "accounts.google.com" | "api.twitter.com" | "connect.stripe.com"
                    ),
                    StoreProvider::Jinxxy => host == "discord.com",
                    StoreProvider::Booth => false,
                }
        })
    {
        return false;
    }
    !["/checkout", "/cart", "/download", "/r/", "/d/"]
        .iter()
        .any(|path| url.path().contains(path))
}

pub fn profile_path(data_dir: &Path, provider: StoreProvider, account: &str) -> Result<PathBuf> {
    Ok(data_dir
        .join("auth")
        .join(provider.key())
        .join(account_id(account)?))
}

/// Save only verified first-party cookies, outside catalogue, IPC and backups.
pub async fn remember(
    window: WebviewWindow,
    data_dir: &Path,
    provider: StoreProvider,
    account: &str,
) -> Result<()> {
    let profile = profile_path(data_dir, provider, account)?;
    let account = account_id(account)?;
    tauri::async_runtime::spawn_blocking(move || {
        let cookies = window
            .cookies()
            .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?
            .into_iter()
            .filter(|cookie| {
                cookie
                    .domain()
                    .is_some_and(|domain| cookie_host(provider, domain.trim_start_matches('.')))
            })
            .collect();
        super::session_cache::save(&profile, provider, &account, cookies)
    })
    .await
    .map_err(|_| anyhow::anyhow!("AUTH_SESSION_STORAGE_FAILED"))?
}

/// Restore before any remote navigation, keeping newer cookies already in the native profile.
pub async fn restore(
    window: WebviewWindow,
    data_dir: &Path,
    provider: StoreProvider,
    account: &str,
) -> Result<()> {
    let profile = profile_path(data_dir, provider, account)?;
    let account = account_id(account)?;
    tauri::async_runtime::spawn_blocking(move || {
        let saved = super::session_cache::load(&profile, provider, &account).unwrap_or_default();
        let current = window
            .cookies()
            .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?;
        for cookie in saved {
            if !cookie
                .domain()
                .is_some_and(|domain| cookie_host(provider, domain.trim_start_matches('.')))
                || current.iter().any(|existing| {
                    existing.name() == cookie.name()
                        && existing.domain() == cookie.domain()
                        && existing.path() == cookie.path()
                })
            {
                continue;
            }
            window
                .set_cookie(cookie)
                .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?;
        }
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?
}

pub fn forget(data_dir: &Path, provider: StoreProvider, account: &str) -> Result<()> {
    super::session_cache::clear(&profile_path(data_dir, provider, account)?)
}

/// Login polling must not recreate a window the human has closed.
pub fn existing_window(
    app: &AppHandle,
    provider: StoreProvider,
    account: &str,
) -> Result<Option<WebviewWindow>> {
    Ok(app.get_webview_window(&format!("{}-auth-{}", provider.key(), account_id(account)?)))
}

/// Keep the verified native session alive: closing its last WebView drops session cookies.
/// Failed/cancelled login still destroys the window; logout clears browsing data separately.
pub async fn finish_login(
    app: &AppHandle,
    provider: StoreProvider,
    account: &str,
    verified: bool,
) -> Result<()> {
    if let Some(window) = existing_window(app, provider, account)? {
        tauri::async_runtime::spawn_blocking(move || {
            if verified {
                window.hide()
            } else {
                window.close()
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?
        .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
    }
    Ok(())
}

/// Creating/showing a profile does not establish authentication. The adapter must verify identity.
pub async fn window(
    app: &AppHandle,
    data_dir: &Path,
    provider: StoreProvider,
    account: &str,
    entry: Url,
    visible: bool,
) -> Result<WebviewWindow> {
    // Existing BOOTH windows and Pixiv navigation retain their established implementation.
    ensure!(
        provider != StoreProvider::Booth,
        "LEGACY_BOOTH_SESSION_REQUIRED"
    );
    ensure!(navigation_allowed(provider, &entry), "INVALID_AUTH_URL");
    let id = account_id(account)?;
    let label = format!("{}-auth-{id}", provider.key());
    if let Some(window) = app.get_webview_window(&label) {
        if visible {
            window
                .navigate(entry)
                .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
            window
                .show()
                .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
            window.set_focus().ok();
        }
        return Ok(window);
    }
    let profile = profile_path(data_dir, provider, &id)?;
    let app = app.clone();
    let restore_data = data_dir.to_path_buf();
    let entry_url = entry.clone();
    let created = tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("/usr/bin/sw_vers")
                .arg("-productVersion")
                .output()
                .map_err(|_| anyhow::anyhow!("AUTH_ISOLATION_UNAVAILABLE"))?;
            let major = std::str::from_utf8(&output.stdout)
                .ok()
                .and_then(|value| value.trim().split('.').next())
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            ensure!(
                output.status.success() && major >= 14,
                "AUTH_ISOLATION_UNAVAILABLE"
            );
        }
        std::fs::create_dir_all(&profile).map_err(|_| anyhow::anyhow!("PROFILE_UNAVAILABLE"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| anyhow::anyhow!("PROFILE_UNAVAILABLE"))?;
        }
        let popup_app = app.clone();
        let popup_label = label.clone();
        let builder = WebviewWindowBuilder::new(
            &app,
            label,
            WebviewUrl::External(Url::parse("about:blank").unwrap()),
        )
        .title(provider.label())
        .inner_size(920.0, 760.0)
        .visible(false)
        .devtools(false)
        .data_directory(profile)
        .on_navigation(move |url| {
            url.as_str() == "about:blank" || navigation_allowed(provider, url)
        })
        .on_new_window(move |url, _| {
            if navigation_allowed(provider, &url) {
                if let Some(window) = popup_app.get_webview_window(&popup_label) {
                    let _ = window.navigate(url);
                }
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_download(|_, _| false);
        #[cfg(target_os = "macos")]
        let builder = {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(format!("{}:{id}", provider.key()).as_bytes());
            let mut store = [0u8; 16];
            store.copy_from_slice(&digest[..16]);
            builder.data_store_identifier(store)
        };
        builder
            .build()
            .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))
    })
    .await
    .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))??;
    restore(created.clone(), &restore_data, provider, account).await?;
    created
        .navigate(entry_url)
        .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
    if visible {
        created
            .show()
            .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
        created.set_focus().ok();
    }
    Ok(created)
}

pub async fn jar(
    window: WebviewWindow,
    provider: StoreProvider,
) -> Result<Arc<reqwest::cookie::Jar>> {
    let origins: Vec<Url> = match provider {
        StoreProvider::Booth => bail!("LEGACY_BOOTH_SESSION_REQUIRED"),
        StoreProvider::Gumroad => vec![Url::parse("https://gumroad.com/").unwrap()],
        StoreProvider::Jinxxy => JINXXY_HOSTS
            .iter()
            .copied()
            .chain(std::iter::once("api.jinxxy.com"))
            .map(|host| Url::parse(&format!("https://{host}/")).unwrap())
            .collect(),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let mut count = 0;
        for url in origins {
            for cookie in window
                .cookies_for_url(url.clone())
                .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?
            {
                if cookie.name().is_empty()
                    || cookie.domain().is_some_and(|domain| {
                        !cookie_host(provider, domain.trim_start_matches('.'))
                    })
                {
                    continue;
                }
                jar.add_cookie_str(&cookie.to_string(), &url);
                count += 1;
            }
        }
        ensure!(count != 0, "AUTH_REQUIRED");
        Ok(jar)
    })
    .await
    .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?
}

const JINXXY_ENGLISH_PREFERENCE: &str =
    "jx_locale=en; Domain=.jinxxy.com; Path=/; Max-Age=31536000; SameSite=Lax; Secure";

/// Called only after the adapter has parsed a protected buyer inventory.
/// Change the documented language preference in this profile; auth cookies keep their scope.
pub async fn jinxxy_english(window: WebviewWindow) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let current = window
            .url()
            .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
        ensure!(
            secure(&current) && current.host_str().is_some_and(jinxxy_host),
            "AUTH_REQUIRED"
        );
        // Use the site's own document.cookie mechanism: the Windows native
        // Cookie::domain conversion strips the leading dot of parent domains.
        window
            .eval(&format!(
                "document.cookie = {};",
                serde_json::to_string(JINXXY_ENGLISH_PREFERENCE).unwrap()
            ))
            .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?;
        let apex = Url::parse("https://jinxxy.com/").unwrap();
        let mut applied = false;
        // eval dispatch is asynchronous; confirm only this public preference
        // before snapshotting the jar. Never inspect or alter auth-cookie values.
        for _ in 0..10 {
            let cookies = window
                .cookies_for_url(apex.clone())
                .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?;
            let preferences: Vec<_> = cookies
                .iter()
                .filter(|cookie| cookie.name() == "jx_locale" && cookie.path() == Some("/"))
                .collect();
            if !preferences.is_empty() && preferences.iter().all(|cookie| cookie.value() == "en") {
                applied = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        ensure!(applied, "JINXXY_SOURCE_UNSUPPORTED");
        if current.host_str() != Some("jinxxy.com") {
            window
                .navigate(Url::parse("https://jinxxy.com/my/inventory").unwrap())
                .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
        }
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?
}

pub async fn disconnect(window: WebviewWindow) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        window
            .navigate(Url::parse("about:blank").unwrap())
            .map_err(|_| anyhow::anyhow!("PROFILE_CLEAR_FAILED"))?;
        window
            .clear_all_browsing_data()
            .map_err(|_| anyhow::anyhow!("PROFILE_CLEAR_FAILED"))?;
        // WebView2 submits clearing asynchronously. Do not report logout before cookies are gone.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if window
                .cookies()
                .map_err(|_| anyhow::anyhow!("PROFILE_CLEAR_FAILED"))?
                .is_empty()
            {
                break;
            }
            ensure!(std::time::Instant::now() < deadline, "PROFILE_CLEAR_FAILED");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        window
            .close()
            .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
        Ok(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("PROFILE_CLEAR_FAILED"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jinxxy_languages_use_exact_hosts_and_only_the_language_preference() {
        for host in JINXXY_HOSTS {
            assert!(navigation_allowed(
                StoreProvider::Jinxxy,
                &Url::parse(&format!("https://{host}/my/inventory")).unwrap()
            ));
            assert!(cookie_host(StoreProvider::Jinxxy, host));
        }
        for host in [
            "other.jinxxy.com",
            "ja.jinxxy.com.evil.test",
            "jx.jinxxy.com",
        ] {
            assert!(!jinxxy_host(host));
        }
        let cookie = tauri::webview::Cookie::parse(JINXXY_ENGLISH_PREFERENCE).unwrap();
        assert_eq!((cookie.name(), cookie.value()), ("jx_locale", "en"));
        assert_eq!(cookie.domain(), Some("jinxxy.com"));
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.max_age().unwrap().whole_seconds(), 31536000);
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(
            cookie.same_site(),
            Some(tauri::webview::cookie::SameSite::Lax)
        );
    }

    #[test]
    fn native_profiles_separate_provider_and_account_and_reject_path_injection() {
        let root = Path::new("authored-root");
        let first = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        let second = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
        assert_ne!(
            profile_path(root, StoreProvider::Gumroad, first).unwrap(),
            profile_path(root, StoreProvider::Jinxxy, first).unwrap()
        );
        assert_ne!(
            profile_path(root, StoreProvider::Gumroad, first).unwrap(),
            profile_path(root, StoreProvider::Gumroad, second).unwrap()
        );
        assert!(profile_path(root, StoreProvider::Gumroad, "../other").is_err());
    }

    #[test]
    fn native_navigation_rejects_other_store_credentials_checkout_and_downloads() {
        let url = |value| Url::parse(value).unwrap();
        assert!(navigation_allowed(
            StoreProvider::Gumroad,
            &url("https://gumroad.com/library")
        ));
        assert!(navigation_allowed(
            StoreProvider::Jinxxy,
            &url("https://zh.jinxxy.com/login")
        ));
        assert!(cookie_host(StoreProvider::Jinxxy, "zh.jinxxy.com"));
        assert!(!navigation_allowed(
            StoreProvider::Gumroad,
            &url("https://zh.jinxxy.com/login")
        ));
        for (provider, entry) in [
            (
                StoreProvider::Gumroad,
                "https://accounts.google.com/o/oauth2/auth",
            ),
            (
                StoreProvider::Gumroad,
                "https://api.twitter.com/oauth/authenticate",
            ),
            (
                StoreProvider::Gumroad,
                "https://connect.stripe.com/oauth/authorize",
            ),
            (
                StoreProvider::Jinxxy,
                "https://discord.com/oauth2/authorize",
            ),
        ] {
            let entry = url(entry);
            assert!(navigation_allowed(provider, &entry));
            assert!(!cookie_host(provider, entry.host_str().unwrap()));
            assert!(!navigation_allowed(StoreProvider::Booth, &entry));
            let other = if provider == StoreProvider::Gumroad {
                StoreProvider::Jinxxy
            } else {
                StoreProvider::Gumroad
            };
            assert!(!navigation_allowed(other, &entry));
        }
        for value in [
            "https://jinxxy.com/my/lists",
            "https://gumroad.com.evil.test/login",
            "http://gumroad.com/login",
            "https://user:secret@gumroad.com/login",
            "https://gumroad.com:8443/login",
            "https://gumroad.com/checkout",
            "https://gumroad.com/r/authored-token",
            "https://accounts.google.com.evil.test/o/oauth2/auth",
            "https://unproved.accounts.google.com/o/oauth2/auth",
            "https://user:secret@accounts.google.com/o/oauth2/auth",
            "http://accounts.google.com/o/oauth2/auth",
            "https://accounts.google.com:8443/o/oauth2/auth",
            "https://accounts.google.com/download/fixture.zip",
            "https://127.0.0.1/login",
            "https://10.0.0.1/login",
        ] {
            assert!(!navigation_allowed(StoreProvider::Gumroad, &url(value)));
        }
        for value in [
            "https://discord.com.evil.test/oauth2/authorize",
            "https://unproved.discord.com/login",
            "https://api.jinxxy.com/login",
            "https://discord.com/download/fixture.zip",
            "https://zh.jinxxy.com.evil.test/login",
            "https://other.jinxxy.com/login",
            "https://secret@zh.jinxxy.com/login",
            "http://zh.jinxxy.com/login",
            "https://zh.jinxxy.com:8443/login",
            "https://zh.jinxxy.com/download/fixture.zip",
        ] {
            assert!(!navigation_allowed(StoreProvider::Jinxxy, &url(value)));
        }
    }
}
