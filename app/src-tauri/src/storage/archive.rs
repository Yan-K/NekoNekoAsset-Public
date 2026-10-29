//! Archive extraction is a prepare operation: sources are never deleted here.
//! The caller must publish and durably record the returned inventory before cleanup.
#[cfg(windows)]
#[path = "archive_process.rs"]
mod process;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_LISTING_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 500_000;
const BUFFER_SIZE: usize = 128 * 1024;
const WORKSPACE_MARKER: &str = ".nna-archive-owner";
const WORKSPACE_OWNER: &str = "NNA-ARCHIVE-WORKSPACE-V1\n";
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(value & 1)));
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub relative_path: String,
    pub size_bytes: u64,
    pub is_directory: bool,
    pub crc32: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedEntry {
    pub relative_path: String,
    pub size_bytes: u64,
    pub hash: String,
    #[serde(default)]
    pub is_directory: bool,
}

#[derive(Debug)]
pub struct ArchiveExtraction {
    pub staging_path: PathBuf,
    pub inventory: Vec<VerifiedEntry>,
    pub source_paths: Vec<PathBuf>,
    // Prepared content remains helper-owned until the caller moves it into
    // durable storage. Any publication/verification failure cleans staging.
    _workspace: WorkspaceGuard,
}

#[derive(Debug)]
struct WorkspaceGuard {
    root: PathBuf,
    parent: PathBuf,
    armed: bool,
    owner: Option<File>,
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        // This exact, fresh UUID child was created by this invocation. Never
        // derive a deletion target from archive member names or caller records.
        if self.armed && self.root.parent() == Some(self.parent.as_path()) {
            self.owner.take();
            let _ = remove_workspace(&self.root);
        }
    }
}

/// Recover only workspaces durably marked by this helper whose lifetime lock
/// has been released. Legacy, foreign, linked and active children remain untouched.
pub(super) fn recover_workspaces(staging_parent: &Path) -> Result<usize> {
    if !staging_parent.is_dir() || validate_no_links(staging_parent).is_err() {
        return Ok(0);
    }
    let parent = staging_parent
        .canonicalize()
        .context("storage_staging_cleanup_failed")?;
    let mut removed = 0;
    for child in fs::read_dir(&parent).context("storage_staging_cleanup_failed")? {
        let Ok(child) = child else {
            continue;
        };
        let name = child.file_name();
        let Some(identity) = name.to_str().and_then(|name| name.strip_prefix("archive-")) else {
            continue;
        };
        let Ok(id) = uuid::Uuid::parse_str(identity) else {
            continue;
        };
        if id.to_string() != identity {
            continue;
        }
        let workspace = child.path();
        if workspace.parent() != Some(parent.as_path())
            || validate_no_links(&workspace).is_err()
            || !workspace.is_dir()
        {
            continue;
        }
        let marker = workspace.join(WORKSPACE_MARKER);
        if validate_no_links(&marker).is_err() {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&marker) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 128 {
            continue;
        }
        let Ok(mut owner) = OpenOptions::new().read(true).write(true).open(&marker) else {
            continue;
        };
        if fs2::FileExt::try_lock_exclusive(&owner).is_err() {
            continue;
        }
        let mut body = Vec::new();
        if (&mut owner).take(129).read_to_end(&mut body).is_err()
            || body != format!("{WORKSPACE_OWNER}{id}\n").as_bytes()
        {
            continue;
        }
        // The UUID child, matching durable owner marker and released OS lock
        // prove ownership before this exact bounded deletion is attempted.
        drop(owner);
        if remove_workspace(&workspace).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn remove_workspace(workspace: &Path) -> Result<()> {
    validate_no_links(workspace)?;
    for child in fs::read_dir(workspace).context("storage_staging_cleanup_failed")? {
        let child = child.context("storage_staging_cleanup_failed")?;
        if child.file_name() != WORKSPACE_MARKER {
            remove_private_tree(&child.path())?;
        }
    }
    // Retain ownership proof until every payload child has been removed, so a
    // locked file can be retried at the next startup without guessing ownership.
    let marker = workspace.join(WORKSPACE_MARKER);
    if marker
        .try_exists()
        .context("storage_staging_cleanup_failed")?
    {
        fs::remove_file(marker).context("storage_staging_cleanup_failed")?;
    }
    fs::remove_dir(workspace).context("storage_staging_cleanup_failed")?;
    Ok(())
}

/// Remove only a helper-owned workspace. Reparse entries are unlinked, never
/// descended into, even after a failed or malicious extraction.
fn remove_private_tree(path: &Path) -> Result<()> {
    let mut pending = vec![(path.to_owned(), false)];
    while let Some((path, visited)) = pending.pop() {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => bail!("storage_staging_cleanup_failed"),
        };
        #[allow(unused_mut)]
        let mut reparse = metadata.file_type().is_symlink();
        #[allow(unused_mut)]
        let mut directory = metadata.is_dir();
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            reparse |= metadata.file_attributes() & 0x400 != 0;
            directory |= metadata.file_attributes() & 0x10 != 0;
        }
        if reparse {
            if directory {
                fs::remove_dir(&path)
            } else {
                fs::remove_file(&path)
            }
            .context("storage_staging_cleanup_failed")?;
            continue;
        }
        #[cfg(windows)]
        if metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&path, permissions).context("storage_staging_cleanup_failed")?;
        }
        if directory && !visited {
            pending.push((path.clone(), true));
            for item in fs::read_dir(&path).context("storage_staging_cleanup_failed")? {
                pending.push((
                    item.context("storage_staging_cleanup_failed")?.path(),
                    false,
                ));
            }
        } else if directory {
            fs::remove_dir(&path).context("storage_staging_cleanup_failed")?;
        } else {
            fs::remove_file(&path).context("storage_staging_cleanup_failed")?;
        }
    }
    Ok(())
}

fn check_cancel(cancelled: &AtomicBool) -> Result<()> {
    ensure!(!cancelled.load(Ordering::Relaxed), "cancelled");
    Ok(())
}

/// Portable path policy, including aliases that Win32 would silently normalize.
/// Backslashes are archive separators even when this runs on Unix.
pub fn validate_relative_path(value: &str) -> Result<PathBuf> {
    ensure!(
        !value.is_empty() && value.len() <= 32_000,
        "archive_unsafe_path"
    );
    ensure!(!value.starts_with(['/', '\\']), "archive_unsafe_path");
    let normalized = value.replace('\\', "/");
    let mut result = PathBuf::new();
    for component in normalized.split('/') {
        ensure!(
            !component.is_empty() && component != "." && component != "..",
            "archive_unsafe_path"
        );
        ensure!(!component.ends_with(['.', ' ']), "archive_unsafe_path");
        ensure!(
            !component
                .chars()
                .any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*')),
            "archive_unsafe_path"
        );
        let stem = component
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_uppercase();
        let device = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
        ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                &stem[3..],
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            ));
        ensure!(!device, "archive_unsafe_path");
        result.push(component);
    }
    Ok(result)
}

fn portable_path(path: &Path) -> Result<String> {
    Ok(path
        .to_str()
        .context("archive_invalid_unicode")?
        .replace('\\', "/"))
}

/// Reject symlinks/reparse points at the target and every existing ancestor.
pub fn validate_no_links(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                ensure!(!metadata.file_type().is_symlink(), "storage_link_forbidden");
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    ensure!(
                        metadata.file_attributes() & 0x400 == 0,
                        "storage_link_forbidden"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(anyhow::Error::new(error).context("storage_path_unavailable")),
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum VolumeKind {
    Single,
    Numbered,
    PartRar,
    ClassicRar,
    SplitZip,
}

#[derive(Clone, Debug)]
struct VolumeName {
    key: String,
    kind: VolumeKind,
    index: u32,
}

fn volume_name(name: &str) -> Option<VolumeName> {
    let lower = name.to_lowercase();
    if lower.ends_with(".unitypackage") {
        return None;
    }
    if let Some((base, suffix)) = lower.rsplit_once('.') {
        if (base.ends_with(".zip") || base.ends_with(".7z"))
            && suffix.len() >= 3
            && suffix.bytes().all(|v| v.is_ascii_digit())
        {
            return Some(VolumeName {
                key: base.to_owned(),
                kind: VolumeKind::Numbered,
                index: suffix.parse().ok()?,
            });
        }
        if suffix.len() == 3 && suffix.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
            let number: u32 = suffix[1..].parse().ok()?;
            if suffix.starts_with('r') {
                return Some(VolumeName {
                    key: base.to_owned(),
                    kind: VolumeKind::ClassicRar,
                    index: number + 1,
                });
            }
            if suffix.starts_with('z') {
                return Some(VolumeName {
                    key: base.to_owned(),
                    kind: VolumeKind::SplitZip,
                    index: number,
                });
            }
        }
    }
    if let Some(base) = lower.strip_suffix(".rar") {
        if let Some((prefix, part)) = base.rsplit_once(".part") {
            if !part.is_empty() && part.bytes().all(|v| v.is_ascii_digit()) {
                return Some(VolumeName {
                    key: prefix.to_owned(),
                    kind: VolumeKind::PartRar,
                    index: part.parse().ok()?,
                });
            }
        }
        return Some(VolumeName {
            key: base.to_owned(),
            kind: VolumeKind::ClassicRar,
            index: 0,
        });
    }
    if let Some(base) = lower.strip_suffix(".zip") {
        return Some(VolumeName {
            key: base.to_owned(),
            kind: VolumeKind::SplitZip,
            index: 0,
        });
    }
    if lower.ends_with(".7z") {
        Some(VolumeName {
            key: lower,
            kind: VolumeKind::Single,
            index: 0,
        })
    } else {
        None
    }
}

pub fn is_supported_archive(path: &Path) -> bool {
    path.file_name()
        .and_then(|v| v.to_str())
        .and_then(volume_name)
        .is_some()
}

/// Returns the first volume first, followed by the remaining source paths.
/// A missing final volume is additionally detected by listing/extraction.
pub fn validate_volume_group(archive: &Path, available: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let name = archive
        .file_name()
        .and_then(|v| v.to_str())
        .context("archive_invalid_name")?;
    let wanted = volume_name(name).context("archive_unsupported")?;
    let mut members = BTreeMap::new();
    for candidate in std::iter::once(archive).chain(available.iter().map(PathBuf::as_path)) {
        let Some(candidate_name) = candidate.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        let Some(volume) = volume_name(candidate_name) else {
            continue;
        };
        if volume.key != wanted.key || volume.kind != wanted.kind {
            continue;
        }
        validate_relative_path(candidate_name)?;
        if let Some(existing) = members.get(&volume.index) {
            ensure!(existing == candidate, "archive_volume_collision");
        } else {
            members.insert(volume.index, candidate.to_owned());
        }
    }
    let first = if matches!(wanted.kind, VolumeKind::Numbered | VolumeKind::PartRar) {
        1
    } else {
        0
    };
    ensure!(
        members.keys().next().copied() == Some(first),
        "archive_missing_volume"
    );
    let mut expected = first;
    for index in members.keys() {
        ensure!(*index == expected, "archive_missing_volume");
        expected = expected.checked_add(1).context("archive_volume_limit")?;
    }
    Ok(members.into_values().collect())
}

/// Parse only the member section after 7-Zip's technical-list separator.
pub fn parse_listing(listing: &str) -> Result<Vec<ArchiveEntry>> {
    let normalized = listing.replace("\r\n", "\n");
    let (_, members) = normalized
        .split_once("\n----------\n")
        .context("archive_listing_invalid")?;
    let mut entries = Vec::new();
    for block in members
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
    {
        // Successful technical listings can append this summary after the
        // member records (for example, a ZIP with harmless trailing bytes).
        if let Some(count) = block.trim().strip_prefix("Warnings: ") {
            ensure!(count.parse::<u64>().is_ok(), "archive_listing_invalid");
            continue;
        }
        let mut fields = BTreeMap::new();
        for line in block.lines() {
            let (key, value) = line.split_once(" = ").context("archive_listing_invalid")?;
            ensure!(
                fields.insert(key, value).is_none(),
                "archive_listing_ambiguous"
            );
        }
        let raw_path = fields.get("Path").context("archive_listing_invalid")?;
        let path = portable_path(&validate_relative_path(raw_path)?)?;
        for key in [
            "Symbolic Link",
            "Hard Link",
            "Link",
            "Copy Link",
            "Reparse",
            "Reparse Point",
        ] {
            ensure!(
                fields.get(key).is_none_or(|v| v.is_empty() || *v == "-"),
                "archive_link_forbidden"
            );
        }
        ensure!(
            fields.get("Encrypted") != Some(&"+"),
            "archive_password_required"
        );
        for key in ["Alternate Stream", "Anti", "Deleted"] {
            ensure!(
                fields.get(key).is_none_or(|v| v.is_empty() || *v == "-"),
                "archive_unsupported_member"
            );
        }
        if let Some(links) = fields.get("Links") {
            ensure!(
                links.is_empty() || links.parse::<u64>().is_ok_and(|v| v <= 1),
                "archive_link_forbidden"
            );
        }
        let attributes = fields.get("Attributes").copied().unwrap_or("");
        let mode = fields.get("Mode").copied().unwrap_or("");
        for token in attributes.split_whitespace().chain(mode.split_whitespace()) {
            // Unix file modes expose symlink/device/socket/fifo types. Windows L is reparse.
            ensure!(
                !token.starts_with(['l', 'b', 'c', 's', 'p']) && !token.contains('L'),
                "archive_link_forbidden"
            );
        }
        let is_directory = fields.get("Folder") == Some(&"+")
            || attributes
                .split_whitespace()
                .any(|v| v.starts_with('D') || v.starts_with('d'))
            || mode.starts_with('d');
        let size_bytes = match fields.get("Size") {
            Some(value) if !value.is_empty() => value.parse().context("archive_listing_invalid")?,
            _ if is_directory => 0,
            _ => bail!("archive_listing_invalid"),
        };
        ensure!(!is_directory || size_bytes == 0, "archive_listing_invalid");
        let crc32 = fields
            .get("CRC")
            .filter(|v| !v.is_empty())
            .map(|v| u32::from_str_radix(v, 16).context("archive_listing_invalid"))
            .transpose()?;
        entries.push(ArchiveEntry {
            relative_path: path,
            size_bytes,
            is_directory,
            crc32,
        });
        ensure!(entries.len() <= MAX_ENTRIES, "archive_entry_limit");
    }
    ensure!(!entries.is_empty(), "archive_empty");
    validate_inventory_paths(&entries)?;
    Ok(entries)
}

fn validate_inventory_paths(entries: &[ArchiveEntry]) -> Result<BTreeMap<String, bool>> {
    let mut explicit = BTreeSet::new();
    let mut paths: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for entry in entries {
        let path = portable_path(&validate_relative_path(&entry.relative_path)?)?;
        ensure!(
            explicit.insert(path.to_uppercase()),
            "archive_path_collision"
        );
        let components: Vec<_> = path.split('/').collect();
        for length in 1..=components.len() {
            let spelling = components[..length].join("/");
            let key = spelling.to_uppercase();
            let directory = length < components.len() || entry.is_directory;
            if let Some((old_spelling, old_directory)) = paths.get(&key) {
                ensure!(
                    *old_spelling == spelling && *old_directory == directory,
                    "archive_path_collision"
                );
            } else {
                paths.insert(key, (spelling, directory));
            }
        }
    }
    Ok(paths.into_values().collect())
}

fn read_bounded(mut reader: impl Read, limit: usize, overflow: Arc<AtomicBool>) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).context("archive_process_io")?;
        if count == 0 {
            break;
        }
        if result.len().saturating_add(count) > limit {
            overflow.store(true, Ordering::Relaxed);
        } else {
            result.extend_from_slice(&buffer[..count]);
        }
    }
    Ok(result)
}

fn run_7zip(
    executable: &Path,
    args: &[OsString],
    cancelled: &AtomicBool,
    listing: bool,
) -> Result<Vec<u8>> {
    check_cancel(cancelled)?;
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    let child_job = process::ChildJob::new()?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000004); // CREATE_NO_WINDOW | CREATE_SUSPENDED
    }
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("archive_tool_unavailable"))?;
    #[cfg(windows)]
    if let Err(error) = child_job.attach_and_resume(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let overflow = Arc::new(AtomicBool::new(false));
    let out_pipe = child.stdout.take().context("archive_process_io")?;
    let err_pipe = child.stderr.take().context("archive_process_io")?;
    let out_overflow = overflow.clone();
    let err_overflow = overflow.clone();
    let out = thread::spawn(move || {
        read_bounded(
            out_pipe,
            if listing {
                MAX_LISTING_BYTES
            } else {
                1024 * 1024
            },
            out_overflow,
        )
    });
    let err = thread::spawn(move || read_bounded(err_pipe, 1024 * 1024, err_overflow));
    let mut interrupted = None;
    let started = Instant::now();
    let timeout = if listing {
        Duration::from_secs(10 * 60)
    } else {
        Duration::from_secs(60 * 60)
    };
    let status = loop {
        if cancelled.load(Ordering::Relaxed)
            || overflow.load(Ordering::Relaxed)
            || started.elapsed() >= timeout
        {
            interrupted = Some(if cancelled.load(Ordering::Relaxed) {
                "cancelled"
            } else if overflow.load(Ordering::Relaxed) {
                "archive_output_limit"
            } else {
                "archive_timeout"
            });
            let _ = child.kill();
            break child.wait();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
        }
    };
    let output = out
        .join()
        .map_err(|_| anyhow::anyhow!("archive_process_io"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("archive_process_io"))??;
    if let Some(code) = interrupted {
        bail!(code);
    }
    ensure!(!overflow.load(Ordering::Relaxed), "archive_output_limit");
    // Only fixed classifications leave this boundary, never private CLI output.
    let status = status.context("archive_process_io")?;
    if !status.success() {
        bail!(classify_process_error(status.code(), &output, &stderr));
    }
    Ok(output)
}

fn classify_process_error(exit: Option<i32>, stdout: &[u8], stderr: &[u8]) -> &'static str {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .to_ascii_lowercase();
    if text.contains("wrong password")
        || text.contains("password is incorrect")
        || text.contains("can not open encrypted archive")
    {
        "archive_password_required"
    } else if text.contains("missing volume")
        || text.contains("cannot find volume")
        || text.contains("can not find volume")
        || (text.contains("unexpected end of archive")
            && (text.contains("type = split") || text.contains("multivolume = +")))
    {
        "archive_missing_volume"
    } else if text.contains("not enough space")
        || text.contains("no space left")
        || text.contains("disk full")
    {
        "storage_low_space"
    } else {
        match exit {
            Some(7) => "archive_tool_arguments",
            Some(8) => "archive_tool_memory",
            Some(255) => "cancelled",
            _ => "archive_corrupt",
        }
    }
}

fn cli_args(operation: &str, archive: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        OsString::from(operation),
        "-y".into(),
        "-bd".into(),
        "-bb0".into(),
        "-sccUTF-8".into(),
        // Never prompt for credentials. Encrypted members are rejected by preflight.
        "-pNNA-no-password-supplied".into(),
        "--".into(),
        archive.as_os_str().to_owned(),
    ]
    .into();
    let name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    let zip = name.ends_with(".zip")
        || name.rsplit_once('.').is_some_and(|(base, suffix)| {
            base.ends_with(".zip")
                && suffix.len() >= 3
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
        });
    if zip {
        // Unlabelled BOOTH ZIP names intentionally prefer Japanese Windows
        // encoding. 7-Zip still honors explicit UTF-8/Unicode ZIP name metadata.
        // Keep listing and extraction on the same decoding policy.
        args.insert(1, "-mcp=932".into());
    }
    args
}

fn hash_file(path: &Path, cancelled: &AtomicBool) -> Result<(u64, String, u32)> {
    let mut file = File::open(path).context("storage_read_failed")?;
    let mut digest = Sha256::new();
    let mut crc = !0u32;
    let mut size = 0u64;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    loop {
        check_cancel(cancelled)?;
        let count = file.read(&mut buffer).context("storage_read_failed")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        for byte in &buffer[..count] {
            crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(*byte)) & 0xff) as usize];
        }
        size = size
            .checked_add(count as u64)
            .context("archive_size_overflow")?;
    }
    Ok((size, hex::encode(digest.finalize()), !crc))
}

fn copy_source(source: &Path, destination: &Path, cancelled: &AtomicBool) -> Result<()> {
    validate_no_links(source)?;
    ensure!(
        fs::metadata(source)
            .context("archive_missing_volume")?
            .is_file(),
        "archive_missing_volume"
    );
    let mut input = File::open(source).context("archive_missing_volume")?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .context("storage_write_failed")?;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut hash = Sha256::new();
    loop {
        check_cancel(cancelled)?;
        let count = input.read(&mut buffer).context("storage_read_failed")?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .context("storage_write_failed")?;
    }
    output.sync_all().context("storage_write_failed")?;
    drop(output);
    ensure!(
        hex::encode(hash.finalize()) == hash_file(destination, cancelled)?.1,
        "storage_copy_verification_failed"
    );
    Ok(())
}

fn reject_hard_links(file: &File, metadata: &fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = file;
        ensure!(metadata.nlink() == 1, "archive_link_forbidden");
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct FileInfo {
            attributes: u32,
            creation: [u32; 2],
            access: [u32; 2],
            write: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                info: *mut FileInfo,
            ) -> i32;
        }
        let mut info = std::mem::MaybeUninit::<FileInfo>::uninit();
        // The OS initializes the repr(C) structure on success; file owns this live handle.
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } != 0,
            "storage_read_failed"
        );
        ensure!(
            unsafe { info.assume_init() }.links == 1,
            "archive_link_forbidden"
        );
        let _ = metadata;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, metadata);
        bail!("archive_platform_unsupported");
    }
    Ok(())
}

/// Verify exact files and directories, including implicit parent directories.
pub fn verify_extracted_inventory(
    root: &Path,
    expected: &[ArchiveEntry],
    cancelled: &AtomicBool,
) -> Result<Vec<VerifiedEntry>> {
    validate_no_links(root)?;
    let mut remaining = validate_inventory_paths(expected)?;
    let expected_files: BTreeMap<_, _> = expected
        .iter()
        .filter(|v| !v.is_directory)
        .map(|v| (v.relative_path.as_str(), v))
        .collect();
    let mut pending = vec![root.to_owned()];
    let mut inventory = Vec::new();
    while let Some(directory) = pending.pop() {
        check_cancel(cancelled)?;
        for item in fs::read_dir(&directory).context("storage_read_failed")? {
            let item = item.context("storage_read_failed")?;
            let path = item.path();
            validate_no_links(&path)?;
            let relative = portable_path(path.strip_prefix(root).context("archive_unsafe_path")?)?;
            let is_directory = remaining
                .remove(&relative)
                .context("archive_inventory_mismatch")?;
            let metadata = fs::symlink_metadata(&path).context("storage_read_failed")?;
            ensure!(
                metadata.is_dir() == is_directory,
                "archive_inventory_mismatch"
            );
            if is_directory {
                inventory.push(VerifiedEntry {
                    relative_path: relative,
                    size_bytes: 0,
                    hash: String::new(),
                    is_directory: true,
                });
                pending.push(path);
                continue;
            }
            ensure!(metadata.is_file(), "archive_link_forbidden");
            // 7-Zip may restore a read-only flag. Temporarily allow flushing the
            // new staged file, then restore its original permissions.
            let original_permissions = metadata.permissions();
            #[cfg(windows)]
            if original_permissions.readonly() {
                let mut writable = original_permissions.clone();
                writable.set_readonly(false);
                fs::set_permissions(&path, writable).context("storage_write_failed")?;
            }
            let file = OpenOptions::new()
                .read(true)
                .write(cfg!(windows))
                .open(&path)
                .context("storage_read_failed")?;
            reject_hard_links(&file, &metadata)?;
            let flushed = file.sync_all();
            drop(file);
            #[cfg(windows)]
            fs::set_permissions(&path, original_permissions).context("storage_write_failed")?;
            #[cfg(not(windows))]
            let _ = original_permissions;
            flushed.context("storage_write_failed")?;
            let (size, hash, crc) = hash_file(&path, cancelled)?;
            let expected_file = expected_files
                .get(relative.as_str())
                .context("archive_inventory_mismatch")?;
            ensure!(
                size == expected_file.size_bytes,
                "archive_inventory_mismatch"
            );
            ensure!(
                expected_file
                    .crc32
                    .is_none_or(|expected_crc| expected_crc == crc),
                "archive_crc_mismatch"
            );
            inventory.push(VerifiedEntry {
                relative_path: relative,
                size_bytes: size,
                hash,
                is_directory: false,
            });
        }
    }
    ensure!(remaining.is_empty(), "archive_inventory_mismatch");
    inventory.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(inventory)
}

/// Produces a verified staging directory without expanding nested archives.
/// Failure/cancellation removes only this invocation's private staging tree.
pub fn extract_archive(
    seven_zip: &Path,
    archive: &Path,
    available_volumes: &[PathBuf],
    staging_parent: &Path,
    cancelled: &AtomicBool,
) -> Result<ArchiveExtraction> {
    check_cancel(cancelled)?;
    validate_no_links(staging_parent)?;
    ensure!(staging_parent.is_dir(), "storage_staging_unavailable");
    let staging_parent = staging_parent
        .canonicalize()
        .context("storage_staging_unavailable")?;
    let sources = validate_volume_group(archive, available_volumes)?;
    let source_state = sources
        .iter()
        .map(|source| {
            validate_no_links(source)?;
            let metadata = fs::metadata(source).context("archive_missing_volume")?;
            ensure!(metadata.is_file(), "archive_missing_volume");
            Ok((
                metadata.len(),
                metadata.modified().context("storage_read_failed")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        fs2::available_space(&staging_parent).context("storage_space_unavailable")?
            >= 16 * 1024 * 1024,
        "storage_low_space"
    );
    let workspace_id = uuid::Uuid::new_v4();
    let workspace = staging_parent.join(format!("archive-{workspace_id}"));
    fs::create_dir(&workspace).context("storage_write_failed")?;
    let mut guard = WorkspaceGuard {
        root: workspace.clone(),
        parent: staging_parent,
        armed: true,
        owner: None,
    };
    let mut owner = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(workspace.join(WORKSPACE_MARKER))
        .context("storage_write_failed")?;
    fs2::FileExt::try_lock_exclusive(&owner).context("storage_write_failed")?;
    owner
        .write_all(format!("{WORKSPACE_OWNER}{workspace_id}\n").as_bytes())
        .context("storage_write_failed")?;
    owner.sync_all().context("storage_write_failed")?;
    guard.owner = Some(owner);
    let inputs = workspace.join("sources");
    let content = workspace.join("content");
    fs::create_dir(&content).context("storage_write_failed")?;
    let first_name = sources[0]
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(volume_name)
        .context("archive_invalid_name")?;
    // Keep split input sets explicit: 7-Zip otherwise discovers adjacent final
    // volumes that the caller did not include, hiding a missing-volume claim.
    let direct = sources.len() == 1
        && first_name.index == 0
        && !fs::read_dir(sources[0].parent().context("archive_invalid_name")?)
            .context("archive_missing_volume")?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().to_str().and_then(volume_name))
            .any(|name| {
                name.key == first_name.key
                    && name.kind == first_name.kind
                    && name.index != first_name.index
            });
    let first = if direct {
        // Ordinary complete single archives are read in place.
        sources[0].clone()
    } else {
        fs::create_dir(&inputs).context("storage_write_failed")?;
        for source in &sources {
            check_cancel(cancelled)?;
            let name = source.file_name().context("archive_invalid_name")?;
            let destination = inputs.join(name);
            // Group aliases need no payload copy on filesystems supporting
            // hard links. Read-only inputs use the existing verified copy path,
            // so helper cleanup never changes their attributes via an alias.
            if fs::metadata(source)
                .context("archive_missing_volume")?
                .permissions()
                .readonly()
                || fs::hard_link(source, &destination).is_err()
            {
                let size = fs::metadata(source)
                    .context("archive_missing_volume")?
                    .len();
                ensure!(
                    fs2::available_space(&workspace).context("storage_space_unavailable")?
                        >= size.saturating_add(16 * 1024 * 1024),
                    "storage_low_space"
                );
                copy_source(source, &destination, cancelled)?;
            }
        }
        inputs.join(sources[0].file_name().context("archive_invalid_name")?)
    };
    let mut listing_args = cli_args("l", &first);
    listing_args.insert(1, "-slt".into());
    let listing = run_7zip(seven_zip, &listing_args, cancelled, true)?;
    let entries = parse_listing(std::str::from_utf8(&listing).context("archive_listing_invalid")?)?;
    let expanded_size = entries.iter().try_fold(0u64, |total, entry| {
        total
            .checked_add(entry.size_bytes)
            .context("archive_size_overflow")
    })?;
    ensure!(
        fs2::available_space(&workspace).context("storage_space_unavailable")?
            >= expanded_size.saturating_add(16 * 1024 * 1024),
        "storage_low_space"
    );
    // Bind writes to literal validated file names. Technical listings are text:
    // a malicious name containing a newline must never smuggle an unlisted
    // member into extraction. Never select a directory recursively.
    let member_list = workspace.join("members.txt");
    let mut names = BufWriter::new(
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&member_list)
            .context("storage_write_failed")?,
    );
    let mut file_count = 0usize;
    for entry in &entries {
        check_cancel(cancelled)?;
        if entry.is_directory {
            fs::create_dir_all(content.join(validate_relative_path(&entry.relative_path)?))
                .context("storage_write_failed")?;
        } else {
            // 7-Zip trims unquoted listfile lines. Quotes retain a leading
            // space in a literal member name; validated paths cannot contain quotes.
            writeln!(names, "\"{}\"", entry.relative_path).context("storage_write_failed")?;
            file_count += 1;
        }
    }
    names.flush().context("storage_write_failed")?;
    drop(names);
    if file_count > 0 {
        let mut extraction_args = cli_args("x", &first);
        let mut output_arg = OsString::from("-o");
        output_arg.push(&content);
        let mut include_arg = OsString::from("-i@");
        include_arg.push(&member_list);
        extraction_args.splice(
            1..1,
            [
                output_arg,
                include_arg,
                "-spd".into(),
                "-r-".into(),
                "-scsUTF-8".into(),
            ],
        );
        run_7zip(seven_zip, &extraction_args, cancelled, false)?;
    }
    let inventory = verify_extracted_inventory(&content, &entries, cancelled)?;
    check_cancel(cancelled)?;
    for (source, state) in sources.iter().zip(source_state) {
        validate_no_links(source)?;
        let metadata = fs::metadata(source).context("archive_missing_volume")?;
        ensure!(
            metadata.is_file()
                && metadata.len() == state.0
                && metadata.modified().context("storage_read_failed")? == state.1,
            "file_integrity_failed"
        );
    }
    if inputs.exists() {
        remove_private_tree(&inputs)?;
    }
    fs::remove_file(&member_list).context("storage_staging_cleanup_failed")?;
    Ok(ArchiveExtraction {
        staging_path: content,
        inventory,
        source_paths: sources,
        _workspace: guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_workspace_recovery_requires_marker_identity_and_released_lock() {
        let root =
            std::env::temp_dir().join(format!("nna-archive-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let active_id = uuid::Uuid::new_v4();
        let active = root.join(format!("archive-{active_id}"));
        fs::create_dir(&active).unwrap();
        let marker = active.join(WORKSPACE_MARKER);
        let mut owner = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&marker)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&owner).unwrap();
        owner
            .write_all(format!("{WORKSPACE_OWNER}{active_id}\n").as_bytes())
            .unwrap();
        owner.sync_all().unwrap();
        fs::create_dir(active.join("content")).unwrap();
        fs::write(
            active.join("content/authored.bin"),
            b"authored uncommitted bytes",
        )
        .unwrap();
        let legacy = root.join(format!("archive-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("human.bin"), b"unmarked bytes").unwrap();
        let invalid = root.join(format!("archive-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&invalid).unwrap();
        fs::write(
            invalid.join(WORKSPACE_MARKER),
            format!("{WORKSPACE_OWNER}{}\n", uuid::Uuid::new_v4()),
        )
        .unwrap();
        assert_eq!(recover_workspaces(&root).unwrap(), 0);
        assert!(active.join("content/authored.bin").exists());
        drop(owner);
        assert_eq!(recover_workspaces(&root).unwrap(), 1);
        assert!(!active.exists());
        assert!(legacy.join("human.bin").exists());
        assert!(invalid.join(WORKSPACE_MARKER).exists());
        fs::remove_file(legacy.join("human.bin")).unwrap();
        fs::remove_dir(legacy).unwrap();
        fs::remove_file(invalid.join(WORKSPACE_MARKER)).unwrap();
        fs::remove_dir(invalid).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn locked_workspace_payload_retains_ownership_proof_for_the_next_recovery() {
        use std::os::windows::fs::OpenOptionsExt;
        let root =
            std::env::temp_dir().join(format!("nna-archive-locked-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let id = uuid::Uuid::new_v4();
        let workspace = root.join(format!("archive-{id}"));
        fs::create_dir(&workspace).unwrap();
        fs::write(
            workspace.join(WORKSPACE_MARKER),
            format!("{WORKSPACE_OWNER}{id}\n"),
        )
        .unwrap();
        let payload = workspace.join("authored.bin");
        fs::write(&payload, b"authored locked bytes").unwrap();
        let handle = File::options()
            .read(true)
            .share_mode(3)
            .open(&payload)
            .unwrap();
        assert_eq!(recover_workspaces(&root).unwrap(), 0);
        assert!(workspace.join(WORKSPACE_MARKER).exists());
        drop(handle);
        assert_eq!(recover_workspaces(&root).unwrap(), 1);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn portable_paths_reject_escape_and_windows_aliases() {
        for path in [
            "../x",
            "a/../../x",
            "C:\\x",
            "\\\\host\\share",
            "/x",
            "a//b",
            "a/./b",
            "a ",
            "a.",
            "CON.txt",
            "lpt¹.bin",
            "x:NNA",
            "a\nPath = evil",
            "a/aux/file",
            "COM1 .txt",
        ] {
            assert!(validate_relative_path(path).is_err(), "{path}");
        }
        assert_eq!(
            portable_path(&validate_relative_path("衣装\\素材/texture.png").unwrap()).unwrap(),
            "衣装/素材/texture.png"
        );
    }

    fn listing(records: &str) -> String {
        format!("7-Zip\n\nPath = source.zip\nType = zip\n\n----------\n{records}\n")
    }

    // Independently authored, uncompressed fixture from the public RAR5 field
    // specification: https://www.rarlab.com/technote.htm . No codec source used.
    fn tiny_rar_fixture(name: &str, part: Option<u8>) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            !bytes.iter().fold(!0u32, |value, byte| {
                (value >> 8) ^ CRC_TABLE[((value ^ u32::from(*byte)) & 0xff) as usize]
            })
        }
        fn block(output: &mut Vec<u8>, body: &[u8]) {
            assert!(body.len() < 128);
            let mut header = vec![body.len() as u8];
            header.extend_from_slice(body);
            output.extend_from_slice(&crc(&header).to_le_bytes());
            output.extend(header);
        }
        assert!(name.len() < 100);
        let mut output = b"Rar!\x1a\x07\x01\0".to_vec();
        let payload = match part {
            Some(0) => &b"a"[..],
            Some(1) => &b"bc"[..],
            None => &b"abc"[..],
            _ => panic!("invalid fixture part"),
        };
        match part {
            None => block(&mut output, &[1, 0, 0]),
            Some(0) => block(&mut output, &[1, 0, 1]),
            Some(_) => block(&mut output, &[1, 0, 3, 1]),
        }
        // file header, data present, packed size, CRC present, unpacked size, attributes
        let flags = match part {
            None => 2,
            Some(0) => 18,
            Some(_) => 10,
        };
        let mut file = vec![2, flags, payload.len() as u8, 4, 3, 32];
        file.extend_from_slice(&crc(if part == Some(0) { payload } else { b"abc" }).to_le_bytes());
        file.extend_from_slice(&[0, 0, name.len() as u8]); // store method, Windows, name length
        file.extend_from_slice(name.as_bytes());
        block(&mut output, &file);
        output.extend_from_slice(payload);
        block(&mut output, &[5, 0, u8::from(part == Some(0))]);
        output
    }

    #[test]
    fn listing_accepts_warning_counts_but_never_error_or_malformed_summaries() {
        let members = "Path = normal.txt\nSize = 3\nCRC = 352441C2";
        let normal = parse_listing(&listing(members)).unwrap();
        assert_eq!(
            parse_listing(&listing(&format!("{members}\n\n\nWarnings: 1"))).unwrap(),
            normal
        );
        for summary in ["Errors: 1", "Warnings: unknown", "Warnings: 1\nErrors: 1"] {
            assert!(parse_listing(&listing(&format!("{members}\n\n{summary}"))).is_err());
        }
    }

    #[test]
    fn listing_rejects_links_collisions_and_ambiguous_records() {
        for records in [
            "Path = a\nSize = 0\nSymbolic Link = ../outside",
            "Path = a\nSize = 0\nHard Link = b",
            "Path = a\nSize = 0\nAttributes = A lrwxrwxrwx",
            "Path = a\nSize = 0\nEncrypted = +",
            "Path = a\nSize = 0\nPath = b",
            "Path = A.txt\nSize = 0\n\nPath = a.txt\nSize = 0",
            "Path = dir\nSize = 0\n\nPath = dir/file\nSize = 0",
            "Path = Dir/a\nSize = 0\n\nPath = dir/b\nSize = 0",
        ] {
            assert!(parse_listing(&listing(records)).is_err(), "{records}");
        }
        let entries = parse_listing(&listing("Path = 衣装\nFolder = +\nSize = 0\n\nPath = 衣装/a.unitypackage\nSize = 3\nCRC = 352441C2")).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!is_supported_archive(Path::new("a.unitypackage")));
    }

    #[test]
    fn volume_groups_are_contiguous_and_keep_original_paths() {
        let paths = vec![
            PathBuf::from("a/demo.7z.001"),
            PathBuf::from("b/demo.7z.002"),
            PathBuf::from("c/demo.7z.003"),
        ];
        assert_eq!(validate_volume_group(&paths[1], &paths).unwrap(), paths);
        assert!(validate_volume_group(&paths[0], &[paths[2].clone()]).is_err());
        assert!(validate_volume_group(&paths[1], &[]).is_err());
        assert!(validate_volume_group(&paths[0], &[PathBuf::from("other/demo.7z.001")]).is_err());
        let zip = vec![
            PathBuf::from("demo.zip"),
            PathBuf::from("demo.z01"),
            PathBuf::from("demo.z02"),
        ];
        assert_eq!(validate_volume_group(&zip[2], &zip).unwrap(), zip);
        let rar = vec![
            PathBuf::from("demo.part01.rar"),
            PathBuf::from("demo.part02.rar"),
        ];
        assert_eq!(validate_volume_group(&rar[1], &rar).unwrap(), rar);
    }

    #[test]
    fn process_errors_are_fixed_actionable_codes() {
        assert_eq!(
            classify_process_error(Some(2), b"private.zip", b"Wrong password?"),
            "archive_password_required"
        );
        assert_eq!(
            classify_process_error(Some(2), b"", b"Missing volume : private.part2.rar"),
            "archive_missing_volume"
        );
        assert_eq!(
            classify_process_error(Some(2), b"", b"Unexpected end of archive"),
            "archive_corrupt"
        );
        assert_eq!(
            classify_process_error(Some(2), b"Type = Split", b"Unexpected end of archive"),
            "archive_missing_volume"
        );
        assert_eq!(
            classify_process_error(Some(2), b"", b"There is not enough space on the disk"),
            "storage_low_space"
        );
    }

    #[test]
    fn inventory_checks_missing_extra_size_crc_and_cancel() {
        let root = std::env::temp_dir().join(format!("nna-archive-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("衣装")).unwrap();
        fs::write(root.join("衣装/a.unitypackage"), b"abc").unwrap();
        let mut entries = parse_listing(&listing(
            "Path = 衣装/a.unitypackage\nSize = 3\nCRC = 352441C2",
        ))
        .unwrap();
        let cancelled = AtomicBool::new(false);
        let result = verify_extracted_inventory(&root, &entries, &cancelled).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result[0].is_directory);
        assert_eq!(
            result[1].hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        entries[0].size_bytes = 4;
        assert!(verify_extracted_inventory(&root, &entries, &cancelled).is_err());
        entries[0].size_bytes = 3;
        entries[0].crc32 = Some(0);
        assert!(verify_extracted_inventory(&root, &entries, &cancelled).is_err());
        entries[0].crc32 = None;
        fs::write(root.join("extra"), b"").unwrap();
        assert!(verify_extracted_inventory(&root, &entries, &cancelled).is_err());
        fs::remove_file(root.join("extra")).unwrap();
        cancelled.store(true, Ordering::Relaxed);
        assert!(verify_extracted_inventory(&root, &entries, &cancelled).is_err());
        cancelled.store(false, Ordering::Relaxed);
        fs::remove_file(root.join("衣装/a.unitypackage")).unwrap();
        assert!(verify_extracted_inventory(&root, &entries, &cancelled).is_err());
        // This exact UUID directory was created by this test and contains synthetic bytes only.
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn inventory_rejects_hard_links() {
        let root =
            std::env::temp_dir().join(format!("nna-archive-link-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("first"), b"abc").unwrap();
        fs::hard_link(root.join("first"), root.join("second")).unwrap();
        let entries = parse_listing(&listing(
            "Path = first\nSize = 3\n\nPath = second\nSize = 3",
        ))
        .unwrap();
        assert!(verify_extracted_inventory(&root, &entries, &AtomicBool::new(false)).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn zip_cli_uses_japanese_legacy_names_for_listing_and_extraction_only() {
        for operation in ["l", "x"] {
            for archive in ["source.zip", "source.ZIP", "source.zip.001"] {
                assert!(
                    cli_args(operation, Path::new(archive)).contains(&OsString::from("-mcp=932"))
                );
            }
            for archive in ["source.7z", "source.rar", "source.7z.001"] {
                assert!(
                    !cli_args(operation, Path::new(archive)).contains(&OsString::from("-mcp=932"))
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn bundled_zip_roundtrip_preserves_cp932_utf8_and_ascii_names() {
        let executable =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/tools/7zip/7z.exe");
        let root =
            std::env::temp_dir().join(format!("nna-authored-zip-names-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("staging")).unwrap();
        let names: &[(&[u8], bool)] = &[
            (b"\x83\x7c.txt", false),
            (b"\x95\x5c.txt", false),
            (b"\x83\x5c.txt", false),
            (b"Readme.txt", false),
            ("髪.txt".as_bytes(), true),
        ];
        let mut zip = Vec::new();
        let mut central = Vec::new();
        for (name, utf8) in names {
            let offset = zip.len() as u32;
            let flags: u16 = if *utf8 { 0x800 } else { 0 };
            zip.extend_from_slice(&0x04034b50u32.to_le_bytes());
            for value in [20, flags, 0, 0, 0] {
                zip.extend_from_slice(&value.to_le_bytes());
            }
            for value in [0x352441c2u32, 3, 3] {
                zip.extend_from_slice(&value.to_le_bytes());
            }
            for value in [name.len() as u16, 0] {
                zip.extend_from_slice(&value.to_le_bytes());
            }
            zip.extend_from_slice(name);
            zip.extend_from_slice(b"abc");
            central.extend_from_slice(&0x02014b50u32.to_le_bytes());
            for value in [20, 20, flags, 0, 0, 0] {
                central.extend_from_slice(&value.to_le_bytes());
            }
            for value in [0x352441c2u32, 3, 3] {
                central.extend_from_slice(&value.to_le_bytes());
            }
            for value in [name.len() as u16, 0, 0, 0, 0] {
                central.extend_from_slice(&value.to_le_bytes());
            }
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name);
        }
        let central_offset = zip.len() as u32;
        zip.extend_from_slice(&central);
        zip.extend_from_slice(&0x06054b50u32.to_le_bytes());
        for value in [0, 0, names.len() as u16, names.len() as u16] {
            zip.extend_from_slice(&value.to_le_bytes());
        }
        zip.extend_from_slice(&(central.len() as u32).to_le_bytes());
        zip.extend_from_slice(&central_offset.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        let archive = root.join("authored.zip");
        fs::write(&archive, &zip).unwrap();
        let extracted = extract_archive(
            &executable,
            &archive,
            &[],
            &root.join("staging"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(extracted.inventory.len(), names.len());
        for name in ["ポ.txt", "表.txt", "ソ.txt", "Readme.txt", "髪.txt"] {
            assert!(extracted
                .inventory
                .iter()
                .any(|entry| entry.relative_path == name));
            assert_eq!(fs::read(extracted.staging_path.join(name)).unwrap(), b"abc");
        }
        assert_eq!(fs::read(archive).unwrap(), zip);
        drop(extracted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires explicit NNA_7ZIP pointing to the lead-supplied runtime"]
    fn real_7zip_roundtrip_split_password_corruption_and_retention() {
        let executable = PathBuf::from(std::env::var_os("NNA_7ZIP").expect("NNA_7ZIP required"));
        let root =
            std::env::temp_dir().join(format!("nna-archive-cli-test-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let staging = root.join("staging");
        fs::create_dir_all(source.join("衣装/empty")).unwrap();
        fs::create_dir(&staging).unwrap();
        fs::write(
            source.join("衣装/nested.zip"),
            b"nested archive bytes stay intact",
        )
        .unwrap();
        fs::write(
            source.join("衣装/package.unitypackage"),
            b"unitypackage stays intact",
        )
        .unwrap();
        let bytes: Vec<_> = (0..20000).map(|index| (index % 251) as u8).collect();
        fs::write(source.join("衣装/content.bin"), &bytes).unwrap();
        let cancelled = AtomicBool::new(false);
        let create = |target: &Path, extra: &[&str]| {
            let mut args: Vec<OsString> = vec![
                "a".into(),
                "-tzip".into(),
                "-mx0".into(),
                "-y".into(),
                "-bd".into(),
            ];
            args.extend(extra.iter().map(OsString::from));
            args.push(target.as_os_str().to_owned());
            args.push(source.join("衣装").as_os_str().to_owned());
            run_7zip(&executable, &args, &cancelled, false).unwrap();
        };
        let zip = root.join("complete.zip");
        create(&zip, &[]);
        let result = extract_archive(&executable, &zip, &[], &staging, &cancelled).unwrap();
        assert!(!result
            .staging_path
            .parent()
            .unwrap()
            .join("sources")
            .exists());
        assert!(result
            .inventory
            .iter()
            .any(|entry| entry.is_directory && entry.relative_path.ends_with("empty")));
        assert_eq!(
            fs::read(result.staging_path.join("衣装/content.bin")).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read(result.staging_path.join("衣装/nested.zip")).unwrap(),
            b"nested archive bytes stay intact"
        );
        assert!(result
            .staging_path
            .join("衣装/package.unitypackage")
            .is_file());
        assert!(zip.is_file());

        let seven_archive = root.join("complete.7z");
        run_7zip(
            &executable,
            &[
                "a".into(),
                "-t7z".into(),
                "-y".into(),
                "-bd".into(),
                seven_archive.as_os_str().to_owned(),
                source.join("衣装").as_os_str().to_owned(),
            ],
            &cancelled,
            false,
        )
        .unwrap();
        let seven_result =
            extract_archive(&executable, &seven_archive, &[], &staging, &cancelled).unwrap();
        assert_eq!(
            fs::read(seven_result.staging_path.join("衣装/content.bin")).unwrap(),
            bytes
        );
        let rar = root.join("complete.rar");
        fs::write(&rar, tiny_rar_fixture("衣装/synthetic.txt", None)).unwrap();
        let rar_result = extract_archive(&executable, &rar, &[], &staging, &cancelled).unwrap();
        assert_eq!(
            fs::read(rar_result.staging_path.join("衣装/synthetic.txt")).unwrap(),
            b"abc"
        );
        let unsafe_rar = root.join("unsafe.rar");
        fs::write(&unsafe_rar, tiny_rar_fixture("../escape.txt", None)).unwrap();
        assert!(extract_archive(&executable, &unsafe_rar, &[], &staging, &cancelled).is_err());
        assert!(!root.join("escape.txt").exists());
        assert!(unsafe_rar.is_file());
        let ambiguous_rar = root.join("ambiguous.rar");
        fs::write(
            &ambiguous_rar,
            tiny_rar_fixture("safe\nMode = /../../escape.txt", None),
        )
        .unwrap();
        assert!(extract_archive(&executable, &ambiguous_rar, &[], &staging, &cancelled).is_err());
        assert!(!staging.join("escape.txt").exists());
        assert!(!root.join("escape.txt").exists());
        let rar_parts = vec![
            root.join("synthetic.part01.rar"),
            root.join("synthetic.part02.rar"),
        ];
        for (index, path) in rar_parts.iter().enumerate() {
            fs::write(
                path,
                tiny_rar_fixture("衣装/synthetic.txt", Some(index as u8)),
            )
            .unwrap();
        }
        let rar_split_result =
            extract_archive(&executable, &rar_parts[1], &rar_parts, &staging, &cancelled).unwrap();
        assert_eq!(
            fs::read(rar_split_result.staging_path.join("衣装/synthetic.txt")).unwrap(),
            b"abc"
        );
        assert!(rar_parts.iter().all(|path| path.is_file()));

        let split = root.join("split.zip");
        create(&split, &["-v4k"]);
        let mut parts: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("split.zip.")
            })
            .collect();
        parts.sort();
        assert!(parts.len() > 1);
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        let moved = other.join(parts[1].file_name().unwrap());
        fs::rename(&parts[1], &moved).unwrap();
        parts[1] = moved;
        let result = extract_archive(&executable, &parts[0], &parts, &staging, &cancelled).unwrap();
        assert_eq!(
            fs::read(result.staging_path.join("衣装/content.bin")).unwrap(),
            bytes
        );
        assert!(parts.iter().all(|path| path.is_file()));
        // A read-only source cannot be aliased safely for cleanup; use the
        // verified-copy fallback without changing the source's attributes.
        let readonly_part = &parts[1];
        let mut permissions = fs::metadata(readonly_part).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(readonly_part, permissions).unwrap();
        let readonly_result =
            extract_archive(&executable, &parts[0], &parts, &staging, &cancelled).unwrap();
        assert_eq!(
            fs::read(readonly_result.staging_path.join("衣装/content.bin")).unwrap(),
            bytes
        );
        assert!(fs::metadata(readonly_part)
            .unwrap()
            .permissions()
            .readonly());
        let mut permissions = fs::metadata(readonly_part).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(readonly_part, permissions).unwrap();
        let completed_staging_count = fs::read_dir(&staging).unwrap().count();
        let incomplete = parts[..parts.len() - 1].to_vec();
        assert!(
            extract_archive(&executable, &parts[0], &incomplete, &staging, &cancelled).is_err()
        );
        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            completed_staging_count
        );

        let password = root.join("password.zip");
        create(&password, &["-pSyntheticOnlyPassword"]);
        assert!(extract_archive(&executable, &password, &[], &staging, &cancelled).is_err());
        assert!(password.is_file());
        let damaged = root.join("damaged.zip");
        fs::write(&damaged, b"independently authored invalid archive").unwrap();
        assert!(extract_archive(&executable, &damaged, &[], &staging, &cancelled).is_err());
        assert!(damaged.is_file());
        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            completed_staging_count
        );
        cancelled.store(true, Ordering::Relaxed);
        assert!(extract_archive(&executable, &zip, &[], &staging, &cancelled).is_err());
        assert!(zip.is_file());
        fs::remove_dir_all(&root).unwrap();
    }
}
