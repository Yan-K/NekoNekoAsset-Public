use anyhow::{bail, ensure, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        LazyLock, Mutex,
    },
};

pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    ensure_regular(path)?;
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), size))
}
pub fn key(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn readable_component(label: &str, identity: &str) -> String {
    let mut name = String::new();
    for ch in label.chars() {
        if name.encode_utf16().count() + ch.len_utf16() > 64 {
            break;
        }
        name.push(if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
            '_'
        } else {
            ch
        });
    }
    let name = name.trim().trim_end_matches(['.', ' ']);
    let mut component = format!(
        "{}--{}",
        if name.is_empty() { "Asset" } else { name },
        &key(identity)[..12]
    );
    if super::archive::validate_relative_path(&component).is_err() {
        component.insert(0, '_');
    }
    component
}
pub fn unused_relative(root: &Path, base: &str) -> Result<String> {
    for index in 0..10_000 {
        let relative = if index == 0 {
            base.to_owned()
        } else {
            format!("{base}-{index}")
        };
        let path = safe_join(root, &relative)?;
        if fs::symlink_metadata(&path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
            return Ok(relative);
        }
    }
    bail!("archive_path_collision")
}
/// This guard owns only the exact per-file temporary, never published content.
pub struct PartialGuard {
    path: PathBuf,
    parent_key: Option<PathBuf>,
}
static PARTIAL_PARENTS: LazyLock<Mutex<BTreeMap<PathBuf, usize>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
impl PartialGuard {
    pub fn new(path: PathBuf) -> Self {
        let parent_key = path.parent().and_then(|parent| {
            // Callers share the same checked root/path spelling. Keep the key
            // stable even when the parent has not been created yet.
            let key = parent.to_path_buf();
            let mut parents = PARTIAL_PARENTS.lock().ok()?;
            *parents.entry(key.clone()).or_default() += 1;
            Some(key)
        });
        Self { path, parent_key }
    }
}
pub struct EmptyDirectoryGuard(pub PathBuf);
impl Drop for EmptyDirectoryGuard {
    fn drop(&mut self) {
        let Ok(parents) = PARTIAL_PARENTS.lock() else {
            return;
        };
        if parents.contains_key(&self.0) {
            return;
        }
        if super::archive::validate_no_links(&self.0).is_ok() {
            let _ = fs::remove_dir(&self.0);
        }
    }
}
impl Drop for PartialGuard {
    fn drop(&mut self) {
        if super::archive::validate_no_links(&self.path).is_ok()
            && ensure_regular(&self.path).is_ok()
        {
            let _ = fs::remove_file(&self.path);
        }
        if let Some(key) = &self.parent_key {
            if let Ok(mut parents) = PARTIAL_PARENTS.lock() {
                let Some(count) = parents.get_mut(key) else {
                    return;
                };
                *count -= 1;
                if *count == 0 {
                    parents.remove(key);
                    if let Some(parent) = self.path.parent() {
                        if super::archive::validate_no_links(parent).is_ok() {
                            let _ = fs::remove_dir(parent);
                        }
                    }
                }
            }
        }
    }
}
/// Empty-only pruning stops before the checked managed boundary. A sibling,
/// reparse point or user-created file stops cleanup without touching its bytes.
pub fn remove_empty_parents(path: &Path, boundary: &Path) {
    if !path.is_absolute() || !boundary.is_absolute() || !path.starts_with(boundary) {
        return;
    }
    let Ok(relative) = path.strip_prefix(boundary) else {
        return;
    };
    if super::archive::validate_relative_path(&relative.to_string_lossy()).is_err() {
        return;
    }
    let mut current = path;
    while current != boundary && current.starts_with(boundary) {
        if super::archive::validate_no_links(current).is_err() || fs::remove_dir(current).is_err() {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }
}
pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "cancelled");
    Ok(())
}
pub async fn cancellable<T>(
    cancel: &AtomicBool,
    future: impl std::future::Future<Output = T>,
) -> Result<T> {
    check_cancel(cancel)?;
    tokio::select! {result=future=>Ok(result),_ = async {loop{tokio::time::sleep(std::time::Duration::from_millis(100)).await;if cancel.load(Ordering::Acquire){break}}}=>bail!("cancelled")}
}
pub async fn delay(cancel: &AtomicBool, duration: std::time::Duration) -> Result<()> {
    cancellable(cancel, tokio::time::sleep(duration)).await
}
pub fn ensure_regular(path: &Path) -> Result<()> {
    reject_link(path)?;
    ensure!(fs::metadata(path)?.is_file(), "file_missing");
    Ok(())
}
pub fn reject_link(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(!meta.file_type().is_symlink(), "unsafe_link");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(meta.file_attributes() & 0x400 == 0, "unsafe_link");
    }
    Ok(())
}
/// Check every existing ancestor, including Windows junctions, before accessing managed content.
pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(root.is_absolute(), "root_not_absolute");
    let relative = super::archive::validate_relative_path(relative)?;
    for p in root.ancestors() {
        if p.exists() {
            reject_link(p)?;
        }
    }
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        if fs::symlink_metadata(&path).is_ok() {
            reject_link(&path)?;
        }
    }
    Ok(path)
}
pub fn verify(path: &Path, hash: &str, size: u64) -> Result<()> {
    let actual = hash_file(path)?;
    ensure!(
        actual.0 == hash && actual.1 == size,
        "file_integrity_failed"
    );
    Ok(())
}
pub fn copy_new_verified(
    source: &Path,
    target: &Path,
    hash: &str,
    size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_regular(source)?;
    let mut input = File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    let copied = (|| -> Result<()> {
        let mut buffer = [0u8; 128 * 1024];
        loop {
            check_cancel(cancel)?;
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
        }
        output.sync_all()?;
        verify(target, hash, size)
    })();
    drop(output);
    if copied.is_err() {
        let _ = fs::remove_file(target);
    }
    copied
}
pub fn space(path: &Path, required: u64) -> Result<()> {
    ensure!(
        fs2::available_space(path)? >= required.saturating_add(16 * 1024 * 1024),
        "low_space"
    );
    Ok(())
}
pub fn absolute_dir(path: &Path, create: bool) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "root_not_absolute");
    for ancestor in path.ancestors() {
        if ancestor.exists() {
            reject_link(ancestor)?;
        }
    }
    if create {
        fs::create_dir_all(path)?;
    }
    ensure!(path.is_dir(), "root_offline");
    let path = path.canonicalize()?;
    let probe = path.join(format!(".nna-probe-{}", uuid::Uuid::new_v4()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    file.sync_all()?;
    drop(file);
    fs::remove_file(probe)?;
    Ok(path)
}
pub fn stable_error(err: &anyhow::Error) -> &'static str {
    let message = err.to_string().to_ascii_lowercase();
    match message.as_str() {
        "account_identity_mismatch" | "store_identity_changed" => return "auth_required",
        // The store no longer offers this file to the account; an update
        // replaces the saved list. Local bytes stay `file_missing`, and other
        // FILE_UNAVAILABLE callers outside downloads keep their own mapping.
        "file_unavailable" => return "store_file_unavailable",
        "source_tree_incomplete" => return "source_tree_incomplete",
        "network_unavailable" => return "network_failed",
        "download_failed" => return "download_http_failed",
        _ => {}
    }
    if message.contains("auth") || message.contains("account_unavailable") {
        return "auth_required";
    }
    if message.contains("archive_link_forbidden") || message.contains("storage_link_forbidden") {
        return "archive_unsafe_link";
    }
    for code in [
        "cancelled",
        "root_offline",
        "low_space",
        "auth_required",
        "permission_denied",
        "link_expired",
        "rate_limited",
        "network_failed",
        "network_timeout",
        "source_changed",
        "download_size_mismatch",
        "download_resolver_failed",
        "download_http_failed",
        "manifest_changed",
        "manifest_incomplete",
        "file_missing",
        "file_integrity_failed",
        "archive_password_required",
        "archive_missing_volume",
        "archive_corrupt",
        "archive_listing_invalid",
        "archive_inventory_mismatch",
        "archive_unsafe_path",
        "archive_unsafe_link",
        "archive_path_collision",
        "archive_tool_unavailable",
        "revision_unknown",
        "unsupported_backup_version",
        "backup_payload_missing",
        "destination_not_empty",
        "destination_not_fresh",
        "local_delete_unsafe_path",
        "local_delete_shared_path",
        "local_delete_cleanup_failed",
        "local_delete_rollback_failed",
        "local_delete_retry_required",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    if let Some(e) = err.downcast_ref::<std::io::Error>() {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            return "permission_denied";
        }
        if e.kind() == std::io::ErrorKind::StorageFull
            || matches!(e.raw_os_error(), Some(28 | 39 | 112))
        {
            return "low_space";
        }
    }
    "storage_failed"
}
pub fn open_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "root_not_absolute");
    ensure!(path.exists(), "file_missing");
    super::archive::validate_no_links(path)?;
    #[cfg(windows)]
    {
        // Filesystem verification keeps verbatim paths; the Shell receives a DOS/UNC path.
        let launch_path = windows_shell_path(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("shell_path_unsupported"))?,
        )?;
        windows_shell_open(launch_path)?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("--")
            .arg(path)
            .spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}
#[cfg(windows)]
fn windows_shell_open(path: String) -> Result<()> {
    // A fresh thread guarantees STA even if the blocking worker already uses MTA.
    std::thread::Builder::new()
        .name("nna-shell-open".into())
        .spawn(move || -> Result<()> {
            use windows::{
                core::{w, PCWSTR},
                Win32::{
                    System::Com::{
                        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
                        COINIT_DISABLE_OLE1DDE,
                    },
                    UI::{
                        Shell::{
                            ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
                            SHELLEXECUTEINFOW,
                        },
                        WindowsAndMessaging::SW_SHOWNORMAL,
                    },
                },
            };
            // Every successful initialization, including S_FALSE, must be balanced.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                .ok()
                .map_err(|e| anyhow::anyhow!("shell_com_initialization_failed: {}", e.code()))?;
            struct ComApartment;
            impl Drop for ComApartment {
                fn drop(&mut self) {
                    unsafe { CoUninitialize() };
                }
            }
            let _apartment = ComApartment;
            let target: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                // Complete dispatch before this STA exits; report errors through IPC.
                fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
                lpVerb: w!("open"),
                lpFile: PCWSTR(target.as_ptr()),
                lpParameters: PCWSTR::null(),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            // The target buffer lives through the synchronous call. No process handle
            // is requested, so only the COM initialization needs cleanup.
            unsafe { ShellExecuteExW(&mut info) }
                .map_err(|e| anyhow::anyhow!("shell_open_failed: {}", e.code()))?;
            Ok(())
        })?
        .join()
        .map_err(|_| anyhow::anyhow!("shell_open_failed"))?
}
#[cfg(any(windows, test))]
fn windows_shell_path(path: &str) -> Result<String> {
    ensure!(!path.contains('\0'), "shell_path_unsupported");
    ensure!(!path.starts_with("\\\\.\\"), "shell_path_unsupported");
    if path
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("\\\\?\\UNC\\"))
    {
        let suffix = &path[8..];
        let mut parts = suffix.split('\\');
        ensure!(
            parts.next().is_some_and(|s| !s.is_empty())
                && parts.next().is_some_and(|s| !s.is_empty()),
            "shell_path_unsupported"
        );
        return Ok(format!("\\\\{suffix}"));
    }
    if let Some(suffix) = path.strip_prefix("\\\\?\\") {
        let bytes = suffix.as_bytes();
        ensure!(
            bytes.first().is_some_and(u8::is_ascii_alphabetic)
                && bytes.get(1) == Some(&b':')
                && bytes.get(2) == Some(&b'\\'),
            "shell_path_unsupported"
        );
        return Ok(suffix.to_owned());
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod launch_path_tests {
    use super::*;
    #[test]
    fn completed_file_does_not_remove_a_waiting_transfers_shared_parent() {
        let root =
            std::env::temp_dir().join(format!("nna-partial-parent-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let parent = root.join(".nna-partials");
        fs::create_dir(&parent).unwrap();
        {
            let _job = EmptyDirectoryGuard(parent.clone());
            let first = parent.join("first.part");
            let second = parent.join("second.part");
            let pending = PartialGuard::new(second.clone());
            fs::write(&first, b"completed authored bytes").unwrap();
            drop(PartialGuard::new(first.clone()));
            assert!(!first.exists());
            assert!(parent.is_dir());
            fs::write(&second, b"later authored bytes").unwrap();
            drop(pending);
            assert!(!second.exists());
            assert!(!parent.exists());
        }
        assert!(!parent.exists());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn provider_identity_and_missing_file_errors_preserve_existing_action_codes() {
        for (error, expected) in [
            ("ACCOUNT_IDENTITY_MISMATCH", "auth_required"),
            ("STORE_IDENTITY_CHANGED", "auth_required"),
            ("FILE_UNAVAILABLE", "store_file_unavailable"),
            ("file_missing", "file_missing"),
            ("SIGNED_LINK_EXPIRED", "link_expired"),
            ("PERMISSION_DENIED", "permission_denied"),
            ("archive_corrupt", "archive_corrupt"),
            ("archive_listing_invalid", "archive_listing_invalid"),
            ("archive_inventory_mismatch", "archive_inventory_mismatch"),
            ("storage.destination_not_fresh", "destination_not_fresh"),
            ("NETWORK_UNAVAILABLE", "network_failed"),
            ("network_failed", "network_failed"),
            ("network_timeout", "network_timeout"),
            ("RATE_LIMITED", "rate_limited"),
            ("SOURCE_CHANGED", "source_changed"),
            ("download_size_mismatch", "download_size_mismatch"),
            ("DOWNLOAD_RESOLVER_FAILED", "download_resolver_failed"),
            ("DOWNLOAD_HTTP_FAILED", "download_http_failed"),
            ("download_failed", "download_http_failed"),
            ("manifest_changed", "manifest_changed"),
        ] {
            assert_eq!(stable_error(&anyhow::anyhow!(error)), expected);
        }
        assert_eq!(
            stable_error(&anyhow::anyhow!("UNRECOGNIZED_PROVIDER_FAILURE")),
            "storage_failed"
        );
    }

    #[test]
    fn windows_shell_paths_normalize_only_supported_verbatim_prefixes() {
        assert_eq!(
            windows_shell_path(r"\\?\E:\Synthetic Library\衣装").unwrap(),
            r"E:\Synthetic Library\衣装"
        );
        assert_eq!(
            windows_shell_path(r"\\?\UNC\synthetic-server\share\衣装").unwrap(),
            r"\\synthetic-server\share\衣装"
        );
        assert_eq!(
            windows_shell_path(r"E:\Synthetic Library").unwrap(),
            r"E:\Synthetic Library"
        );
        assert_eq!(
            windows_shell_path(r"\\synthetic-server\share\衣装").unwrap(),
            r"\\synthetic-server\share\衣装"
        );
        assert!(windows_shell_path(r"\\?\GLOBALROOT\Device\HarddiskVolume1").is_err());
        assert!(windows_shell_path(r"\\.\PhysicalDrive0").is_err());
        assert!(windows_shell_path("E:\\Synthetic\0Ignored").is_err());
    }
}
pub fn required_string<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    match value[field].as_str() {
        Some(s) if !s.is_empty() => Ok(s),
        _ => bail!("invalid_arguments"),
    }
}
