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

const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILES: usize = 2048;
const PREFIX: &str = "nna-preview-";
const EXTENSIONS: [&str; 4] = ["png", "jpg", "gif", "webp"];

pub struct PreviewCache {
    root: PathBuf,
    client: reqwest::Client,
    slots: Semaphore,
    keys: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    disk: Arc<Mutex<()>>,
}

impl PreviewCache {
    pub fn new(data_dir: PathBuf) -> Result<Self> {
        ensure!(data_dir.is_absolute(), "preview_path_refused");
        let root = data_dir.join("cache").join("previews");
        no_links(&root)?;
        fs::create_dir_all(&root).context("preview_cache_io")?;
        no_links(&root)?;
        // Startup is local maintenance only. Recent partials may still belong to a
        // request in another cache instance; only hour-old abandoned partials go.
        trim(&root, MAX_BYTES, MAX_FILES, None)?;
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
            root,
            client,
            slots: Semaphore::new(2),
            keys: Mutex::new(HashMap::new()),
            disk: Arc::new(Mutex::new(())),
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
        let root = self.root.clone();
        let lookup_key = key.clone();
        let disk = self.disk.clone();
        let cached = tokio::task::spawn_blocking(move || {
            let _guard = disk.lock().map_err(|_| anyhow!("preview_cache_lock"))?;
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
        let root = self.root.clone();
        let disk = self.disk.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = disk.lock().map_err(|_| anyhow!("preview_cache_lock"))?;
            // Reserve room for both bounded streams, including their partials.
            trim(&root, MAX_BYTES - 2 * MAX_FILE, MAX_FILES - 2, None)
        })
        .await
        .map_err(|_| anyhow!("preview_cache_worker"))??;
        let temporary = self
            .root
            .join(format!("{PREFIX}{key}-{}.part", uuid::Uuid::new_v4()));
        let mut guard = PartialGuard(PathBuf::new());
        let extension = tokio::time::timeout(
            Duration::from_secs(30),
            self.fetch(url, &temporary, &mut guard),
        )
        .await
        .map_err(|_| anyhow!("preview_timeout"))??;
        let root = self.root.clone();
        let disk = self.disk.clone();
        let temp = temporary.clone();
        let path = tokio::task::spawn_blocking(move || {
            let _guard = disk.lock().map_err(|_| anyhow!("preview_cache_lock"))?;
            let path = publish(&root, &temp, &key, extension)?;
            trim(&root, MAX_BYTES, MAX_FILES, Some(&path))?;
            Ok::<_, anyhow::Error>(path)
        })
        .await
        .map_err(|_| anyhow!("preview_cache_worker"))??;
        guard.0 = PathBuf::new();
        Ok(path)
    }

    async fn fetch(
        &self,
        url: Url,
        temporary: &Path,
        guard: &mut PartialGuard,
    ) -> Result<&'static str> {
        no_links(&self.root)?;
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
        Ok(extension)
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
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                ensure!(!metadata.file_type().is_symlink(), "preview_path_refused");
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "preview_path_refused"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(anyhow!("preview_cache_io")),
        }
    }
    Ok(())
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

fn trim(root: &Path, budget: u64, count_limit: usize, protected: Option<&Path>) -> Result<()> {
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
        let metadata = fs::symlink_metadata(&path).context("preview_cache_io")?;
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
            {
                fs::remove_file(&path).context("preview_cache_io")?;
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
        fs::remove_file(path).context("preview_cache_io")?;
        total = total.saturating_sub(size);
        count -= 1;
    }
    ensure!(
        total <= budget && count <= count_limit,
        "preview_cache_full"
    );
    Ok(())
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
