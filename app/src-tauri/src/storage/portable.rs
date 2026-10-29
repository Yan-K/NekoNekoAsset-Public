//! Portable file operations. The caller holds the storage operation lock, supplies only
//! catalogue-managed payloads, and commits catalogue/root mappings after these helpers succeed.
//! No helper changes the catalogue or deletes an entire user-selected library directory.

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const FORMAT: &str = "NekoNekoAsset.full-backup";
const VERSION: u32 = 1;
const MANIFEST_LIMIT: u64 = 256 * 1024 * 1024;
const ENTRY_LIMIT: usize = 1_000_000;
const BUFFER_SIZE: usize = 128 * 1024;

#[derive(Debug, Clone)]
pub struct PayloadSource {
    pub root_id: String,
    pub relative_path: String,
    pub source_path: PathBuf,
    pub expected_hash: Option<String>,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryEntry {
    pub root_id: String,
    pub relative_path: String,
    pub hash: String,
    pub size_bytes: u64,
    /// Within a backup ZIP and the restored directory. Root identifiers are hex encoded.
    pub archive_path: String,
    #[serde(default)]
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug)]
pub struct RestoredBackup {
    /// The exact portable catalogue value supplied to create_backup; the caller validates
    /// its version and atomically restores it with the new per-device root mappings.
    pub catalogue: Value,
    pub payload: Vec<InventoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationReceipt {
    pub version: u32,
    pub source_root: PathBuf,
    pub destination_root: PathBuf,
    pub inventory: Vec<InventoryEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupManifest {
    format: String,
    version: u32,
    catalogue: Value,
    inventory: Vec<InventoryEntry>,
}

pub fn create_backup(
    destination: &Path,
    catalogue: Value,
    payload: &[PayloadSource],
    cancel: &AtomicBool,
) -> Result<BackupSummary> {
    write_zip(destination, Some(catalogue), payload, cancel)
}

/// Exports the exact usable managed files selected by the caller, retaining their
/// relative structure and distinct root namespaces. Missing/unverified files fail the
/// whole export; an existing destination is never replaced.
pub fn export_zip(
    destination: &Path,
    payload: &[PayloadSource],
    cancel: &AtomicBool,
) -> Result<BackupSummary> {
    ensure!(!payload.is_empty(), "storage.export_empty");
    write_zip(destination, None, payload, cancel)
}

fn write_zip(
    destination: &Path,
    catalogue: Option<Value>,
    payload: &[PayloadSource],
    cancel: &AtomicBool,
) -> Result<BackupSummary> {
    cancelled(cancel)?;
    let inventory = inspect_sources(payload, cancel)?;
    let total_bytes = inventory_total(&inventory)?;
    let destination = new_file_destination(destination)?;
    let parent = destination
        .parent()
        .context("storage.invalid_destination")?;
    let manifest_bytes = if let Some(catalogue) = catalogue {
        let bytes = serde_json::to_vec(&BackupManifest {
            format: FORMAT.into(),
            version: VERSION,
            catalogue,
            inventory: inventory.clone(),
        })
        .context("storage.manifest_invalid")?;
        ensure!(
            bytes.len() as u64 <= MANIFEST_LIMIT,
            "storage.manifest_too_large"
        );
        Some(bytes)
    } else {
        None
    };
    let manifest_size = manifest_bytes.as_ref().map_or(0, |b| b.len() as u64);
    // Stored ZIP avoids CPU cost on already-compressed assets and gives a predictable
    // upper bound for free-space preflight, including Zip64/central-directory overhead.
    require_space(
        parent,
        total_bytes
            .checked_add(manifest_size)
            .and_then(|n| n.checked_add((inventory.len() as u64).saturating_mul(2048)))
            .context("storage.size_overflow")?,
    )?;
    let temporary = parent.join(format!(".nna-export-{}.part", Uuid::new_v4()));
    let mut guard = Temporary::file(temporary.clone());
    let file = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .open(&temporary)
        .context("storage.export_create_failed")?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true)
        .unix_permissions(0o600);
    for (source, entry) in payload.iter().zip(&inventory) {
        cancelled(cancel)?;
        if entry.is_directory {
            checked_directory(&source.source_path)?;
            zip.add_directory(
                format!("{}/", entry.archive_path),
                options.unix_permissions(0o700),
            )
            .context("storage.export_write_failed")?;
            continue;
        }
        zip.start_file(&entry.archive_path, options)
            .context("storage.export_write_failed")?;
        let (hash, size) = transfer(&mut open_regular(&source.source_path)?, &mut zip, cancel)?;
        ensure!(
            size == entry.size_bytes && hash == entry.hash,
            "storage.source_changed"
        );
    }
    if let Some(bytes) = manifest_bytes {
        zip.start_file("manifest.json", options)
            .context("storage.export_write_failed")?;
        zip.write_all(&bytes)
            .context("storage.export_write_failed")?;
    }
    let file = zip.finish().context("storage.export_finish_failed")?;
    file.sync_all().context("storage.export_flush_failed")?;
    drop(file);
    // Read every payload back from the actual completed ZIP, including its CRC.
    verify_zip_payloads(&temporary, &inventory, cancel)?;
    cancelled(cancel)?;
    publish_file_exclusive(&temporary, &destination)?;
    sync_parent(parent)?;
    guard.disarm();
    Ok(BackupSummary {
        file_count: inventory.iter().filter(|entry| !entry.is_directory).count(),
        total_bytes,
    })
}

/// Validates the complete archive before returning a fully verified fresh tree.
/// No existing catalogue is touched. On failure, only this operation's staging tree
/// is removed. The caller may retain the completed tree if its metadata transaction fails.
pub fn restore_backup(
    source: &Path,
    fresh_root: &Path,
    cancel: &AtomicBool,
) -> Result<RestoredBackup> {
    cancelled(cancel)?;
    let mut archive = ZipArchive::new(open_regular(source)?).context("storage.backup_invalid")?;
    ensure!(
        archive.len() <= ENTRY_LIMIT + 1,
        "storage.inventory_too_large"
    );
    let manifest = read_manifest(&mut archive)?;
    validate_inventory(&manifest.inventory)?;
    validate_zip_entries(&mut archive, &manifest.inventory, true)?;
    let destination = fresh_directory_destination(fresh_root)?;
    let parent = destination
        .parent()
        .context("storage.invalid_destination")?;
    require_space(parent, inventory_total(&manifest.inventory)?)?;
    let temporary = parent.join(format!(".nna-restore-{}", Uuid::new_v4()));
    fs::create_dir(&temporary).context("storage.restore_create_failed")?;
    let mut guard = Temporary::tree(temporary.clone());
    for entry in &manifest.inventory {
        cancelled(cancel)?;
        let target = temporary.join(&entry.archive_path);
        if entry.is_directory {
            create_parents(&temporary, &target.join("placeholder"))?;
            continue;
        }
        create_parents(&temporary, &target)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .context("storage.restore_create_failed")?;
        let mut input = archive
            .by_name(&entry.archive_path)
            .context("storage.backup_payload_missing")?;
        let (hash, size) = transfer_limited(&mut input, &mut output, entry.size_bytes, cancel)?;
        ensure!(
            hash == entry.hash && size == entry.size_bytes,
            "storage.backup_hash_mismatch"
        );
        output.sync_all().context("storage.restore_flush_failed")?;
    }
    // Independently re-read persisted files before publishing the directory.
    for entry in &manifest.inventory {
        verify_disk(&temporary.join(&entry.archive_path), entry, cancel)?;
    }
    cancelled(cancel)?;
    publish_directory(&temporary, &destination, cancel)?;
    guard.disarm();
    Ok(RestoredBackup {
        catalogue: manifest.catalogue,
        payload: manifest.inventory,
    })
}

/// Copies only the caller's explicit managed inventory, never all arbitrary contents
/// of a user-selected root. The mapping switch is the caller's subsequent durable edit.
pub fn copy_verified_root(
    source_root: &Path,
    destination_root: &Path,
    payload: &[PayloadSource],
    cancel: &AtomicBool,
) -> Result<MigrationReceipt> {
    cancelled(cancel)?;
    let source_root = checked_directory(source_root)?;
    let destination_root = fresh_directory_destination(destination_root)?;
    ensure!(
        !destination_root.starts_with(&source_root) && !source_root.starts_with(&destination_root),
        "storage.migration_overlapping_roots"
    );
    let inventory = inspect_sources(payload, cancel)?;
    let mut names = BTreeMap::new();
    for (source, entry) in payload.iter().zip(&inventory) {
        insert_inventory_path(&mut names, &entry.relative_path, entry.is_directory)?;
        let expected_path = source_root.join(&entry.relative_path);
        let actual = if entry.is_directory {
            checked_directory(&source.source_path)?
        } else {
            checked_regular_path(&source.source_path)?
        };
        let expected = if entry.is_directory {
            checked_directory(&expected_path)?
        } else {
            checked_regular_path(&expected_path)?
        };
        ensure!(expected == actual, "storage.migration_unmanaged_path");
    }
    let parent = destination_root
        .parent()
        .context("storage.invalid_destination")?;
    require_space(parent, inventory_total(&inventory)?)?;
    let temporary = parent.join(format!(".nna-move-{}", Uuid::new_v4()));
    fs::create_dir(&temporary).context("storage.migration_create_failed")?;
    let mut guard = Temporary::tree(temporary.clone());
    for (source, entry) in payload.iter().zip(&inventory) {
        cancelled(cancel)?;
        let target = temporary.join(&entry.relative_path);
        if entry.is_directory {
            create_parents(&temporary, &target.join("placeholder"))?;
            continue;
        }
        create_parents(&temporary, &target)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .context("storage.migration_create_failed")?;
        let (hash, size) = transfer(&mut open_regular(&source.source_path)?, &mut output, cancel)?;
        ensure!(
            hash == entry.hash && size == entry.size_bytes,
            "storage.source_changed"
        );
        output
            .sync_all()
            .context("storage.migration_flush_failed")?;
        drop(output);
        verify_disk(&target, entry, cancel)?;
    }
    for entry in &inventory {
        verify_disk(&temporary.join(&entry.relative_path), entry, cancel)?;
    }
    cancelled(cancel)?;
    publish_directory(&temporary, &destination_root, cancel)?;
    guard.disarm();
    Ok(MigrationReceipt {
        version: VERSION,
        source_root,
        destination_root,
        inventory,
    })
}

/// An explicit whole-library move already holds both library lifetime locks.
/// Only freshly created destinations are accepted; SQLite is supplied last so
/// the target is not a usable catalogue until every managed payload is durable.
pub(super) fn copy_into_locked_root(
    source_root: &Path,
    destination_root: &Path,
    payload: &[PayloadSource],
    cancel: &AtomicBool,
) -> Result<BackupSummary> {
    let source = checked_directory(source_root)?;
    let destination = checked_directory(destination_root)?;
    ensure!(
        !source.starts_with(&destination) && !destination.starts_with(&source),
        "storage.migration_overlapping_roots"
    );
    for entry in fs::read_dir(&destination).context("storage.destination_unavailable")? {
        ensure!(
            entry
                .context("storage.destination_unavailable")?
                .file_name()
                == ".nna-library.lock",
            "storage.destination_not_fresh"
        );
    }
    let inventory = inspect_sources(payload, cancel)?;
    require_space(&destination, inventory_total(&inventory)?)?;
    let mut owned: Vec<&InventoryEntry> = Vec::new();
    let mut created_directories = Vec::new();
    let copied = (|| -> Result<()> {
        for (input, entry) in payload.iter().zip(&inventory) {
            cancelled(cancel)?;
            let target = destination.join(&entry.relative_path);
            let mut directory = if entry.is_directory { Some(target.as_path()) } else { target.parent() };
            while let Some(path) = directory {
                if path == destination { break; }
                if !path.exists() { created_directories.push(path.to_path_buf()); }
                directory = path.parent();
            }
            if entry.is_directory {
                create_parents(&destination, &target.join("placeholder"))?;
                owned.push(entry);
                continue;
            }
            create_parents(&destination, &target)?;
            let temporary = target.with_file_name(format!(".nna-move-{}.part", Uuid::new_v4()));
            let output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .context("storage.migration_create_failed")?;
            let mut partial = Temporary::file(temporary.clone());
            // Rust drops in reverse declaration order. Close the Windows file
            // before the partial guard tries to remove it on any failed copy.
            let mut output = output;
            let (hash, bytes) =
                transfer(&mut open_regular(&input.source_path)?, &mut output, cancel)?;
            ensure!(
                hash == entry.hash && bytes == entry.size_bytes,
                "storage.source_changed"
            );
            output
                .sync_all()
                .context("storage.migration_flush_failed")?;
            drop(output);
            verify_disk(&temporary, entry, cancel)?;
            publish_file_exclusive(&temporary, &target)?;
            partial.disarm();
            owned.push(entry);
        }
        Ok(())
    })();
    if let Err(error) = copied {
        // Preserve changed/new user files; remove only exact copies this call
        // created, and prune only empty directories below the selected target.
        for entry in owned.into_iter().rev() {
            let target = destination.join(&entry.relative_path);
            if entry.is_directory {
                let _ = fs::remove_dir(&target);
            } else if verify_disk(&target, entry, &AtomicBool::new(false)).is_ok() {
                let _ = fs::remove_file(&target);
            }
            let mut parent = target.parent();
            while let Some(path) = parent {
                if path == destination
                    || !path.starts_with(&destination)
                    || fs::remove_dir(path).is_err()
                {
                    break;
                }
                parent = path.parent();
            }
        }
        // The failed current entry may have created parents before any file
        // completed. Only remove the empty directories this call introduced.
        created_directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        created_directories.dedup();
        for path in created_directories {
            let _ = fs::remove_dir(path);
        }
        return Err(error);
    }
    Ok(BackupSummary {
        file_count: inventory.iter().filter(|entry| !entry.is_directory).count(),
        total_bytes: inventory_total(&inventory)?,
    })
}

/// Explicitly requested cleanup only. A receipt must come from the durable root
/// record, not arbitrary frontend JSON. All destination files and all remaining old
/// copies are reverified before any deletion; individual copies are checked again
/// immediately before deleting. Already removed old files make retry idempotent.
pub fn cleanup_old_root(receipt: &MigrationReceipt, cancel: &AtomicBool) -> Result<usize> {
    cancelled(cancel)?;
    ensure!(
        receipt.version == VERSION,
        "storage.migration_version_unsupported"
    );
    validate_inventory(&receipt.inventory)?;
    let source = checked_directory(&receipt.source_root)?;
    let destination = checked_directory(&receipt.destination_root)?;
    ensure!(
        !source.starts_with(&destination) && !destination.starts_with(&source),
        "storage.migration_overlapping_roots"
    );
    let mut names = BTreeMap::new();
    for entry in &receipt.inventory {
        insert_inventory_path(&mut names, &entry.relative_path, entry.is_directory)?;
        verify_disk(&destination.join(&entry.relative_path), entry, cancel)?;
        verify_old_if_present(&source.join(&entry.relative_path), entry, cancel)?;
    }
    let mut removed = 0;
    for entry in &receipt.inventory {
        // Empty directories are harmless to retain. File cleanup removes empty
        // ancestors opportunistically and never deletes a populated directory.
        if entry.is_directory {
            continue;
        }
        cancelled(cancel)?;
        let old = source.join(&entry.relative_path);
        verify_disk(&destination.join(&entry.relative_path), entry, cancel)?;
        if verify_old_if_present(&old, entry, cancel)? {
            cancelled(cancel)?;
            fs::remove_file(&old).context("storage.migration_cleanup_failed")?;
            removed += 1;
            // Remove only now-empty parent directories, never the root itself.
            let mut directory = old.parent();
            while let Some(path) = directory {
                if path == source || !path.starts_with(&source) {
                    break;
                }
                if fs::remove_dir(path).is_err() {
                    break;
                }
                directory = path.parent();
            }
        }
    }
    sync_parent(&source)?;
    Ok(removed)
}

fn verify_old_if_present(path: &Path, entry: &InventoryEntry, cancel: &AtomicBool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            verify_disk(path, entry, cancel)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing leaf is safe; a missing/offline root must not pass. The caller
            // already validates the root, and any still-present ancestors reject links.
            check_existing_ancestors(path)?;
            Ok(false)
        }
        Err(_) => bail!("storage.root_unavailable"),
    }
}

fn inspect_sources(payload: &[PayloadSource], cancel: &AtomicBool) -> Result<Vec<InventoryEntry>> {
    ensure!(payload.len() <= ENTRY_LIMIT, "storage.inventory_too_large");
    let mut inventory = Vec::with_capacity(payload.len());
    for source in payload {
        cancelled(cancel)?;
        ensure!(
            !source.root_id.is_empty() && source.root_id.len() <= 256,
            "storage.root_id_invalid"
        );
        safe_relative(&source.relative_path)?;
        let archive_path = archive_path(&source.root_id, &source.relative_path);
        let (hash, size_bytes) = if source.is_directory {
            checked_directory(&source.source_path)?;
            ensure!(
                source.expected_hash.as_deref().is_none_or(str::is_empty),
                "storage.directory_hash_invalid"
            );
            (String::new(), 0)
        } else {
            transfer(
                &mut open_regular(&source.source_path)?,
                &mut std::io::sink(),
                cancel,
            )?
        };
        if let Some(expected) = source
            .expected_hash
            .as_ref()
            .filter(|_| !source.is_directory)
        {
            ensure!(
                valid_hash(expected) && hash.eq_ignore_ascii_case(expected),
                "storage.source_hash_mismatch"
            );
        }
        inventory.push(InventoryEntry {
            root_id: source.root_id.clone(),
            relative_path: source.relative_path.clone(),
            hash,
            size_bytes,
            archive_path,
            is_directory: source.is_directory,
        });
    }
    validate_inventory(&inventory)?;
    Ok(inventory)
}

fn read_manifest(archive: &mut ZipArchive<File>) -> Result<BackupManifest> {
    let mut entry = archive
        .by_name("manifest.json")
        .context("storage.backup_manifest_missing")?;
    ensure!(entry.size() <= MANIFEST_LIMIT, "storage.manifest_too_large");
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)
        .context("storage.backup_invalid")?;
    ensure!(
        bytes.len() as u64 <= MANIFEST_LIMIT,
        "storage.manifest_too_large"
    );
    // Check the envelope version before deserializing v1 fields, so future layouts
    // report a useful unsupported-version result rather than an incidental parse error.
    let value: Value = serde_json::from_slice(&bytes).context("storage.backup_manifest_invalid")?;
    ensure!(
        value.get("format").and_then(Value::as_str) == Some(FORMAT),
        "storage.backup_format_unsupported"
    );
    ensure!(
        value.get("version").and_then(Value::as_u64) == Some(VERSION as u64),
        "storage.backup_version_unsupported"
    );
    serde_json::from_value(value).context("storage.backup_manifest_invalid")
}

fn validate_inventory(inventory: &[InventoryEntry]) -> Result<()> {
    ensure!(
        inventory.len() <= ENTRY_LIMIT,
        "storage.inventory_too_large"
    );
    let mut names = BTreeMap::new();
    for entry in inventory {
        ensure!(
            !entry.root_id.is_empty() && entry.root_id.len() <= 256,
            "storage.root_id_invalid"
        );
        safe_relative(&entry.relative_path)?;
        ensure!(
            if entry.is_directory {
                entry.hash.is_empty() && entry.size_bytes == 0
            } else {
                valid_hash(&entry.hash)
            },
            "storage.inventory_hash_invalid"
        );
        ensure!(
            entry.archive_path == archive_path(&entry.root_id, &entry.relative_path),
            "storage.inventory_path_invalid"
        );
        insert_inventory_path(&mut names, &entry.archive_path, entry.is_directory)?;
    }
    inventory_total(inventory)?;
    Ok(())
}

fn validate_zip_entries(
    archive: &mut ZipArchive<File>,
    inventory: &[InventoryEntry],
    has_manifest: bool,
) -> Result<()> {
    ensure!(
        archive.len() == inventory.len() + usize::from(has_manifest),
        "storage.backup_inventory_mismatch"
    );
    let expected: BTreeMap<_, _> = inventory
        .iter()
        .map(|entry| (entry.archive_path.as_str(), entry))
        .collect();
    let mut names = BTreeMap::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).context("storage.backup_invalid")?;
        let name = if entry.is_dir() {
            entry
                .name()
                .strip_suffix('/')
                .context("storage.path_unsafe")?
        } else {
            entry.name()
        };
        insert_inventory_path(&mut names, name, entry.is_dir())?;
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            ensure!(
                kind == 0
                    || if entry.is_dir() {
                        kind == 0o040000
                    } else {
                        kind == 0o100000
                    },
                "storage.backup_link_unsupported"
            );
        }
        if has_manifest && name == "manifest.json" {
            ensure!(
                !entry.is_dir() && entry.size() <= MANIFEST_LIMIT,
                "storage.manifest_too_large"
            );
        } else {
            ensure!(
                expected
                    .get(name)
                    .is_some_and(|item| item.size_bytes == entry.size()
                        && item.is_directory == entry.is_dir()),
                "storage.backup_inventory_mismatch"
            );
        }
    }
    Ok(())
}

fn verify_zip_payloads(
    path: &Path,
    inventory: &[InventoryEntry],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut archive =
        ZipArchive::new(open_regular(path)?).context("storage.export_verify_failed")?;
    let has_manifest = archive.len() == inventory.len() + 1;
    validate_zip_entries(&mut archive, inventory, has_manifest)?;
    if has_manifest {
        let _ = read_manifest(&mut archive)?;
    }
    for entry in inventory {
        cancelled(cancel)?;
        if entry.is_directory {
            continue;
        }
        let mut input = archive
            .by_name(&entry.archive_path)
            .context("storage.backup_payload_missing")?;
        let (hash, size) =
            transfer_limited(&mut input, &mut std::io::sink(), entry.size_bytes, cancel)?;
        ensure!(
            hash == entry.hash && size == entry.size_bytes,
            "storage.export_verify_failed"
        );
    }
    Ok(())
}

fn inventory_total(inventory: &[InventoryEntry]) -> Result<u64> {
    inventory.iter().try_fold(0u64, |sum, entry| {
        sum.checked_add(entry.size_bytes)
            .context("storage.size_overflow")
    })
}

fn archive_path(root_id: &str, relative: &str) -> String {
    format!("payload/{}/{}", hex::encode(root_id.as_bytes()), relative)
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// Use a portable Windows-safe subset even on Unix. Refuse unrepresentable names
// instead of renaming archive contents or allowing NTFS ADS/device aliases.
fn safe_relative(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 32_000 && !name.contains('\\'),
        "storage.path_unsafe"
    );
    for component in name.split('/') {
        ensure!(
            !component.is_empty() && component != "." && component != "..",
            "storage.path_unsafe"
        );
        ensure!(
            component.encode_utf16().count() <= 255,
            "storage.path_unrepresentable"
        );
        ensure!(
            !component.ends_with(['.', ' '])
                && !component
                    .chars()
                    .any(|c| c.is_control() || "<>:\"|?*".contains(c)),
            "storage.path_unsafe"
        );
        let stem = component
            .split('.')
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let numbered_device = (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(
                &stem[3..],
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            );
        ensure!(
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
            ) && !numbered_device,
            "storage.path_unsafe"
        );
    }
    Ok(())
}

struct SeenPath {
    original: String,
    directory: bool,
    explicit: bool,
}

fn insert_inventory_path(
    names: &mut BTreeMap<String, SeenPath>,
    name: &str,
    directory: bool,
) -> Result<()> {
    safe_relative(name)?;
    let key = name.to_lowercase();
    // Register implicit parents as well: `Dir/a` plus `dir/b` must not silently
    // merge two original directories on a case-insensitive destination.
    for (index, _) in name.match_indices('/') {
        let prefix = &name[..index];
        let folded = prefix.to_lowercase();
        if let Some(existing) = names.get(&folded) {
            ensure!(
                existing.directory && existing.original == prefix,
                "storage.path_collision"
            );
        } else {
            names.insert(
                folded,
                SeenPath {
                    original: prefix.into(),
                    directory: true,
                    explicit: false,
                },
            );
        }
    }
    if let Some(existing) = names.get(&key) {
        ensure!(
            directory && existing.directory && !existing.explicit && existing.original == name,
            "storage.path_collision"
        );
    }
    names.insert(
        key,
        SeenPath {
            original: name.into(),
            directory,
            explicit: true,
        },
    );
    Ok(())
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "storage.cancelled");
    Ok(())
}

fn transfer<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    cancel: &AtomicBool,
) -> Result<(String, u64)> {
    transfer_limited(input, output, u64::MAX, cancel)
}

fn transfer_limited<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    maximum: u64,
    cancel: &AtomicBool,
) -> Result<(String, u64)> {
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut digest = Sha256::new();
    let mut size = 0u64;
    loop {
        cancelled(cancel)?;
        let read = input.read(&mut buffer).context("storage.read_failed")?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .context("storage.size_overflow")?;
        ensure!(size <= maximum, "storage.backup_size_mismatch");
        output
            .write_all(&buffer[..read])
            .context("storage.write_failed")?;
        digest.update(&buffer[..read]);
    }
    cancelled(cancel)?;
    Ok((hex::encode(digest.finalize()), size))
}

fn verify_disk(path: &Path, entry: &InventoryEntry, cancel: &AtomicBool) -> Result<()> {
    cancelled(cancel)?;
    if entry.is_directory {
        checked_directory(path)?;
        return Ok(());
    }
    let mut file = open_regular(path)?;
    ensure!(
        file.metadata().context("storage.metadata_failed")?.len() == entry.size_bytes,
        "storage.file_changed"
    );
    let (hash, size) = transfer_limited(&mut file, &mut std::io::sink(), entry.size_bytes, cancel)?;
    ensure!(
        hash == entry.hash && size == entry.size_bytes,
        "storage.file_changed"
    );
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

fn check_existing_ancestors(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "storage.path_not_absolute");
    let mut current = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, Component::ParentDir | Component::CurDir),
            "storage.path_unsafe"
        );
        current.push(component.as_os_str());
        // A Windows prefix alone (especially \\?\C:) is not an absolute directory.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(!is_link(&metadata), "storage.link_unsupported"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => bail!("storage.root_unavailable"),
        }
    }
    Ok(())
}

fn checked_regular_path(path: &Path) -> Result<PathBuf> {
    check_existing_ancestors(path)?;
    let metadata = fs::symlink_metadata(path).context("storage.file_missing")?;
    ensure!(
        metadata.is_file() && !is_link(&metadata),
        "storage.not_regular_file"
    );
    fs::canonicalize(path).context("storage.file_unavailable")
}

fn open_regular(path: &Path) -> Result<File> {
    let canonical = checked_regular_path(path)?;
    let file = File::open(canonical).context("storage.file_unavailable")?;
    ensure!(
        file.metadata()
            .context("storage.metadata_failed")?
            .is_file(),
        "storage.not_regular_file"
    );
    Ok(file)
}

fn checked_directory(path: &Path) -> Result<PathBuf> {
    check_existing_ancestors(path)?;
    let metadata = fs::symlink_metadata(path).context("storage.root_unavailable")?;
    ensure!(
        metadata.is_dir() && !is_link(&metadata),
        "storage.not_directory"
    );
    fs::canonicalize(path).context("storage.root_unavailable")
}

fn destination_path(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "storage.path_not_absolute");
    check_existing_ancestors(path)?;
    let filename = path.file_name().context("storage.invalid_destination")?;
    safe_relative(filename.to_str().context("storage.path_unrepresentable")?)?;
    let parent = checked_directory(path.parent().context("storage.invalid_destination")?)?;
    Ok(parent.join(filename))
}

fn new_file_destination(path: &Path) -> Result<PathBuf> {
    let destination = destination_path(path)?;
    ensure!(
        fs::symlink_metadata(&destination)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "storage.destination_exists"
    );
    Ok(destination)
}

fn fresh_directory_destination(path: &Path) -> Result<PathBuf> {
    let destination = destination_path(path)?;
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir() && !is_link(&metadata),
                "storage.destination_not_fresh"
            );
            ensure!(
                fs::read_dir(&destination)
                    .context("storage.destination_unavailable")?
                    .next()
                    .is_none(),
                "storage.destination_not_fresh"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => bail!("storage.destination_unavailable"),
    }
    Ok(destination)
}

fn create_parents(root: &Path, target: &Path) -> Result<()> {
    let parent = target.parent().context("storage.path_unsafe")?;
    ensure!(parent.starts_with(root), "storage.path_unsafe");
    check_existing_ancestors(parent)?;
    fs::create_dir_all(parent).context("storage.create_directory_failed")?;
    check_existing_ancestors(parent)?;
    Ok(())
}

fn publish_directory(temporary: &Path, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    #[cfg(unix)]
    flush_staging_directories(temporary, cancel)?;
    cancelled(cancel)?;
    fresh_directory_destination(destination)?;
    // remove_dir only succeeds for an empty directory; never recursively replace it.
    if destination.exists() {
        fs::remove_dir(destination).context("storage.destination_not_fresh")?;
    }
    fs::rename(temporary, destination).context("storage.publish_failed")?;
    sync_parent(
        destination
            .parent()
            .context("storage.invalid_destination")?,
    )
}

#[cfg(unix)]
fn flush_staging_directories(root: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        cancelled(cancel)?;
        for child in fs::read_dir(&directories[index]).context("storage.staging_read_failed")? {
            let path = child.context("storage.staging_read_failed")?.path();
            let metadata = fs::symlink_metadata(&path).context("storage.staging_read_failed")?;
            ensure!(!is_link(&metadata), "storage.link_unsupported");
            if metadata.is_dir() {
                directories.push(path);
            }
        }
        index += 1;
    }
    for directory in directories.iter().rev() {
        cancelled(cancel)?;
        sync_parent(directory)?;
    }
    Ok(())
}

fn publish_file_exclusive(temporary: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let existing: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let new: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // Flags 0: same-volume rename, and an existing target is never replaced.
        // This supports FAT/exFAT and network shares without requiring hard links.
        let moved = unsafe { MoveFileExW(existing.as_ptr(), new.as_ptr(), 0) };
        if moved == 0 {
            return Err(std::io::Error::last_os_error()).context("storage.export_publish_failed");
        }
    }
    #[cfg(not(windows))]
    {
        // Unlike ordinary Unix rename, link is exclusive even if another process
        // creates the destination after preflight. Both paths share the same volume.
        fs::hard_link(temporary, destination).context("storage.export_publish_failed")?;
        fs::remove_file(temporary).context("storage.export_cleanup_failed")?;
    }
    Ok(())
}

fn require_space(directory: &Path, bytes: u64) -> Result<()> {
    let free = fs2::available_space(directory).context("storage.space_unknown")?;
    let required = bytes
        .checked_add(1024 * 1024)
        .context("storage.size_overflow")?;
    ensure!(free >= required, "storage.low_space");
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .context("storage.directory_flush_failed")?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

struct Temporary {
    path: PathBuf,
    tree: bool,
    armed: bool,
}
impl Temporary {
    fn file(path: PathBuf) -> Self {
        Self {
            path,
            tree: false,
            armed: true,
        }
    }
    fn tree(path: PathBuf) -> Self {
        Self {
            path,
            tree: true,
            armed: true,
        }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.tree {
            let _ = remove_staging_tree(&self.path);
        } else {
            let _ = fs::remove_file(&self.path);
        }
    }
}

// Only invoked for the UUID directory created by this helper. Do not follow a link
// even in cleanup. Deliberately never use recursive deletion on an old library root.
fn remove_staging_tree(path: &Path) -> Result<()> {
    check_existing_ancestors(path)?;
    let metadata = fs::symlink_metadata(path).context("storage.staging_missing")?;
    ensure!(
        metadata.is_dir() && !is_link(&metadata),
        "storage.link_unsupported"
    );
    for child in fs::read_dir(path).context("storage.staging_read_failed")? {
        let child = child.context("storage.staging_read_failed")?;
        let metadata = fs::symlink_metadata(child.path()).context("storage.staging_read_failed")?;
        ensure!(!is_link(&metadata), "storage.link_unsupported");
        if metadata.is_dir() {
            remove_staging_tree(&child.path())?;
        } else if metadata.is_file() {
            fs::remove_file(child.path()).context("storage.staging_cleanup_failed")?;
        } else {
            bail!("storage.not_regular_file");
        }
    }
    fs::remove_dir(path).context("storage.staging_cleanup_failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("nna-portable-synthetic-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
        fn path(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
        fn file(&self, path: &str, bytes: &[u8]) -> PathBuf {
            let target = self.path(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, bytes).unwrap();
            target
        }
        fn payload(&self, root: &str, relative: &str, bytes: &[u8]) -> PayloadSource {
            PayloadSource {
                root_id: root.into(),
                relative_path: relative.into(),
                source_path: self.file(&format!("source/{root}/{relative}"), bytes),
                expected_hash: Some(hex::encode(Sha256::digest(bytes))),
                is_directory: false,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = remove_staging_tree(&self.0);
        }
    }
    fn active() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn full_backup_restores_files_images_and_unchanged_catalogue_semantics() {
        let fixture = Fixture::new();
        let payload = vec![
            fixture.payload(
                "assets",
                "Base/123/revision1/衣装.unitypackage",
                b"synthetic package",
            ),
            fixture.payload("media", "custom/image.png", b"synthetic image bytes"),
            fixture.payload("assets", "Base/123/revision1/empty.txt", b""),
        ];
        let catalogue = json!({"version":1,"records":{
            "overlay":[{"id":"123","name":"Human name","rejectedTags":["wrong"],"thumbnail":"custom/image.png"}],
            "acquisition":[{"id":"unresolved-source","productId":null,"source":"gift"}],
            "entitlement":[{"id":"ent","accountId":"a","fileIds":["f"],"manifestComplete":false}],
            "root":[{"id":"assets"}],"collection":[{"id":"c","productIds":["123"]}]
        }});
        let backup = fixture.path("full.zip");
        let summary = create_backup(&backup, catalogue.clone(), &payload, &active()).unwrap();
        assert_eq!(summary.file_count, 3);
        let restored_path = fixture.path("restored");
        let restored = restore_backup(&backup, &restored_path, &active()).unwrap();
        assert_eq!(restored.catalogue, catalogue);
        assert_eq!(restored.payload.len(), 3);
        for (source, entry) in payload.iter().zip(restored.payload) {
            assert_eq!(
                fs::read(restored_path.join(entry.archive_path)).unwrap(),
                fs::read(&source.source_path).unwrap()
            );
        }
        assert!(payload[0].source_path.exists());
    }

    #[test]
    fn empty_nested_directories_survive_backup_restore_and_migration() {
        let fixture = Fixture::new();
        let mut payload = vec![fixture.payload("root", "original/file.bin", b"good")];
        let empty = fixture.path("source/root/original/empty/nested");
        fs::create_dir_all(&empty).unwrap();
        payload.push(PayloadSource {
            root_id: "root".into(),
            relative_path: "original/empty/nested".into(),
            source_path: empty,
            expected_hash: None,
            is_directory: true,
        });
        // Explicit parent entries may come after children.
        payload.push(PayloadSource {
            root_id: "root".into(),
            relative_path: "original".into(),
            source_path: fixture.path("source/root/original"),
            expected_hash: None,
            is_directory: true,
        });
        create_backup(
            &fixture.path("dirs.zip"),
            json!({"version":1}),
            &payload,
            &active(),
        )
        .unwrap();
        let restored = restore_backup(
            &fixture.path("dirs.zip"),
            &fixture.path("restored"),
            &active(),
        )
        .unwrap();
        assert!(fixture
            .path(&format!("restored/{}", restored.payload[1].archive_path))
            .is_dir());
        let receipt = copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("moved"),
            &payload,
            &active(),
        )
        .unwrap();
        assert!(fixture.path("moved/original/empty/nested").is_dir());
        assert_eq!(cleanup_old_root(&receipt, &active()).unwrap(), 1);
        assert!(fixture.path("source/root/original/empty/nested").is_dir());
    }

    #[test]
    fn existing_destinations_and_nonempty_restore_roots_are_preserved() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("root", "file.bin", b"good")];
        let backup = fixture.file("existing.zip", b"keep this existing backup");
        assert!(create_backup(&backup, json!({}), &payload, &active()).is_err());
        assert_eq!(fs::read(&backup).unwrap(), b"keep this existing backup");
        create_backup(&fixture.path("new.zip"), json!({}), &payload, &active()).unwrap();
        fixture.file("occupied/unrelated.txt", b"keep");
        assert!(restore_backup(
            &fixture.path("new.zip"),
            &fixture.path("occupied"),
            &active()
        )
        .is_err());
        assert_eq!(
            fs::read(fixture.path("occupied/unrelated.txt")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn existing_empty_picker_directories_are_valid_restore_and_migration_targets() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("root", "file.bin", b"good")];
        create_backup(
            &fixture.path("full.zip"),
            json!({"version":1}),
            &payload,
            &active(),
        )
        .unwrap();
        fs::create_dir(fixture.path("restore-picker")).unwrap();
        let restored = restore_backup(
            &fixture.path("full.zip"),
            &fixture.path("restore-picker"),
            &active(),
        )
        .unwrap();
        assert_eq!(
            fs::read(
                fixture
                    .path("restore-picker")
                    .join(&restored.payload[0].archive_path)
            )
            .unwrap(),
            b"good"
        );
        fs::create_dir(fixture.path("move-picker")).unwrap();
        copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("move-picker"),
            &payload,
            &active(),
        )
        .unwrap();
        assert_eq!(
            fs::read(fixture.path("move-picker/file.bin")).unwrap(),
            b"good"
        );
        assert!(payload[0].source_path.exists());
    }

    #[test]
    fn exclusive_file_publication_does_not_replace_a_late_destination() {
        let fixture = Fixture::new();
        let temporary = fixture.file("temporary.part", b"new bytes");
        let destination = fixture.file("destination.zip", b"existing bytes");
        assert!(publish_file_exclusive(&temporary, &destination).is_err());
        assert_eq!(fs::read(temporary).unwrap(), b"new bytes");
        assert_eq!(fs::read(destination).unwrap(), b"existing bytes");
    }

    #[test]
    fn cancelled_operations_publish_nothing_and_preserve_sources() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("root", "file.bin", b"good")];
        let cancelled = AtomicBool::new(true);
        assert!(create_backup(
            &fixture.path("cancelled.zip"),
            json!({}),
            &payload,
            &cancelled
        )
        .is_err());
        assert!(export_zip(&fixture.path("cancelled-export.zip"), &payload, &cancelled).is_err());
        create_backup(&fixture.path("good.zip"), json!({}), &payload, &active()).unwrap();
        assert!(restore_backup(
            &fixture.path("good.zip"),
            &fixture.path("restore"),
            &cancelled
        )
        .is_err());
        assert!(copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("move"),
            &payload,
            &cancelled
        )
        .is_err());
        assert!(!fixture.path("cancelled.zip").exists());
        assert!(!fixture.path("restore").exists());
        assert!(!fixture.path("move").exists());
        assert_eq!(fs::read(&payload[0].source_path).unwrap(), b"good");
    }

    #[test]
    fn cancellation_during_stream_stops_at_bounded_chunk() {
        struct CancellingReader<'a> {
            flag: &'a AtomicBool,
            delivered: bool,
        }
        impl Read for CancellingReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.delivered {
                    panic!("read after cancellation");
                }
                self.delivered = true;
                buffer[..4].copy_from_slice(b"part");
                self.flag.store(true, Ordering::Release);
                Ok(4)
            }
        }
        let flag = active();
        let mut output = Vec::new();
        let result = transfer(
            &mut CancellingReader {
                flag: &flag,
                delivered: false,
            },
            &mut output,
            &flag,
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert_eq!(output, b"part");
    }

    #[test]
    fn unsafe_paths_case_aliases_and_file_directory_collisions_are_refused() {
        for path in [
            "../out",
            "/absolute",
            "C:/file",
            "foo\\bar",
            "x//y",
            "x/./y",
            "x/../y",
            "file:ads",
            "NUL.txt",
            "COM1",
            "LPT².bin",
            "trail.",
            "trail ",
            "a\0b",
        ] {
            assert!(safe_relative(path).is_err(), "accepted {path:?}");
        }
        assert!(safe_relative("日本語/原名.psd").is_ok());
        let mut names = BTreeMap::new();
        insert_inventory_path(&mut names, "a/File.bin", false).unwrap();
        assert!(insert_inventory_path(&mut names, "A/file.BIN", false).is_err());
        assert!(insert_inventory_path(&mut names, "A/another.bin", false).is_err());
        assert!(insert_inventory_path(&mut names, "a", false).is_err());
        assert!(insert_inventory_path(&mut names, "a/File.bin/child", false).is_err());
    }

    fn authored_backup(
        fixture: &Fixture,
        name: &str,
        manifest: Value,
        entries: &[(&str, &[u8])],
    ) -> PathBuf {
        let path = fixture.path(name);
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.finish().unwrap().sync_all().unwrap();
        path
    }

    fn one_file_manifest() -> Value {
        json!({"format":FORMAT,"version":1,"catalogue":{"version":1},"inventory":[{
            "rootId":"r","relativePath":"file.bin","archivePath":"payload/72/file.bin",
            "sizeBytes":4,"hash":hex::encode(Sha256::digest(b"good"))
        }]})
    }

    #[test]
    fn unsupported_version_is_refused_before_creating_restore_tree() {
        let fixture = Fixture::new();
        let backup = authored_backup(
            &fixture,
            "future.zip",
            json!({"format":FORMAT,"version":99}),
            &[],
        );
        let error = restore_backup(&backup, &fixture.path("restore"), &active()).unwrap_err();
        assert!(error.to_string().contains("version_unsupported"));
        assert!(!fixture.path("restore").exists());
    }

    #[test]
    fn missing_extra_tampered_and_unsafe_backup_payloads_do_not_publish() {
        let fixture = Fixture::new();
        let manifest = one_file_manifest();
        let missing = authored_backup(&fixture, "missing.zip", manifest.clone(), &[]);
        let extra = authored_backup(
            &fixture,
            "extra.zip",
            manifest.clone(),
            &[("payload/72/file.bin", b"good"), ("extra.bin", b"x")],
        );
        let tampered = authored_backup(
            &fixture,
            "tampered.zip",
            manifest.clone(),
            &[("payload/72/file.bin", b"evil")],
        );
        let unsafe_zip =
            authored_backup(&fixture, "unsafe.zip", manifest, &[("../out.bin", b"good")]);
        for backup in [missing, extra, tampered, unsafe_zip] {
            assert!(restore_backup(&backup, &fixture.path("restore"), &active()).is_err());
            assert!(!fixture.path("restore").exists());
        }
        assert!(!fixture.path("out.bin").exists());
        assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".nna-restore-")));
    }

    #[test]
    fn symbolic_link_zip_entries_are_refused_before_writing_payloads() {
        let fixture = Fixture::new();
        let path = fixture.path("link.zip");
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.add_symlink("payload/72/file.bin", "evil", options)
            .unwrap();
        zip.start_file("manifest.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&one_file_manifest()).unwrap())
            .unwrap();
        zip.finish().unwrap().sync_all().unwrap();
        let error = restore_backup(&path, &fixture.path("restore"), &active()).unwrap_err();
        assert!(error.to_string().contains("link_unsupported"));
        assert!(!fixture.path("restore").exists());
    }

    #[test]
    fn hash_mismatch_and_missing_source_abort_backup_without_partial_output() {
        let fixture = Fixture::new();
        let mut payload = vec![fixture.payload("root", "file.bin", b"good")];
        payload[0].expected_hash = Some("0".repeat(64));
        assert!(create_backup(&fixture.path("bad.zip"), json!({}), &payload, &active()).is_err());
        assert!(!fixture.path("bad.zip").exists());
        fs::remove_file(&payload[0].source_path).unwrap();
        assert!(export_zip(&fixture.path("missing.zip"), &payload, &active()).is_err());
        assert!(!fixture.path("missing.zip").exists());
    }

    #[test]
    fn space_preflight_refuses_impossible_allocation_without_creating_files() {
        let fixture = Fixture::new();
        let error = require_space(&fixture.0, u64::MAX - 1024 * 1024).unwrap_err();
        assert!(error.to_string().contains("low_space"));
        assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
    }

    #[test]
    fn zip_export_contains_only_requested_files_and_no_manifest() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("r", "package/file.psd", b"synthetic layered source")];
        let destination = fixture.path("selection.zip");
        export_zip(&destination, &payload, &active()).unwrap();
        let mut zip = ZipArchive::new(File::open(destination).unwrap()).unwrap();
        assert_eq!(zip.len(), 1);
        assert!(zip.by_name("manifest.json").is_err());
        let mut bytes = Vec::new();
        zip.by_name("payload/72/package/file.psd")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"synthetic layered source");
    }

    #[test]
    fn migration_copies_only_managed_files_and_cleanup_preserves_unrelated_files() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("root", "managed/file.bin", b"good")];
        fixture.file("source/root/user-unrelated.txt", b"leave alone");
        let receipt = copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("moved"),
            &payload,
            &active(),
        )
        .unwrap();
        let receipt: MigrationReceipt =
            serde_json::from_value(serde_json::to_value(receipt).unwrap()).unwrap();
        assert!(payload[0].source_path.exists());
        assert_eq!(
            fs::read(fixture.path("moved/managed/file.bin")).unwrap(),
            b"good"
        );
        assert!(!fixture.path("moved/user-unrelated.txt").exists());
        assert_eq!(cleanup_old_root(&receipt, &active()).unwrap(), 1);
        assert_eq!(cleanup_old_root(&receipt, &active()).unwrap(), 0);
        assert!(fixture.path("source/root").exists());
        assert_eq!(
            fs::read(fixture.path("source/root/user-unrelated.txt")).unwrap(),
            b"leave alone"
        );
    }

    #[test]
    fn cleanup_preflights_all_files_and_rejects_changed_old_or_destination() {
        let fixture = Fixture::new();
        let payload = vec![
            fixture.payload("root", "first.bin", b"one"),
            fixture.payload("root", "second.bin", b"two"),
        ];
        let receipt = copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("moved"),
            &payload,
            &active(),
        )
        .unwrap();
        fs::write(fixture.path("moved/second.bin"), b"bad").unwrap();
        assert!(cleanup_old_root(&receipt, &active()).is_err());
        assert!(payload[0].source_path.exists());
        fs::write(fixture.path("moved/second.bin"), b"two").unwrap();
        fs::write(&payload[1].source_path, b"new").unwrap();
        assert!(cleanup_old_root(&receipt, &active()).is_err());
        assert!(payload[0].source_path.exists());
        assert_eq!(fs::read(&payload[1].source_path).unwrap(), b"new");
    }

    #[test]
    fn migration_refuses_unmanaged_sources_overlaps_and_occupied_targets() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("root", "file.bin", b"good")];
        assert!(copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("source/root/inside"),
            &payload,
            &active()
        )
        .is_err());
        fixture.file("occupied/unrelated", b"keep");
        assert!(copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("occupied"),
            &payload,
            &active()
        )
        .is_err());
        let mut bad = payload.clone();
        bad[0].source_path = fixture.file("other/file.bin", b"good");
        assert!(copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("moved"),
            &bad,
            &active()
        )
        .is_err());
        assert!(payload[0].source_path.exists());
        assert!(!fixture.path("moved").exists());
    }

    #[test]
    fn cleanup_cancelled_keeps_all_old_files() {
        let fixture = Fixture::new();
        let payload = vec![fixture.payload("root", "file.bin", b"good")];
        let receipt = copy_verified_root(
            &fixture.path("source/root"),
            &fixture.path("moved"),
            &payload,
            &active(),
        )
        .unwrap();
        assert!(cleanup_old_root(&receipt, &AtomicBool::new(true)).is_err());
        assert!(payload[0].source_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_payload_and_parent_links_are_refused() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let source = fixture.file("source/file.bin", b"good");
        symlink(&source, fixture.path("linked.bin")).unwrap();
        assert!(open_regular(&fixture.path("linked.bin")).is_err());
        symlink(fixture.path("source"), fixture.path("alias")).unwrap();
        assert!(open_regular(&fixture.path("alias/file.bin")).is_err());
        fs::remove_file(fixture.path("linked.bin")).unwrap();
        fs::remove_file(fixture.path("alias")).unwrap();
    }
}
