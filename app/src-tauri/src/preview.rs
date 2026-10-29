//! Device-only public raster preview cache. No cookies, catalogue records or URLs are persisted.
use anyhow::{anyhow, ensure, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, FileTimes},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, Semaphore},
};
use url::Url;

#[path = "path_safety.rs"]
pub(crate) mod path_safety;

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILES: usize = 2048;
const PREFIX: &str = "nna-preview-";
const EXTENSIONS: [&str; 4] = ["png", "jpg", "gif", "webp"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CacheUsage {
    bytes: u64,
    files: usize,
}
impl CacheUsage {
    fn needs_reservation(&self) -> bool {
        self.bytes > MAX_BYTES - 2 * MAX_FILE || self.files > MAX_FILES - 2
    }
    fn reserve(&mut self, root: &Path) -> Result<()> {
        if self.needs_reservation() {
            *self = trim(root, MAX_BYTES - 2 * MAX_FILE, MAX_FILES - 2, None)?;
        }
        Ok(())
    }
    fn published(&mut self, bytes: u64) {
        // This is an upper bound between scans. A rescan may already have
        // counted part of an active stream, or an existing matching destination.
        // Counting it again only causes earlier maintenance; it cannot consume
        // the space reserved for the other bounded stream.
        self.bytes = self.bytes.saturating_add(bytes);
        self.files = self.files.saturating_add(1);
    }
}

pub struct PreviewCache {
    owner: path_safety::PinnedRoot,
    client: reqwest::Client,
    slots: Semaphore,
    keys: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    disk: Arc<Mutex<Option<CacheUsage>>>,
}

impl PreviewCache {
    pub fn new(data_dir: PathBuf) -> Result<Self> {
        ensure!(data_dir.is_absolute(), "preview_path_refused");
        // Cache failures belong to preview requests, not application startup.
        let client = reqwest::Client::builder()
            .cookie_store(false)
            .referer(false)
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || permitted_url(attempt.url()).is_err() {
                    attempt.error("preview_url_refused")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| anyhow!("preview_client_failed"))?;
        Ok(Self {
            owner: path_safety::PinnedRoot::new(data_dir),
            client,
            slots: Semaphore::new(2),
            keys: Mutex::new(HashMap::new()),
            disk: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn get(&self, raw: String) -> Result<PathBuf> {
        let url = parse_public_url(&raw)?;
        let key = hex::encode(Sha256::digest(url.as_str().as_bytes()));
        let key_lock = {
            let mut keys = self
                .keys
                .lock()
                .map_err(|_| anyhow!("preview_cache_lock"))?;
            keys.retain(|_, entry| entry.strong_count() > 0);
            if let Some(lock) = keys.get(&key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(AsyncMutex::new(()));
                keys.insert(key.clone(), Arc::downgrade(&lock));
                lock
            }
        };
        let _key_guard = key_lock.lock().await;
        let root = self.owner.resolve("cache/previews").map_err(path_error)?;
        let lookup_root = root.clone();
        let lookup_key = key.clone();
        let disk = self.disk.clone();
        let cached = tokio::task::spawn_blocking(move || {
            let root = lookup_root;
            let mut usage = disk.lock().map_err(|_| anyhow!("preview_cache_lock"))?;
            if usage.is_none() {
                no_links(&root)?;
                fs::create_dir_all(&root).context("preview_cache_io")?;
                no_links(&root)?;
                // Recent partials may still belong to another request. Only
                // hour-old abandoned partials are eligible for maintenance.
                *usage = Some(trim(&root, MAX_BYTES, MAX_FILES, None)?);
            }
            find_cached(&root, &lookup_key)
        })
        .await
        .map_err(|_| anyhow!("preview_cache_worker"))??;
        if let Some(path) = cached {
            return Ok(path);
        }

        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| anyhow!("preview_cache_closed"))?;
        let reservation_root = root.clone();
        let disk = self.disk.clone();
        tokio::task::spawn_blocking(move || {
            let root = reservation_root;
            let mut usage = disk.lock().map_err(|_| anyhow!("preview_cache_lock"))?;
            // Reserve room for both bounded streams, including their partials.
            usage
                .as_mut()
                .ok_or_else(|| anyhow!("preview_cache_lock"))?
                .reserve(&root)
        })
        .await
        .map_err(|_| anyhow!("preview_cache_worker"))??;
        let temporary = root.join(format!("{PREFIX}{key}-{}.part", uuid::Uuid::new_v4()));
        let mut guard = PartialGuard(PathBuf::new());
        let (extension, bytes) = tokio::time::timeout(
            Duration::from_secs(30),
            self.fetch(&root, url, &temporary, &mut guard),
        )
        .await
        .map_err(|_| anyhow!("preview_timeout"))??;
        let disk = self.disk.clone();
        let temp = temporary.clone();
        let path = tokio::task::spawn_blocking(move || {
            let mut usage = disk.lock().map_err(|_| anyhow!("preview_cache_lock"))?;
            let path = publish(&root, &temp, &key, extension)?;
            // The pre-fetch reservation covers both bounded concurrent streams;
            // publishing their bytes cannot exceed it, so no second scan is needed.
            usage
                .as_mut()
                .ok_or_else(|| anyhow!("preview_cache_lock"))?
                .published(bytes);
            Ok::<_, anyhow::Error>(path)
        })
        .await
        .map_err(|_| anyhow!("preview_cache_worker"))??;
        guard.0 = PathBuf::new();
        Ok(path)
    }

    async fn fetch(
        &self,
        root: &Path,
        url: Url,
        temporary: &Path,
        guard: &mut PartialGuard,
    ) -> Result<(&'static str, u64)> {
        no_links(root)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| anyhow!("preview_request_failed"))?;
        permitted_url(response.url())?;
        ensure!(response.status().is_success(), "preview_request_failed");
        if let Some(size) = response.content_length() {
            ensure!(size <= MAX_FILE, "preview_too_large");
        }
        let mut output = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary)
            .await
            .context("preview_cache_io")?;
        guard.0 = temporary.to_owned();
        let mut total = 0u64;
        let mut header = Vec::with_capacity(16);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|_| anyhow!("preview_request_failed"))?;
            total = bounded_size(total, bytes.len())?;
            header.extend_from_slice(&bytes[..bytes.len().min(16 - header.len())]);
            output.write_all(&bytes).await.context("preview_cache_io")?;
        }
        let extension =
            raster_extension(&header).ok_or_else(|| anyhow!("preview_raster_required"))?;
        ensure!(total > 0, "preview_raster_required");
        output.flush().await.context("preview_cache_io")?;
        output.sync_all().await.context("preview_cache_io")?;
        drop(output); // Windows rename must happen after the writer is closed.
        Ok((extension, total))
    }
}

pub(crate) fn parse_public_url(raw: &str) -> Result<Url> {
    ensure!(
        raw.len() <= 8192 && raw.trim() == raw && !raw.contains('\\'),
        "preview_url_refused"
    );
    let (_, rest) = raw
        .split_once("://")
        .ok_or_else(|| anyhow!("preview_url_refused"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Url normalizes an explicit :443 away; reject it before parsing as well.
    ensure!(!authority.contains([':', '@']), "preview_url_refused");
    let url = Url::parse(raw).map_err(|_| anyhow!("preview_url_refused"))?;
    permitted_url(&url)?;
    Ok(url)
}

fn permitted_url(url: &Url) -> Result<()> {
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "preview_url_refused"
    );
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("preview_url_refused"))?;
    ensure!(
        (host.ends_with(".pximg.net")
            || matches!(host, "public-files.gumroad.com" | "jinxxy-c.jinxxy-cdn.com"))
            && host.len() <= 253,
        "preview_url_refused"
    );
    ensure!(
        host.split('.').all(|label| !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')),
        "preview_url_refused"
    );
    Ok(())
}

fn bounded_size(current: u64, incoming: usize) -> Result<u64> {
    let next = current
        .checked_add(incoming as u64)
        .ok_or_else(|| anyhow!("preview_too_large"))?;
    ensure!(next <= MAX_FILE, "preview_too_large");
    Ok(next)
}

fn raster_extension(header: &[u8]) -> Option<&'static str> {
    if header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if header.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        Some("gif")
    } else if header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

pub(crate) fn no_links(path: &Path) -> Result<()> {
    path_safety::no_links(path).map_err(path_error)
}

fn path_error(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<std::io::Error>().is_some() {
        anyhow!("preview_cache_io")
    } else {
        anyhow!("preview_path_refused")
    }
}

fn managed_name(name: &str) -> bool {
    let Some(tail) = name.strip_prefix(PREFIX) else {
        return false;
    };
    let Some((hash, extension)) = tail.rsplit_once('.') else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && EXTENSIONS.contains(&extension)
}

fn partial_name(name: &str) -> bool {
    let Some(tail) = name
        .strip_prefix(PREFIX)
        .and_then(|s| s.strip_suffix(".part"))
    else {
        return false;
    };
    if tail.len() != 101 || tail.as_bytes()[64] != b'-' {
        return false;
    }
    tail[..64]
        .bytes()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && uuid::Uuid::parse_str(&tail[65..]).is_ok()
}

fn valid_cached(path: &Path, extension: &str) -> Result<bool> {
    no_links(path)?;
    let metadata = fs::symlink_metadata(path).context("preview_cache_io")?;
    if !metadata.is_file() || metadata.len() > MAX_FILE {
        return Ok(false);
    }
    let mut header = [0u8; 16];
    let count = File::open(path)
        .context("preview_cache_io")?
        .read(&mut header)
        .context("preview_cache_io")?;
    Ok(raster_extension(&header[..count]) == Some(extension))
}

fn find_cached(root: &Path, key: &str) -> Result<Option<PathBuf>> {
    no_links(root)?;
    for extension in EXTENSIONS {
        let path = root.join(format!("{PREFIX}{key}.{extension}"));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(anyhow!("preview_cache_io")),
            Ok(_) => (),
        }
        if valid_cached(&path, extension)? {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .context("preview_cache_io")?;
            // Approximate LRU with at most one timestamp write per minute per file.
            if file
                .metadata()
                .context("preview_cache_io")?
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age >= Duration::from_secs(60))
            {
                file.set_times(FileTimes::new().set_modified(SystemTime::now()))
                    .context("preview_cache_io")?;
            }
            return Ok(Some(path));
        }
        if fs::symlink_metadata(&path)
            .context("preview_cache_io")?
            .is_file()
        {
            fs::remove_file(path).context("preview_cache_io")?;
        }
    }
    Ok(None)
}

fn publish(root: &Path, temporary: &Path, key: &str, extension: &str) -> Result<PathBuf> {
    ensure!(
        temporary.parent() == Some(root)
            && temporary
                .file_name()
                .and_then(|v| v.to_str())
                .is_some_and(partial_name),
        "preview_path_refused"
    );
    let destination = root.join(format!("{PREFIX}{key}.{extension}"));
    ensure!(
        destination
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(managed_name),
        "preview_path_refused"
    );
    no_links(temporary)?;
    no_links(&destination)?;
    if destination.exists() {
        ensure!(
            valid_cached(&destination, extension)?,
            "preview_cache_collision"
        );
        fs::remove_file(temporary).context("preview_cache_io")?;
    } else {
        fs::rename(temporary, &destination).context("preview_cache_io")?;
    }
    Ok(destination)
}

fn trim(
    root: &Path,
    budget: u64,
    count_limit: usize,
    protected: Option<&Path>,
) -> Result<CacheUsage> {
    no_links(root)?;
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut partials = 0usize;
    for entry in fs::read_dir(root).context("preview_cache_io")? {
        let entry = entry.context("preview_cache_io")?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !managed_name(&name) && !partial_name(&name) {
            continue;
        }
        // DirEntry metadata does not follow links and reuses the Windows
        // directory enumeration data instead of another stat for each file.
        let metadata = entry.metadata().context("preview_cache_io")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                continue;
            }
        }
        if partial_name(&name) {
            if metadata
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= Duration::from_secs(3600))
                && fs::remove_file(&path).is_ok()
            {
                continue;
            } else {
                total = total.saturating_add(metadata.len());
                partials += 1;
            }
            continue;
        }
        total = total.saturating_add(metadata.len());
        files.push((
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path,
            metadata.len(),
        ));
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut count = files.len() + partials;
    for (_, path, size) in files {
        if total <= budget && count <= count_limit {
            break;
        }
        if protected == Some(path.as_path()) {
            continue;
        }
        no_links(&path)?;
        if fs::remove_file(path).is_err() {
            continue;
        }
        total = total.saturating_sub(size);
        count -= 1;
    }
    ensure!(
        total <= budget && count <= count_limit,
        "preview_cache_full"
    );
    Ok(CacheUsage {
        bytes: total,
        files: count,
    })
}

struct PartialGuard(PathBuf);
impl Drop for PartialGuard {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("nna-preview-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn hash(n: u8) -> String {
        format!("{n:064x}")
    }
    fn png() -> &'static [u8] {
        b"\x89PNG\r\n\x1a\nsynthetic signature fixture"
    }
    fn cache_file(root: &Path, n: u8) -> PathBuf {
        let path = root.join(format!("{PREFIX}{}.png", hash(n)));
        fs::write(&path, png()).unwrap();
        path
    }

    #[test]
    fn quota_reserves_two_maximum_streams_before_another_fetch() {
        let mut usage = CacheUsage {
            bytes: MAX_BYTES - 2 * MAX_FILE,
            files: MAX_FILES - 2,
        };
        assert!(!usage.needs_reservation());
        let mut over_bytes = usage;
        over_bytes.bytes += 1;
        assert!(over_bytes.needs_reservation());
        let mut over_files = usage;
        over_files.files += 1;
        assert!(over_files.needs_reservation());
        // Both slots may finish before the next reservation. Their maximum
        // combined payload still fits the bytes and entry-count bounds.
        usage.published(MAX_FILE);
        usage.published(MAX_FILE);
        assert_eq!(
            usage,
            CacheUsage {
                bytes: MAX_BYTES,
                files: MAX_FILES
            }
        );
        assert!(usage.needs_reservation());
    }

    #[test]
    fn quota_keeps_safe_overcounts_until_maintenance_and_reclaims_stale_partials() {
        let fixture = Fixture::new();
        cache_file(&fixture.0, 1);
        let corrupt = cache_file(&fixture.0, 2);
        fs::write(&corrupt, b"authored invalid raster").unwrap();
        let mut usage = trim(&fixture.0, MAX_BYTES, MAX_FILES, None).unwrap();
        assert_eq!(usage.files, 2);
        assert!(find_cached(&fixture.0, &hash(2)).unwrap().is_none());
        let before = usage;
        usage.reserve(&fixture.0).unwrap();
        // Ordinary misses do not scan the directory: an already removed
        // corrupt entry remains an upper-bound count until quota maintenance.
        assert_eq!(usage, before);

        let key = hash(3);
        let partial = fixture
            .0
            .join(format!("{PREFIX}{key}-{}.part", uuid::Uuid::new_v4()));
        fs::write(&partial, png()).unwrap();
        usage = trim(&fixture.0, MAX_BYTES, MAX_FILES, None).unwrap();
        assert_eq!(usage.files, 2); // Includes the active partial.
        let destination = publish(&fixture.0, &partial, &key, "png").unwrap();
        usage.published(png().len() as u64);
        assert_eq!(usage.files, 3);
        // A matching cached destination is harmless to count again as well.
        fs::write(&partial, png()).unwrap();
        assert_eq!(
            publish(&fixture.0, &partial, &key, "png").unwrap(),
            destination
        );
        usage.published(png().len() as u64);
        assert_eq!(usage.files, 4);

        let abandoned =
            fixture
                .0
                .join(format!("{PREFIX}{}-{}.part", hash(4), uuid::Uuid::new_v4()));
        fs::write(&abandoned, png()).unwrap();
        File::options()
            .write(true)
            .open(&abandoned)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        usage.bytes = MAX_BYTES;
        usage.reserve(&fixture.0).unwrap();
        assert_eq!(
            usage,
            CacheUsage {
                bytes: 2 * png().len() as u64,
                files: 2
            }
        );
        assert!(!abandoned.exists());
        assert!(destination.exists());
    }

    #[tokio::test]
    async fn optional_cache_initializes_on_demand_and_retries_before_network() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("cache");
        fs::write(&parent, b"authored path blocker").unwrap();
        let cache = PreviewCache::new(fixture.0.clone()).unwrap();
        let url = "https://booth.pximg.net/authored-preview.png";
        assert!(cache.get(url.into()).await.is_err());
        assert_eq!(fs::read(&parent).unwrap(), b"authored path blocker");
        fs::remove_file(&parent).unwrap();
        let root = parent.join("previews");
        fs::create_dir_all(&root).unwrap();
        let key = hex::encode(Sha256::digest(url.as_bytes()));
        let image = root.join(format!("{PREFIX}{key}.png"));
        fs::write(&image, png()).unwrap();
        let cached = cache.get(url.into()).await.unwrap();
        assert_eq!(cached, image.canonicalize().unwrap());
        assert_eq!(fs::read(cached).unwrap(), png());
    }

    #[cfg(windows)]
    #[test]
    fn eviction_skips_locked_files_when_other_entries_can_meet_the_budget() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let locked = cache_file(&fixture.0, 1);
        File::options()
            .write(true)
            .open(&locked)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        // Permit reads/metadata, but keep Windows deletion sharing disabled.
        let _handle = File::options()
            .read(true)
            .share_mode(3)
            .open(&locked)
            .unwrap();
        let removable = cache_file(&fixture.0, 2);
        trim(&fixture.0, MAX_BYTES, 1, None).unwrap();
        assert!(locked.exists());
        assert!(!removable.exists());
    }

    #[test]
    fn public_url_boundary_rejects_credentials_ports_queries_and_lookalikes() {
        for raw in [
            "https://booth.pximg.net/a.png",
            "https://i.pximg.net/a/b.jpg",
            "https://public-files.gumroad.com/synthetic-preview",
            "https://jinxxy-c.jinxxy-cdn.com/cdn-cgi/image/format=auto,onerror=redirect/synthetic/thumbnail.webp",
        ] {
            assert!(parse_public_url(raw).is_ok());
        }
        for raw in [
            "http://booth.pximg.net/a",
            "https://booth.pximg.net:443/a",
            "https://booth.pximg.net:444/a",
            "https://u:p@booth.pximg.net/a",
            "https://@booth.pximg.net/a",
            "https://booth.pximg.net/a?token=x",
            "https://booth.pximg.net/a#x",
            "https://booth.pximg.net.attacker.test/a",
            "https://pximg.net/a",
            "https://bad_pximg.pximg.net/a",
            "https://-bad.pximg.net/a",
            "https://booth.pximg.net./a",
            "https://127.0.0.1/a",
            " https://booth.pximg.net/a",
            "https://booth.pximg.net\\a",
            "http://public-files.gumroad.com/a",
            "https://public-files.gumroad.com:443/a",
            "https://u:p@public-files.gumroad.com/a",
            "https://public-files.gumroad.com/a?Signature=synthetic",
            "https://public-files.gumroad.com/a#fragment",
            "https://public-files.gumroad.com.attacker.test/a",
            "https://other.gumroad.com/a",
            "https://jinxxy-c.jinxxy-cdn.com.attacker.test/a",
            "https://other.jinxxy-cdn.com/a",
            "https://jinxxy-c.jinxxy-cdn.com/a?token=synthetic",
            "https://jinxxy-c.jinxxy-cdn.com:443/a",
        ] {
            assert!(parse_public_url(raw).is_err(), "{raw}");
        }
    }
    #[test]
    fn raster_signatures_and_stream_bounds_are_exact() {
        for (header, extension) in [
            (png(), "png"),
            (&b"\xff\xd8\xffdata"[..], "jpg"),
            (&b"GIF89a"[..], "gif"),
            (&b"RIFF1234WEBP"[..], "webp"),
        ] {
            assert_eq!(raster_extension(header), Some(extension));
        }
        assert_eq!(raster_extension(b"<svg>"), None);
        assert_eq!(raster_extension(b"RIFF1234WAVE"), None);
        assert_eq!(bounded_size(MAX_FILE - 1, 1).unwrap(), MAX_FILE);
        assert!(bounded_size(MAX_FILE, 1).is_err());
        assert!(bounded_size(u64::MAX, 1).is_err());
    }
    #[test]
    fn closed_temporary_publishes_once_and_roundtrips_cache_hit() {
        let fixture = Fixture::new();
        let key = hash(1);
        let temp = fixture
            .0
            .join(format!("{PREFIX}{key}-{}.part", uuid::Uuid::new_v4()));
        {
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .unwrap();
            use std::io::Write;
            (&file).write_all(png()).unwrap();
            file.sync_all().unwrap();
        }
        let published = publish(&fixture.0, &temp, &key, "png").unwrap();
        assert!(!temp.exists());
        assert_eq!(
            find_cached(&fixture.0, &key).unwrap(),
            Some(published.clone())
        );
        fs::write(&temp, png()).unwrap();
        assert_eq!(publish(&fixture.0, &temp, &key, "png").unwrap(), published);
        let unrelated = fixture.0.join("human-image.png");
        fs::write(&unrelated, png()).unwrap();
        assert!(publish(&fixture.0, &unrelated, &key, "png").is_err());
        assert!(unrelated.exists());
    }
    #[test]
    fn eviction_only_removes_managed_files_and_preserves_recent_partial() {
        let fixture = Fixture::new();
        let first = cache_file(&fixture.0, 1);
        let second = cache_file(&fixture.0, 2);
        File::options()
            .write(true)
            .open(&first)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        let unrelated = fixture.0.join("user-thumbnail.png");
        fs::write(&unrelated, png()).unwrap();
        let partial = fixture
            .0
            .join(format!("{PREFIX}{}-{}.part", hash(3), uuid::Uuid::new_v4()));
        fs::write(&partial, png()).unwrap();
        trim(&fixture.0, MAX_BYTES, 2, Some(&second)).unwrap();
        assert!(!first.exists());
        assert!(second.exists());
        assert!(unrelated.exists());
        assert!(partial.exists());
        assert!(trim(&fixture.0, 0, 0, None).is_err());
        assert!(!second.exists());
        assert!(unrelated.exists());
        File::options()
            .write(true)
            .open(&partial)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        trim(&fixture.0, 0, 0, None).unwrap();
        assert!(!partial.exists());
    }
    #[test]
    fn invalid_cached_content_is_removed_without_touching_other_names() {
        let fixture = Fixture::new();
        let path = cache_file(&fixture.0, 1);
        fs::write(&path, b"<html>not a raster</html>").unwrap();
        assert!(find_cached(&fixture.0, &hash(1)).unwrap().is_none());
        assert!(!path.exists());
        assert!(!managed_name("nna-preview-../media.png"));
        assert!(!partial_name("nna-preview-short.part"));
    }
    #[test]
    fn service_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<PreviewCache>();
    }
}
