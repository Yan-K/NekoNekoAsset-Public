use crate::catalogue::Catalogue;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

pub fn dialog(app: &tauri::AppHandle, kind: &str, options: Value) -> Result<Value> {
    let mut picker = app.dialog().file();
    if let Some(path) = options.get("defaultPath").and_then(Value::as_str) {
        picker = picker.set_directory(path);
    }
    if let Some(name) = options.get("defaultName").and_then(Value::as_str) {
        picker = picker.set_file_name(name);
    }
    if let Some(filters) = options.get("filters").and_then(Value::as_array) {
        for filter in filters {
            if let (Some(name), Some(extensions)) = (
                filter.get("name").and_then(Value::as_str),
                filter.get("extensions").and_then(Value::as_array),
            ) {
                let extensions: Vec<&str> = extensions.iter().filter_map(Value::as_str).collect();
                picker = picker.add_filter(name, &extensions);
            }
        }
    }
    let result = match kind {
        "folder" => {
            if options.get("multiple").and_then(Value::as_bool) == Some(true) {
                return picker
                    .blocking_pick_folders()
                    .map(|files| {
                        files
                            .into_iter()
                            .map(|file| file.into_path().map(|p| json!(p)))
                            .collect::<std::result::Result<Vec<Value>, _>>()
                            .map(Value::Array)
                            .map_err(Into::into)
                    })
                    .unwrap_or(Ok(Value::Null));
            }
            picker.blocking_pick_folder()
        }
        "open_file" => {
            if options.get("multiple").and_then(Value::as_bool) == Some(true) {
                let files = picker.blocking_pick_files();
                return files
                    .map(|files| {
                        files
                            .into_iter()
                            .map(|file| file.into_path().map(|p| json!(p)))
                            .collect::<std::result::Result<Vec<Value>, _>>()
                            .map(Value::Array)
                            .map_err(Into::into)
                    })
                    .unwrap_or(Ok(Value::Null));
            }
            picker.blocking_pick_file()
        }
        "save_file" => picker.blocking_save_file(),
        _ => bail!("invalid_dialog_kind"),
    };
    result
        .map(|file| file.into_path().map(|path| json!(path)).map_err(Into::into))
        .unwrap_or(Ok(Value::Null))
}
fn selected_path(args: &Value) -> Result<PathBuf> {
    let path = PathBuf::from(
        args.get("path")
            .and_then(Value::as_str)
            .context("path_required")?,
    );
    if !path.is_absolute() {
        bail!("absolute_path_required");
    }
    Ok(path)
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("invalid_path")?;
    let temporary = parent.join(format!(".nna-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("file_write_failed")?;
    if file.write_all(bytes).and_then(|_| file.sync_all()).is_err() {
        let _ = fs::remove_file(&temporary);
        bail!("file_write_failed");
    }
    drop(file);
    if fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
        bail!("file_replace_failed");
    }
    Ok(())
}
#[cfg(windows)]
fn copy_clipboard_text(app: &tauri::AppHandle, text: &str) -> Result<()> {
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT},
        },
    };
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    struct Allocation(Option<HGLOBAL>);
    impl Drop for Allocation {
        fn drop(&mut self) {
            if let Some(memory) = self.0 {
                unsafe {
                    let _ = GlobalFree(Some(memory));
                }
            }
        }
    }
    // CF_UNICODETEXT is the standard Windows UTF-16 text clipboard format.
    const CF_UNICODETEXT: u32 = 13;
    let window = app
        .get_webview_window("main")
        .context("clipboard_unavailable")?;
    let owner = window
        .hwnd()
        .map_err(|_| anyhow::anyhow!("clipboard_unavailable"))?;
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        GlobalAlloc(
            GMEM_MOVEABLE | GMEM_ZEROINIT,
            text.len() * std::mem::size_of::<u16>(),
        )
    }
    .map_err(|_| anyhow::anyhow!("clipboard_unavailable"))?;
    let mut allocation = Allocation(Some(handle));
    let buffer = unsafe { GlobalLock(handle) };
    if buffer.is_null() {
        bail!("clipboard_unavailable");
    }
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer.cast::<u16>(), text.len());
        // A zero result also means the final lock was successfully released.
        let _ = GlobalUnlock(handle);
        OpenClipboard(Some(HWND(owner.0 as _)))
            .map_err(|_| anyhow::anyhow!("clipboard_unavailable"))?;
    }
    let clipboard = Clipboard;
    unsafe {
        EmptyClipboard().map_err(|_| anyhow::anyhow!("clipboard_unavailable"))?;
        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(handle.0)))
            .map_err(|_| anyhow::anyhow!("clipboard_unavailable"))?;
    }
    allocation.0 = None; // Windows owns the allocation after successful publication.
    unsafe { CloseClipboard() }.map_err(|_| anyhow::anyhow!("clipboard_unavailable"))?;
    std::mem::forget(clipboard);
    Ok(())
}
#[cfg(not(windows))]
fn copy_clipboard_text(_: &tauri::AppHandle, _: &str) -> Result<()> {
    bail!("clipboard_unavailable")
}

pub fn file_action(
    app: &tauri::AppHandle,
    catalogue: &Catalogue,
    action: &str,
    args: Value,
) -> Result<Value> {
    match action {
        "copy_text" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .context("text_required")?;
            if text.len() > 1024 * 1024 || text.contains('\0') {
                bail!("invalid_clipboard_text");
            }
            copy_clipboard_text(app, text)?;
            Ok(json!({"copied":true}))
        }
        "read_text" => {
            let path = selected_path(&args)?;
            if fs::metadata(&path).context("file_unavailable")?.len() > 1024 * 1024 {
                bail!("file_too_large");
            }
            let text = fs::read_to_string(path).context("invalid_text_file")?;
            Ok(json!({"text":text.strip_prefix('\u{feff}').unwrap_or(&text)}))
        }
        "write_text" => {
            let path = selected_path(&args)?;
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .context("text_required")?;
            if text.len() > 1024 * 1024 {
                bail!("file_too_large");
            }
            write_atomic(&path, text.as_bytes())?;
            Ok(json!({"path":path}))
        }
        "copy_image" => {
            let source = selected_path(&args)?;
            if fs::metadata(&source).context("file_unavailable")?.len() > 32 * 1024 * 1024 {
                bail!("image_too_large");
            }
            let bytes = fs::read(source).context("file_read_failed")?;
            let extension = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "png"
            } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                "jpg"
            } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
                "gif"
            } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
                "webp"
            } else {
                bail!("unsupported_image");
            };
            let hash = hex::encode(Sha256::digest(&bytes));
            let relative = PathBuf::from("media").join(format!("{}.{}", hash, extension));
            let path = catalogue.data_dir().join(&relative);
            fs::create_dir_all(path.parent().unwrap()).context("media_unavailable")?;
            if !path.exists() {
                write_atomic(&path, &bytes)?;
            }
            app.asset_protocol_scope().allow_file(&path)?;
            Ok(
                json!({"path":path,"relativePath":relative.to_string_lossy().replace('\\',"/"),"hash":hash}),
            )
        }
        _ => bail!("unknown_file_action"),
    }
}
/// Normalize a persistent public product page without storing login or download credentials.
pub(crate) fn normalize_public_url(raw: &str) -> Result<String> {
    if raw.is_empty()
        || raw.len() > 8192
        || raw.chars().any(char::is_control)
        || raw.trim() != raw
        || raw.contains('\\')
    {
        bail!("invalid_public_url");
    }
    let url = url::Url::parse(raw).context("invalid_public_url")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.cannot_be_a_base()
    {
        bail!("invalid_public_url");
    }
    // Ordinary public query parameters and anchors are valid for future shops.
    // Expiring signed downloads and session credentials cannot become product URLs.
    for (name, _) in url.query_pairs() {
        let key: String = name
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect();
        if [
            "token",
            "accesstoken",
            "refreshtoken",
            "session",
            "sessionid",
            "sessiontoken",
            "password",
            "authorization",
            "apikey",
            "secret",
            "signature",
            "sig",
            "policy",
            "keypairid",
        ]
        .contains(&key.as_str())
            || key.starts_with("xamz")
            || key.starts_with("xgoog")
        {
            bail!("invalid_public_url");
        }
    }
    Ok(url.into())
}

pub fn external_open(app: &tauri::AppHandle, raw: &str) -> Result<()> {
    let url = normalize_public_url(raw)?;
    app.opener()
        .open_url(url, None::<&str>)
        .context("external_open_failed")?;
    Ok(())
}

pub(crate) fn zoom_factor(scale: f64) -> Result<f64> {
    if !scale.is_finite() || !(80.0..=300.0).contains(&scale) {
        bail!("invalid_ui_scale");
    }
    Ok(scale / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_product_urls_support_other_shops_without_credentials() {
        for raw in [
            "https://booth.pm/ja/items/7329831",
            "https://creator.gumroad.com/l/asset?layout=profile#details",
            "http://localhost:8000/products/my-asset",
            "https://example.test/作品",
        ] {
            assert!(normalize_public_url(raw).is_ok(), "{raw}");
        }
        for raw in [
            "",
            " https://example.test/a",
            "https://example.test/a\n",
            "javascript:alert(1)",
            "file:///C:/asset.zip",
            "data:text/plain,x",
            "https://user:password@example.test/a",
            "https://example.test\\a",
            "https://example.test/a?token=x",
            "https://example.test/a?X-Amz-Signature=x",
            "https://example.test/a?x_goog_credential=x",
            "https://example.test/a?sessionId=x",
        ] {
            assert!(normalize_public_url(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn native_zoom_accepts_the_requested_range_only() {
        assert_eq!(zoom_factor(80.0).unwrap(), 0.8);
        assert_eq!(zoom_factor(100.0).unwrap(), 1.0);
        assert_eq!(zoom_factor(300.0).unwrap(), 3.0);
        for scale in [0.0, 79.0, 301.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(zoom_factor(scale).is_err());
        }
    }
}
