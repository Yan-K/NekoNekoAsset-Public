//! Isolated native BOOTH profiles and device-protected first-party session snapshots.
use super::safety;
use anyhow::{bail, Result};
use std::{path::Path, sync::Arc};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub async fn window(
    app: &AppHandle,
    data_dir: &Path,
    account: &str,
    visible: bool,
) -> Result<WebviewWindow> {
    let id = safety::account_id(account)?;
    let label = format!("booth-auth-{id}");
    if let Some(window) = app.get_webview_window(&label) {
        if visible {
            window
                .navigate(url::Url::parse("https://accounts.booth.pm/library").unwrap())
                .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
            window
                .show()
                .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
            window.set_focus().ok();
        }
        return Ok(window);
    }
    let app = app.clone();
    let profile = data_dir.join("auth").join("booth").join(&id);
    let restore_data = data_dir.to_path_buf();
    let created = tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("/usr/bin/sw_vers")
                .arg("-productVersion")
                .output()
                .map_err(|_| anyhow::anyhow!("AUTH_ISOLATION_UNAVAILABLE"))?;
            let major = std::str::from_utf8(&output.stdout)
                .ok()
                .and_then(|v| v.trim().split('.').next())
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            if !output.status.success() || major < 14 {
                bail!("AUTH_ISOLATION_UNAVAILABLE");
            }
        }
        std::fs::create_dir_all(&profile).map_err(|_| anyhow::anyhow!("PROFILE_UNAVAILABLE"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| anyhow::anyhow!("PROFILE_UNAVAILABLE"))?;
        }
        // No remote capabilities or init scripts; lead config grants IPC to main only.
        let popup_app = app.clone();
        let popup_label = label.clone();
        let builder = WebviewWindowBuilder::new(
            &app,
            label,
            WebviewUrl::External(url::Url::parse("about:blank").unwrap()),
        )
        .title("BOOTH / pixiv")
        .inner_size(920.0, 760.0)
        .visible(false)
        .devtools(false)
        .data_directory(profile)
        .on_navigation(|url| url.as_str() == "about:blank" || safety::auth_navigation(url))
        .on_new_window(move |url, _| {
            // Keep provider login redirects in the same isolated profile; never create unscoped remote windows.
            if safety::auth_navigation(&url) {
                if let Some(window) = popup_app.get_webview_window(&popup_label) {
                    let _ = window.navigate(url);
                }
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_download(|_, _| false);
        #[cfg(target_os = "macos")]
        let builder =
            builder.data_store_identifier(*uuid::Uuid::parse_str(&id).unwrap().as_bytes());
        builder
            .build()
            .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))
    })
    .await
    .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))??;
    crate::stores::session::restore(
        created.clone(),
        &restore_data,
        crate::stores::StoreProvider::Booth,
        account,
    )
    .await?;
    created
        .navigate(url::Url::parse("https://accounts.booth.pm/library").unwrap())
        .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
    if visible {
        created
            .show()
            .map_err(|_| anyhow::anyhow!("AUTH_WINDOW_FAILED"))?;
        created.set_focus().ok();
    }
    Ok(created)
}
pub async fn jar(window: WebviewWindow) -> Result<Arc<reqwest::cookie::Jar>> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut cookies = Vec::new();
        for origin in ["https://accounts.booth.pm/", "https://booth.pm/"] {
            let url = url::Url::parse(origin).unwrap();
            for cookie in crate::stores::session::cookies_for_request(&window, &url)? {
                // The jar enforces scope on subsequent redirects; never set Cookie headers globally.
                if cookie
                    .domain()
                    .is_some_and(|d| !safety::booth_host(d.trim_start_matches('.')))
                {
                    continue;
                }
                if cookie.name().is_empty() {
                    continue;
                }
                cookies.push((url.clone(), cookie));
            }
        }
        if cookies.is_empty() {
            bail!("AUTH_REQUIRED");
        }
        Ok(request_jar(cookies))
    })
    .await
    .map_err(|_| anyhow::anyhow!("COOKIE_ACCESS_FAILED"))?
}
/// The jar keys a host-only cookie and a domain cookie for the same host alike, so
/// the later one replaces the other. BOOTH re-issues its session for .booth.pm; a
/// same-named host-only booth.pm copy added last left accounts.booth.pm signed out.
fn request_jar(
    mut cookies: Vec<(url::Url, tauri::webview::Cookie<'static>)>,
) -> Arc<reqwest::cookie::Jar> {
    cookies.sort_by_key(|(_, cookie)| cookie.domain().is_some());
    let jar = Arc::new(reqwest::cookie::Jar::default());
    for (url, cookie) in cookies {
        jar.add_cookie_str(&cookie.to_string(), &url);
    }
    jar
}
pub async fn disconnect(app: &AppHandle, data_dir: &Path, account: &str) -> Result<()> {
    let window = window(app, data_dir, account, false).await?;
    crate::stores::session::forget(data_dir, crate::stores::StoreProvider::Booth, account)?;
    crate::stores::session::disconnect(window).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::cookie::CookieStore;

    #[test]
    fn domain_session_cookie_wins_over_a_same_named_host_only_copy() {
        let apex = url::Url::parse("https://booth.pm/").unwrap();
        let current = tauri::webview::Cookie::build(("authored_session", "CURRENT"))
            .domain("booth.pm")
            .path("/")
            .secure(true)
            .build();
        let stale = tauri::webview::Cookie::build(("authored_session", "STALE"))
            .path("/")
            .secure(true)
            .build();
        // The login window can return the two copies in either order.
        for order in [[current.clone(), stale.clone()], [stale.clone(), current.clone()]] {
            let jar = request_jar(order.into_iter().map(|cookie| (apex.clone(), cookie)).collect());
            for url in ["https://accounts.booth.pm/library?page=1", "https://booth.pm/downloadables/1"] {
                let header = jar.cookies(&url::Url::parse(url).unwrap());
                assert_eq!(header.as_ref().and_then(|v| v.to_str().ok()), Some("authored_session=CURRENT"));
            }
        }
    }
}
