//! Exact ownership of future, uncommitted Setup login profiles only.
use crate::stores::{session, StoreProvider};
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, sync::Mutex};

const MARKERS: &str = "setup-auth";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker { version: u32, provider: StoreProvider, account: String, pending: bool }

pub(crate) struct SetupSessions {
    device: PathBuf,
    leases: Mutex<BTreeMap<String, File>>,
}
impl SetupSessions {
    pub(crate) fn new(device: &Path) -> Result<Self> {
        let device = device.canonicalize().context("SESSION_CACHE_UNAVAILABLE")?;
        ensure!(device.is_dir(), "SESSION_CACHE_UNAVAILABLE");
        Ok(Self { device, leases: Mutex::new(BTreeMap::new()) })
    }
    pub(crate) fn arm(&self, provider: StoreProvider, account: &str) -> Result<()> {
        let marker = marker(provider, account, true)?;
        let name = marker_name(&marker);
        let mut leases = self.leases.lock().map_err(|_| anyhow::anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
        if leases.contains_key(&name) { return Ok(()) }
        let profile = session::profile_path(&self.device, provider, account)?;
        checked_owned_path(&self.device, &profile)?;
        // Never claim a profile from an older Setup, another library or a UUID
        // collision. This marker must precede the first native profile write.
        ensure!(fs::symlink_metadata(&profile).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound), "SESSION_CACHE_UNAVAILABLE");
        let directory = self.device.join(MARKERS);
        checked_owned_path(&self.device, &directory)?;
        fs::create_dir_all(&directory).context("SESSION_CACHE_UNAVAILABLE")?;
        checked_owned_path(&self.device, &directory)?;
        let path = directory.join(&name);
        let mut file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).context("SESSION_CACHE_UNAVAILABLE")?;
        FileExt::try_lock_exclusive(&file).context("SESSION_CACHE_UNAVAILABLE")?;
        write_marker(&mut file, &marker)?;
        sync_directory(&directory)?;
        leases.insert(name, file);
        Ok(())
    }
    pub(crate) fn release(&self, accounts: &[Value]) -> Result<()> {
        self.set_pending(accounts, false)
    }
    pub(crate) fn rearm(&self, accounts: &[Value]) -> Result<()> {
        self.set_pending(accounts, true)
    }
    fn set_pending(&self, accounts: &[Value], pending: bool) -> Result<()> {
        let mut leases = self.leases.lock().map_err(|_| anyhow::anyhow!("SESSION_CACHE_UNAVAILABLE"))?;
        for account in accounts {
            let Some((provider, id)) = account_identity(account) else { continue };
            let marker = marker(provider, id, pending)?;
            if let Some(file) = leases.get_mut(&marker_name(&marker)) {
                // Flush the released state before any account can reach SQL.
                // A partial/uncertain write is never cleanup authority.
                write_marker(file, &marker)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn schedule_cleanup(device: &Path) {
    // Exact-marker maintenance is optional and never blocks native startup.
    // It runs for ready libraries too, without inspecting catalogue membership.
    let Ok(device) = device.canonicalize() else { return };
    let _ = std::thread::Builder::new().name("nna-setup-auth-cleanup".into())
        .spawn(move || { let _ = cleanup_abandoned(&device); });
}

pub(crate) fn account_identity(account: &Value) -> Option<(StoreProvider, &str)> {
    Some((StoreProvider::parse(account["store"].as_str()?).ok()?, account["id"].as_str()?))
}
fn marker(provider: StoreProvider, account: &str, pending: bool) -> Result<Marker> {
    let canonical = uuid::Uuid::parse_str(account).context("INVALID_ACCOUNT_ID")?.to_string();
    ensure!(account == canonical, "INVALID_ACCOUNT_ID");
    Ok(Marker { version: 1, provider, account: canonical, pending })
}
fn marker_name(marker: &Marker) -> String { format!("{}-{}.json", marker.provider.key(), marker.account) }
fn write_marker(file: &mut File, marker: &Marker) -> Result<()> {
    let bytes = serde_json::to_vec(marker).context("SESSION_CACHE_UNAVAILABLE")?;
    file.seek(SeekFrom::Start(0)).context("SESSION_CACHE_UNAVAILABLE")?;
    file.set_len(0).context("SESSION_CACHE_UNAVAILABLE")?;
    file.write_all(&bytes).context("SESSION_CACHE_UNAVAILABLE")?;
    file.sync_all().context("SESSION_CACHE_UNAVAILABLE")
}
fn is_link(metadata: &fs::Metadata) -> bool {
    let link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    { use std::os::windows::fs::MetadataExt; link || metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))]
    { link }
}
fn checked_owned_path(device: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(device).context("SESSION_CACHE_UNAVAILABLE")?;
    let mut current = device.to_path_buf();
    for component in relative.components() {
        ensure!(matches!(component, std::path::Component::Normal(_)), "SESSION_CACHE_UNAVAILABLE");
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(!is_link(&metadata), "SESSION_CACHE_UNAVAILABLE"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error).context("SESSION_CACHE_UNAVAILABLE"),
        }
    }
    Ok(())
}
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn cleanup_abandoned(device: &Path) -> Result<usize> {
    let directory = device.join(MARKERS);
    checked_owned_path(device, &directory)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("SESSION_CACHE_UNAVAILABLE"),
    };
    let mut removed = 0;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let clean = (|| -> Result<bool> {
            checked_owned_path(device, &path)?;
            ensure!(fs::symlink_metadata(&path)?.is_file(), "SESSION_CACHE_UNAVAILABLE");
            let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
            FileExt::try_lock_exclusive(&file)?; // A live Setup still owns this profile.
            let mut bytes = Vec::new();
            (&mut file).take(1025).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1024, "SESSION_CACHE_UNAVAILABLE");
            let stored: Marker = serde_json::from_slice(&bytes)?;
            ensure!(stored.version == 1 && stored.account == uuid::Uuid::parse_str(&stored.account)?.to_string()
                && entry.file_name() == marker_name(&stored).as_str(), "SESSION_CACHE_UNAVAILABLE");
            if stored.pending {
                let profile = session::profile_path(device, stored.provider, &stored.account)?;
                remove_owned_profile(device, &profile)?;
            }
            drop(file);
            fs::remove_file(&path)?;
            Ok(stored.pending)
        })();
        if matches!(clean, Ok(true)) { removed += 1; }
    }
    Ok(removed)
}

fn remove_owned_profile(device: &Path, profile: &Path) -> Result<()> {
    checked_owned_path(device, profile)?;
    if !profile.try_exists()? { return Ok(()) }
    let mut pending = vec![profile.to_path_buf()];
    let mut files = Vec::new();
    let mut directories = Vec::new();
    // Validate the complete exact account tree before deleting any of it.
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(!is_link(&metadata), "SESSION_CACHE_UNAVAILABLE");
        if metadata.is_dir() {
            for child in fs::read_dir(&path)? { pending.push(child?.path()); }
            directories.push(path);
        } else {
            ensure!(metadata.is_file(), "SESSION_CACHE_UNAVAILABLE");
            files.push(path);
        }
    }
    for path in files { checked_owned_path(device, &path)?; fs::remove_file(path)?; }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in directories { checked_owned_path(device, &path)?; fs::remove_dir(path)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("nna-setup-auth-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn profile(&self, provider: StoreProvider, id: &str) -> PathBuf {
            let path = session::profile_path(&self.0, provider, id).unwrap();
            fs::create_dir_all(path.join("WebView/Cache")).unwrap();
            fs::write(path.join("session.bin"), b"authored protected fixture").unwrap();
            fs::write(path.join("WebView/Cache/authored"), b"authored browser fixture").unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = remove_owned_profile(&self.0, &self.0); }
    }
    #[test]
    fn cleanup_requires_exact_pending_marker_and_released_lifetime_lease() {
        let fixture = Fixture::new();
        let sessions = SetupSessions::new(&fixture.0).unwrap();
        assert!(!fixture.0.join(MARKERS).exists());
        let id = uuid::Uuid::new_v4().to_string();
        sessions.arm(StoreProvider::Gumroad, &id).unwrap();
        let pending = fixture.profile(StoreProvider::Gumroad, &id);
        let old_id = uuid::Uuid::new_v4().to_string();
        let unmarked = fixture.profile(StoreProvider::Gumroad, &old_id);
        fs::write(fixture.0.join(MARKERS).join("malformed.json"), b"invalid marker").unwrap();
        assert_eq!(cleanup_abandoned(&fixture.0).unwrap(), 0);
        assert!(pending.join("session.bin").exists());
        drop(sessions);
        assert_eq!(cleanup_abandoned(&fixture.0).unwrap(), 1);
        assert!(!pending.exists());
        assert_eq!(fs::read(unmarked.join("session.bin")).unwrap(), b"authored protected fixture");
        assert!(fixture.0.join(MARKERS).join("malformed.json").exists());
    }
    #[test]
    fn released_marker_and_repeated_login_never_authorize_committed_profile_cleanup() {
        let fixture = Fixture::new();
        let sessions = SetupSessions::new(&fixture.0).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        sessions.arm(StoreProvider::Booth, &id).unwrap();
        let profile = fixture.profile(StoreProvider::Booth, &id);
        sessions.release(&[json!({"store":"BOOTH","id":id})]).unwrap();
        sessions.arm(StoreProvider::Booth, &id).unwrap();
        drop(sessions);
        assert_eq!(cleanup_abandoned(&fixture.0).unwrap(), 0);
        assert_eq!(fs::read(profile.join("session.bin")).unwrap(), b"authored protected fixture");
        let next = SetupSessions::new(&fixture.0).unwrap();
        assert!(next.arm(StoreProvider::Booth, &id).is_err());
        assert!(profile.join("WebView/Cache/authored").exists());
    }
    #[test]
    #[cfg(windows)]
    fn abandoned_profile_with_a_junction_is_retained_without_touching_the_target() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new();
        let sessions = SetupSessions::new(&fixture.0).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        sessions.arm(StoreProvider::Jinxxy, &id).unwrap();
        let profile = fixture.profile(StoreProvider::Jinxxy, &id);
        let outside = fixture.0.join("unrelated");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"unrelated authored bytes").unwrap();
        let junction = profile.join("linked");
        let status = std::process::Command::new("cmd").args(["/C", "mklink", "/J"])
            .arg(&junction).arg(&outside).creation_flags(0x08000000).output().unwrap();
        assert!(status.status.success(), "authored junction fixture must be available");
        drop(sessions);
        let cleaned = cleanup_abandoned(&fixture.0).unwrap();
        let profile_retained = profile.join("session.bin").exists();
        let outside_bytes = fs::read(outside.join("keep")).unwrap();
        fs::remove_dir(&junction).unwrap();
        assert_eq!(cleaned, 0);
        assert!(profile_retained);
        assert_eq!(outside_bytes, b"unrelated authored bytes");
    }
}
