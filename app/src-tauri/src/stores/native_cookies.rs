//! Windows cookie scope must survive the Cookie crate's normalized domain view.
use super::super::session_cache::ScopedCookie;
use anyhow::{anyhow, Result};
use std::sync::mpsc;
use tauri::{
    webview::cookie::{time::OffsetDateTime, SameSite},
    webview::Cookie,
    WebviewWindow,
};
use webview2_com::{
    GetCookiesCompletedHandler,
    Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Cookie, ICoreWebView2CookieManager, ICoreWebView2_2,
        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX, COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE,
        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
    },
};
use windows::{
    core::{Interface, BOOL, HSTRING, PWSTR},
    Win32::System::Com::CoTaskMemFree,
};

fn failed() -> anyhow::Error {
    anyhow!("COOKIE_ACCESS_FAILED")
}

unsafe fn text(
    get: impl FnOnce(&mut PWSTR) -> windows::core::Result<()>,
) -> windows::core::Result<String> {
    let mut pointer = PWSTR::null();
    get(&mut pointer)?;
    let value = pointer.to_string();
    if !pointer.is_null() {
        CoTaskMemFree(Some(pointer.0.cast()));
    }
    Ok(value?)
}

pub(super) unsafe fn capture(native: &ICoreWebView2Cookie) -> windows::core::Result<ScopedCookie> {
    let name = text(|out| native.Name(out))?;
    let value = text(|out| native.Value(out))?;
    let domain = text(|out| native.Domain(out))?;
    let path = text(|out| native.Path(out))?;
    let mut http_only = BOOL::default();
    let mut secure = BOOL::default();
    let mut session = BOOL::default();
    let mut expires = 0.0;
    let mut same_site = COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX;
    native.IsHttpOnly(&mut http_only)?;
    native.IsSecure(&mut secure)?;
    native.IsSession(&mut session)?;
    native.Expires(&mut expires)?;
    native.SameSite(&mut same_site)?;
    let mut cookie = Cookie::build((name, value))
        .domain(domain.clone())
        .path(path)
        .http_only(http_only.as_bool())
        .secure(secure.as_bool())
        .same_site(match same_site {
            COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT => SameSite::Strict,
            COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE => SameSite::None,
            _ => SameSite::Lax,
        })
        .build();
    if !session.as_bool() && expires >= 0.0 {
        if let Ok(expiry) = OffsetDateTime::from_unix_timestamp(expires as i64) {
            cookie.set_expires(expiry);
        }
    }
    Ok(ScopedCookie {
        cookie,
        domain: Some(domain),
    })
}

pub(super) unsafe fn install(
    manager: &ICoreWebView2CookieManager,
    scoped: ScopedCookie,
) -> windows::core::Result<()> {
    let cookie = scoped.cookie;
    let native = manager.CreateCookie(
        &HSTRING::from(cookie.name()),
        &HSTRING::from(cookie.value()),
        &HSTRING::from(scoped.domain.as_deref().unwrap_or_default()),
        &HSTRING::from(cookie.path().unwrap_or("/")),
    )?;
    if let Some(expiry) = cookie.expires_datetime() {
        native.SetExpires(expiry.unix_timestamp() as f64)?;
    }
    if let Some(value) = cookie.http_only() {
        native.SetIsHttpOnly(value)?;
    }
    if let Some(value) = cookie.secure() {
        native.SetIsSecure(value)?;
    }
    if let Some(value) = cookie.same_site() {
        native.SetSameSite(match value {
            SameSite::Strict => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
            SameSite::Lax => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
            SameSite::None => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE,
        })?;
    }
    manager.AddOrUpdateCookie(&native)
}

// Call only on the blocking session worker; callbacks stay on the native UI thread.
pub(super) fn list(window: &WebviewWindow) -> Result<Vec<ScopedCookie>> {
    list_inner(window, None)
}

pub(super) fn for_url(window: &WebviewWindow, url: &str) -> Result<Vec<ScopedCookie>> {
    list_inner(window, Some(url.to_owned()))
}

fn list_inner(window: &WebviewWindow, uri: Option<String>) -> Result<Vec<ScopedCookie>> {
    let (send, receive) = mpsc::channel();
    window
        .with_webview(move |view| {
            let uri = uri.map(HSTRING::from).unwrap_or_default();
            let callback_send = send.clone();
            let started = (|| unsafe {
                let manager = view
                    .controller()
                    .CoreWebView2()?
                    .cast::<ICoreWebView2_2>()?
                    .CookieManager()?;
                manager.GetCookies(
                    &uri,
                    &GetCookiesCompletedHandler::create(Box::new(move |error, cookies| {
                        let result = (|| {
                            error?;
                            let mut output = vec![];
                            if let Some(cookies) = cookies {
                                let mut count = 0;
                                cookies.Count(&mut count)?;
                                for index in 0..count {
                                    output.push(capture(&cookies.GetValueAtIndex(index)?)?);
                                }
                            }
                            windows::core::Result::Ok(output)
                        })()
                        .map_err(|_| failed());
                        let _ = callback_send.send(result);
                        Ok(())
                    })),
                )
            })();
            if started.is_err() {
                let _ = send.send(Err(failed()));
            }
        })
        .map_err(|_| failed())?;
    receive
        .recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|_| failed())?
}

pub(super) fn set(window: &WebviewWindow, cookies: Vec<ScopedCookie>) -> Result<()> {
    let (send, receive) = mpsc::channel();
    window
        .with_webview(move |view| {
            let result = (|| unsafe {
                let manager = view
                    .controller()
                    .CoreWebView2()?
                    .cast::<ICoreWebView2_2>()?
                    .CookieManager()?;
                for cookie in cookies {
                    install(&manager, cookie)?;
                }
                windows::core::Result::Ok(())
            })()
            .map_err(|_| failed());
            let _ = send.send(result);
        })
        .map_err(|_| failed())?;
    receive
        .recv_timeout(std::time::Duration::from_secs(15))
        .map_err(|_| failed())?
}
