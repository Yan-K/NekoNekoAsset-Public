//! Exact catalogue leaf selection and verified, App-owned Bridge payload copies.
use super::{archive, artifact_id, io, StorageService};
use anyhow::{anyhow, ensure, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};
use uuid::Uuid;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnityLeaf {
    pub leaf_ref: String,
    pub product_id: String,
    pub product_title: String,
    pub file_id: String,
    pub source_name: String,
    pub variant_name: String,
    pub revision: String,
    pub member_path: String,
    pub display_name: String,
    pub relative_path: String,
    pub kind: String,
    pub bytes: u64,
    pub sha256: String,
}

// Local source/snapshot paths are never IPC DTOs or diagnostic output.
pub struct UnitySnapshotEntry {
    pub entry_id: String,
    pub leaf: UnityLeaf,
    pub path: PathBuf,
}
pub struct UnitySnapshot {
    pub directory: PathBuf,
    pub entries: Vec<UnitySnapshotEntry>,
}
struct ResolvedLeaf {
    leaf: UnityLeaf,
    source: PathBuf,
    remote: Value,
    artifact: Arc<Value>,
    root: Value,
}

impl StorageService {
    pub async fn list_unity_inventory(&self, product_ids: &[String]) -> Result<Vec<UnityLeaf>> {
        let guard = self.operations.clone().read_owned().await;
        let service = self.clone();
        let products = product_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            Ok(service
                .resolve_unity_leaves(&products)?
                .into_iter()
                .map(|entry| entry.leaf)
                .collect())
        })
        .await?
    }

    pub async fn prepare_unity_snapshot(
        &self,
        request_id: &str,
        product_ids: &[String],
        leaf_refs: &[String],
        cancel: Arc<AtomicBool>,
    ) -> Result<UnitySnapshot> {
        let directory = self.unity_snapshot_directory(request_id)?;
        ensure!(!leaf_refs.is_empty(), "unity_selection_empty");
        let requested: HashSet<_> = leaf_refs.iter().collect();
        ensure!(
            requested.len() == leaf_refs.len(),
            "unity_selection_duplicate"
        );
        let guard = self.operations.clone().read_owned().await;
        let service = self.clone();
        let products = product_ids.to_vec();
        let refs = leaf_refs.to_vec();
        tokio::task::spawn_blocking(move || -> Result<UnitySnapshot> {
            // An aborted async waiter cannot release this gate while the copy
            // worker is still reading managed storage.
            let _guard = guard;
            io::check_cancel(&cancel)?;
            let mut available: HashMap<_, _> = service
                .resolve_unity_leaves(&products)?
                .into_iter()
                .map(|entry| (entry.leaf.leaf_ref.clone(), entry))
                .collect();
            let selected: Vec<_> = refs
                .iter()
                .map(|reference| {
                    available
                        .remove(reference)
                        .ok_or_else(|| anyhow!("unity_leaf_stale"))
                })
                .collect::<Result<_>>()?;
            require_distinct_paths(&selected)?;
            let required = selected.iter().try_fold(0u64, |total, entry| {
                total
                    .checked_add(entry.leaf.bytes)
                    .ok_or_else(|| anyhow!("unity_payload_too_large"))
            })?;
            let parent = directory
                .parent()
                .ok_or_else(|| anyhow!("unity_snapshot_invalid"))?;
            archive::validate_no_links(parent)?;
            fs::create_dir_all(parent)?;
            archive::validate_no_links(parent)?;
            io::space(parent, required)?;
            // A reused request ID never overwrites an existing request payload.
            fs::create_dir(&directory)?;
            let mut owned = SnapshotGuard {
                directory: directory.clone(),
                armed: true,
            };
            archive::validate_no_links(&directory)?;
            let mut entries = Vec::with_capacity(selected.len());
            for selected in &selected {
                io::check_cancel(&cancel)?;
                // Recheck the complete managed source chain at the read boundary.
                let source = service.unity_source_path(selected)?;
                ensure!(source == selected.source, "unity_leaf_stale");
                let entry_id = Uuid::new_v4().to_string();
                let target = io::safe_join(&directory, &entry_id)?;
                copy_snapshot(
                    &source,
                    &target,
                    &selected.leaf.sha256,
                    selected.leaf.bytes,
                    &cancel,
                )?;
                entries.push(UnitySnapshotEntry {
                    entry_id,
                    leaf: selected.leaf.clone(),
                    path: target,
                });
            }
            // The storage read gate permits ordinary download/source publication.
            // Do not publish a selection whose observed records changed during I/O.
            let current: HashMap<_, _> = service
                .resolve_unity_leaves(&products)?
                .into_iter()
                .map(|entry| (entry.leaf.leaf_ref.clone(), entry))
                .collect();
            for selected in &selected {
                let fresh = current
                    .get(&selected.leaf.leaf_ref)
                    .ok_or_else(|| anyhow!("unity_leaf_stale"))?;
                ensure!(
                    fresh.remote == selected.remote
                        && fresh.artifact == selected.artifact
                        && fresh.root == selected.root,
                    "unity_leaf_stale"
                );
            }
            io::check_cancel(&cancel)?;
            owned.armed = false;
            Ok(UnitySnapshot { directory, entries })
        })
        .await?
    }

    /// Caller supplies a request ID only, after protocol-approved receipt proof.
    pub fn cleanup_unity_snapshot(&self, request_id: &str) -> Result<()> {
        clean_snapshot(&self.unity_snapshot_directory(request_id)?)
    }

    fn unity_snapshot_directory(&self, request_id: &str) -> Result<PathBuf> {
        let id = Uuid::parse_str(request_id).map_err(|_| anyhow!("unity_request_invalid"))?;
        ensure!(id.to_string() == request_id, "unity_request_invalid");
        io::safe_join(
            &self.catalogue.data_dir(),
            &format!("unity/requests/{id}/snapshot"),
        )
    }

    fn resolve_unity_leaves(&self, products: &[String]) -> Result<Vec<ResolvedLeaf>> {
        let mut result = Vec::new();
        for product in products.iter().collect::<BTreeSet<_>>() {
            ensure!(
                self.catalogue.get("product", product)?.is_some(),
                "product_missing"
            );
            let title = self
                .catalogue
                .command("product_details", json!({"id":product}))?["title"]
                .as_str()
                .unwrap_or(product)
                .to_owned();
            let start = result.len();
            for remote in self.kept_files(product)? {
                let revision = remote["revision"].as_str().unwrap_or("");
                if revision.is_empty() || revision == "unknown" {
                    continue;
                }
                let Some(artifact) = self.catalogue.get("artifact", &artifact_id(&remote))? else {
                    continue;
                };
                if artifact["productId"].as_str() != Some(product.as_str())
                    || artifact["fileId"] != remote["id"]
                    || artifact["revision"] != remote["revision"]
                    || !matches!(
                        artifact["status"].as_str(),
                        Some("ready" | "downloaded" | "extractionFailed")
                    )
                {
                    continue;
                }
                let Some(root_id) = artifact["rootId"].as_str() else {
                    continue;
                };
                let Some(root) = self.catalogue.get("root", root_id)? else {
                    continue;
                };
                if self.root_path(&root).is_err() {
                    continue;
                }
                // Every member retains the same complete inventory for stale checks.
                let artifact = Arc::new(artifact);
                if artifact["originalAvailable"] == true {
                    let name = remote["sourceRelativePath"]
                        .as_str()
                        .or(remote["name"].as_str())
                        .unwrap_or("");
                    if let Ok(leaf) =
                        self.unity_leaf(&remote, &artifact, &root, "", name, &artifact)
                    {
                        result.push(leaf);
                    }
                }
                if artifact["status"] == "ready" {
                    for entry in artifact["extractedInventory"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if entry["isDirectory"] == true {
                            continue;
                        }
                        let Some(member) = entry["relativePath"].as_str() else {
                            continue;
                        };
                        if let Ok(leaf) =
                            self.unity_leaf(&remote, &artifact, &root, member, member, entry)
                        {
                            result.push(leaf);
                        }
                    }
                }
            }
            for entry in &mut result[start..] {
                entry.leaf.product_title = title.clone();
            }
        }
        result.sort_by(|a, b| {
            (&a.leaf.product_id, &a.leaf.file_id, &a.leaf.member_path).cmp(&(
                &b.leaf.product_id,
                &b.leaf.file_id,
                &b.leaf.member_path,
            ))
        });
        Ok(result)
    }

    fn unity_leaf(
        &self,
        remote: &Value,
        artifact: &Arc<Value>,
        root: &Value,
        member: &str,
        name: &str,
        fact: &Value,
    ) -> Result<ResolvedLeaf> {
        let relative_path = archive::validate_relative_path(name)?
            .to_str()
            .ok_or_else(|| anyhow!("unity_leaf_invalid"))?
            .replace('\\', "/");
        let bytes = fact["sizeBytes"]
            .as_u64()
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| anyhow!("unity_leaf_invalid"))?;
        let hash = io::required_string(fact, "hash")?;
        ensure!(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "unity_leaf_invalid"
        );
        let display_name = relative_path.rsplit('/').next().unwrap_or("").to_owned();
        let kind = if display_name.to_ascii_lowercase().ends_with(".unitypackage") {
            "unitypackage"
        } else {
            "file"
        }
        .to_owned();
        let leaf_ref = io::key(&serde_json::to_string(&json!([
            remote["productId"],
            remote["id"],
            remote["revision"],
            artifact["id"],
            root["id"],
            root["path"],
            artifact["relativePath"],
            artifact["extractedPath"],
            member,
            relative_path,
            hash,
            bytes
        ]))?);
        let mut selected = ResolvedLeaf {
            leaf: UnityLeaf {
                leaf_ref,
                product_id: io::required_string(remote, "productId")?.to_owned(),
                product_title: String::new(),
                file_id: io::required_string(remote, "id")?.to_owned(),
                source_name: remote["name"].as_str().unwrap_or("").to_owned(),
                variant_name: remote["variantName"].as_str().unwrap_or("").to_owned(),
                revision: io::required_string(remote, "revision")?.to_owned(),
                member_path: member.to_owned(),
                display_name,
                relative_path,
                kind,
                bytes,
                sha256: hash.to_owned(),
            },
            source: PathBuf::new(),
            remote: remote.clone(),
            artifact: Arc::clone(artifact),
            root: root.clone(),
        };
        selected.source = self.unity_source_path(&selected)?;
        io::ensure_regular(&selected.source)?;
        ensure!(
            fs::metadata(&selected.source)?.len() == bytes,
            "file_integrity_failed"
        );
        Ok(selected)
    }

    fn unity_source_path(&self, selected: &ResolvedLeaf) -> Result<PathBuf> {
        let root = self.root_path(&selected.root)?;
        if selected.leaf.member_path.is_empty() {
            io::safe_join(
                &root,
                io::required_string(&selected.artifact, "relativePath")?,
            )
        } else {
            let base = io::safe_join(
                &root,
                io::required_string(&selected.artifact, "extractedPath")?,
            )?;
            io::safe_join(&base, &selected.leaf.member_path)
        }
    }
}

// Hash the source buffers while copying, rather than rereading the entire copy.
// The sender hashes this immutable snapshot again as it sends it; the receiver
// checks the same digest before staging or allowing any project writes.
fn copy_snapshot(
    source: &Path,
    target: &Path,
    expected_hash: &str,
    expected_size: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    io::ensure_regular(source)?;
    let mut input = fs::File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    let mut hash = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        io::check_cancel(cancel)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("file_integrity_failed"))?;
        ensure!(copied <= expected_size, "file_integrity_failed");
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    ensure!(
        copied == expected_size && hex::encode(hash.finalize()) == expected_hash,
        "file_integrity_failed"
    );
    io::check_cancel(cancel)?;
    output.sync_all()?;
    Ok(())
}

fn require_distinct_paths(selected: &[ResolvedLeaf]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for entry in selected {
        let path = entry.leaf.relative_path.to_lowercase();
        ensure!(paths.insert(path), "unity_path_collision");
    }
    for path in &paths {
        for (index, _) in path.match_indices('/') {
            ensure!(!paths.contains(&path[..index]), "unity_path_collision");
        }
    }
    Ok(())
}

struct SnapshotGuard {
    directory: PathBuf,
    armed: bool,
}
impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = clean_snapshot(&self.directory);
        }
    }
}
fn clean_snapshot(directory: &Path) -> Result<()> {
    archive::validate_no_links(directory)?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("unity_snapshot_invalid"))?;
        let id = Uuid::parse_str(&name).map_err(|_| anyhow!("unity_snapshot_invalid"))?;
        ensure!(id.to_string() == name, "unity_snapshot_invalid");
        let path = io::safe_join(directory, &name)?;
        io::ensure_regular(&path)?;
        paths.push(path);
    }
    for path in paths {
        archive::validate_no_links(&path)?;
        io::ensure_regular(&path)?;
        fs::remove_file(path)?;
    }
    fs::remove_dir(directory)?;
    // The request directory may contain a root-owned receipt; retain it then.
    if let Some(parent) = directory.parent() {
        let _ = fs::remove_dir(parent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{booth::BoothService, catalogue::Catalogue, stores::StoreService};
    use std::sync::atomic::Ordering;

    fn fixture() -> (StorageService, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("nna-unity-snapshot-synthetic-{}", Uuid::new_v4()));
        let catalogue = Catalogue::open(&directory.join("data")).unwrap();
        let booth = Arc::new(BoothService::new(catalogue.clone()).unwrap());
        let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
        let service = StorageService::new(catalogue, stores).unwrap();
        let root = directory.join("library");
        fs::create_dir_all(&root).unwrap();
        service
            .catalogue
            .put(
                "root",
                "synthetic-root",
                json!({"id":"synthetic-root","path":root,"status":"online"}),
            )
            .unwrap();
        service.catalogue.put("product", "local:synthetic", json!({"id":"local:synthetic","provider":"local","custom":true,"title":"Authored Bridge fixture","localManifest":{"complete":true,"fileIds":["package","archive"],"directories":[]}})).unwrap();
        (service, root)
    }

    fn seed_file(
        service: &StorageService,
        root: &Path,
        id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Value {
        let remote = json!({"id":id,"productId":"local:synthetic","provider":"local","originKind":"local","name":name,"revision":"r1"});
        service
            .catalogue
            .put("remote_file", id, remote.clone())
            .unwrap();
        let relative = format!("assets/Authored--synthetic/original/{id}");
        let path = root.join(&relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        let (hash, size) = io::hash_file(&path).unwrap();
        let artifact = json!({"id":artifact_id(&remote),"productId":"local:synthetic","fileId":id,"revision":"r1","rootId":"synthetic-root","productDirectory":"assets/Authored--synthetic","relativePath":relative,"hash":hash,"sizeBytes":size,"status":"ready","originalAvailable":true});
        service
            .catalogue
            .put(
                "artifact",
                artifact["id"].as_str().unwrap(),
                artifact.clone(),
            )
            .unwrap();
        artifact
    }

    #[tokio::test]
    async fn unity_snapshot_binds_original_and_inventory_member_and_releases_gate() {
        let (service, root) = fixture();
        let original = seed_file(
            &service,
            &root,
            "package",
            "Packages/Example.unitypackage",
            b"authored intact package",
        );
        let mut archive = seed_file(
            &service,
            &root,
            "archive",
            "Example.zip",
            b"authored retained archive",
        );
        let base = "assets/Authored--synthetic/extracted/Example";
        fs::create_dir_all(root.join(base).join("Models")).unwrap();
        fs::create_dir_all(root.join(base).join("Empty")).unwrap();
        fs::write(
            root.join(base).join("Models/example.fbx"),
            b"authored model",
        )
        .unwrap();
        let (hash, size) = io::hash_file(&root.join(base).join("Models/example.fbx")).unwrap();
        archive["extractedPath"] = json!(base);
        archive["extractedInventory"] = json!([
            {"relativePath":"Models/example.fbx","hash":hash,"sizeBytes":size},
            {"relativePath":"Empty","hash":"","sizeBytes":0,"isDirectory":true}
        ]);
        service
            .catalogue
            .put("artifact", archive["id"].as_str().unwrap(), archive.clone())
            .unwrap();
        let products = vec!["local:synthetic".to_owned()];
        let leaves = service.list_unity_inventory(&products).await.unwrap();
        assert_eq!(leaves.len(), 3, "directories are not transferable leaves");
        let package = leaves
            .iter()
            .find(|leaf| leaf.file_id == "package")
            .unwrap();
        assert_eq!(package.kind, "unitypackage");
        assert_eq!(package.member_path, "");
        assert_eq!(package.relative_path, "Packages/Example.unitypackage");
        let member = leaves
            .iter()
            .find(|leaf| !leaf.member_path.is_empty())
            .unwrap();
        let retained = leaves
            .iter()
            .find(|leaf| leaf.file_id == "archive" && leaf.member_path.is_empty())
            .unwrap();
        assert_ne!(member.leaf_ref, retained.leaf_ref);
        assert_eq!(member.relative_path, "Models/example.fbx");
        let request = Uuid::new_v4().to_string();
        let snapshot = service
            .prepare_unity_snapshot(
                &request,
                &products,
                &[package.leaf_ref.clone(), member.leaf_ref.clone()],
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.entries.len(), 2);
        assert!(
            service.operations.try_write().is_ok(),
            "snapshot retains no operation guard"
        );
        for entry in &snapshot.entries {
            assert_eq!(entry.path.parent(), Some(snapshot.directory.as_path()));
            assert_eq!(
                entry.path.file_name().unwrap().to_str().unwrap(),
                entry.entry_id
            );
            io::verify(&entry.path, &entry.leaf.sha256, entry.leaf.bytes).unwrap();
        }
        assert_eq!(
            fs::read(&snapshot.entries[0].path).unwrap(),
            b"authored intact package"
        );
        // The immutable copy remains unchanged when its source changes later.
        fs::write(
            root.join(original["relativePath"].as_str().unwrap()),
            b"changed after snapshot",
        )
        .unwrap();
        assert_eq!(
            fs::read(&snapshot.entries[0].path).unwrap(),
            b"authored intact package"
        );
        // A root-owned receipt is outside cleanup's exact owned snapshot.
        let receipt = snapshot.directory.parent().unwrap().join("receipt.json");
        fs::write(&receipt, b"authored receipt").unwrap();
        service.cleanup_unity_snapshot(&request).unwrap();
        assert!(!snapshot.directory.exists());
        assert!(receipt.exists());
        assert!(root.join(base).join("Models/example.fbx").exists());
        service.cleanup_unity_snapshot(&request).unwrap();
        assert!(service.cleanup_unity_snapshot("../library").is_err());
        // A later corrupt member must also remove the already verified first
        // snapshot copy, while retaining both registered managed inputs.
        fs::write(
            root.join(original["relativePath"].as_str().unwrap()),
            b"authored intact package",
        )
        .unwrap();
        fs::write(
            root.join(base).join("Models/example.fbx"),
            b"damaged model!",
        )
        .unwrap();
        let failed_request = Uuid::new_v4().to_string();
        let failed = service
            .prepare_unity_snapshot(
                &failed_request,
                &products,
                &[package.leaf_ref.clone(), member.leaf_ref.clone()],
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_eq!(failed.err().unwrap().to_string(), "file_integrity_failed");
        assert!(!service
            .unity_snapshot_directory(&failed_request)
            .unwrap()
            .exists());
        assert!(root
            .join(original["relativePath"].as_str().unwrap())
            .exists());
        assert_eq!(
            fs::read(root.join(base).join("Models/example.fbx")).unwrap(),
            b"damaged model!"
        );
    }

    #[tokio::test]
    async fn unity_inventory_offers_store_files_kept_outside_the_current_list() {
        let (service, root) = fixture();
        let c = &service.catalogue;
        c.put("product", "900001", json!({"id":"900001","store":"BOOTH","title":"Authored store product"})).unwrap();
        c.put("entitlement", "grant", json!({"id":"grant","productId":"900001","accountId":"authored-account","fileIds":["current"],"manifestComplete":true})).unwrap();
        // The store dropped `dropped` after its download; the user added the third.
        for (id, origin) in [("current", None), ("dropped", None), ("local-file:added", Some("local"))] {
            let mut remote = json!({"id":id,"productId":"900001","name":format!("{}.unitypackage", id.replace(':', "-")),"revision":"r1"});
            if let Some(origin) = origin {
                remote["originKind"] = json!(origin);
            }
            c.put("remote_file", id, remote.clone()).unwrap();
            let relative = format!("assets/Authored--900001/{}.unitypackage", id.replace(':', "-"));
            let path = root.join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("authored {id}")).unwrap();
            let (hash, size) = io::hash_file(&path).unwrap();
            let artifact = json!({"id":artifact_id(&remote),"productId":"900001","fileId":id,"revision":"r1","rootId":"synthetic-root","productDirectory":"assets/Authored--900001","relativePath":relative,"hash":hash,"sizeBytes":size,"status":"ready","originalAvailable":true});
            c.put("artifact", artifact["id"].as_str().unwrap(), artifact.clone()).unwrap();
        }
        // A dropped file that was never downloaded has nothing to import.
        c.put("remote_file", "gone", json!({"id":"gone","productId":"900001","name":"gone.unitypackage","revision":"r1"})).unwrap();
        let leaves = service.list_unity_inventory(&["900001".to_owned()]).await.unwrap();
        let mut ids: Vec<_> = leaves.iter().map(|leaf| leaf.file_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["current", "dropped", "local-file:added"]);
    }

    #[tokio::test]
    async fn unity_snapshot_rejects_stale_corrupt_collision_and_cancel_preserving_sources() {
        let (service, root) = fixture();
        let mut original = seed_file(
            &service,
            &root,
            "package",
            "Example.unitypackage",
            b"authored payload",
        );
        let products = vec!["local:synthetic".to_owned()];
        let old = service
            .list_unity_inventory(&products)
            .await
            .unwrap()
            .remove(0);
        original["hash"] = json!(io::key("changed binding"));
        service
            .catalogue
            .put(
                "artifact",
                original["id"].as_str().unwrap(),
                original.clone(),
            )
            .unwrap();
        let stale_request = Uuid::new_v4().to_string();
        let stale = service
            .prepare_unity_snapshot(
                &stale_request,
                &products,
                &[old.leaf_ref],
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_eq!(stale.err().unwrap().to_string(), "unity_leaf_stale");
        assert!(!service
            .unity_snapshot_directory(&stale_request)
            .unwrap()
            .exists());
        let original = seed_file(
            &service,
            &root,
            "package",
            "Example.unitypackage",
            b"authored payload",
        );
        let good = service
            .list_unity_inventory(&products)
            .await
            .unwrap()
            .remove(0);
        let source = root.join(original["relativePath"].as_str().unwrap());
        fs::write(&source, b"damaged! payload").unwrap(); // same length; listing alone cannot prove integrity
        let corrupt_request = Uuid::new_v4().to_string();
        let corrupt = service
            .prepare_unity_snapshot(
                &corrupt_request,
                &products,
                &[good.leaf_ref.clone()],
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_eq!(corrupt.err().unwrap().to_string(), "file_integrity_failed");
        assert!(!service
            .unity_snapshot_directory(&corrupt_request)
            .unwrap()
            .exists());
        assert_eq!(fs::read(&source).unwrap(), b"damaged! payload");
        seed_file(
            &service,
            &root,
            "package",
            "Example.unitypackage",
            b"authored payload",
        );
        seed_file(
            &service,
            &root,
            "archive",
            "example.UNITYPACKAGE",
            b"authored second payload",
        );
        let leaves = service.list_unity_inventory(&products).await.unwrap();
        let collision = service
            .prepare_unity_snapshot(
                &Uuid::new_v4().to_string(),
                &products,
                &leaves
                    .iter()
                    .map(|leaf| leaf.leaf_ref.clone())
                    .collect::<Vec<_>>(),
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_eq!(collision.err().unwrap().to_string(), "unity_path_collision");
        let cancel = Arc::new(AtomicBool::new(false));
        // Pause at the existing storage gate and cancel before copying starts.
        let gate = service.operations.write().await;
        let worker = service.clone();
        let request = Uuid::new_v4().to_string();
        let requested = request.clone();
        let cancelling = cancel.clone();
        let task = tokio::spawn(async move {
            worker
                .prepare_unity_snapshot(&requested, &products, &[good.leaf_ref], cancelling)
                .await
        });
        tokio::task::yield_now().await;
        cancel.store(true, Ordering::Release);
        drop(gate);
        assert_eq!(task.await.unwrap().err().unwrap().to_string(), "cancelled");
        assert!(!service.unity_snapshot_directory(&request).unwrap().exists());
        assert!(source.exists());
        assert!(service.operations.try_write().is_ok());
    }

    #[test]
    fn unity_snapshot_collision_detects_file_parent_even_with_intervening_name() {
        let entry = |relative: &str| ResolvedLeaf {
            leaf: UnityLeaf {
                leaf_ref: String::new(),
                product_id: String::new(),
                product_title: String::new(),
                file_id: String::new(),
                source_name: String::new(),
                variant_name: String::new(),
                revision: String::new(),
                member_path: String::new(),
                display_name: String::new(),
                relative_path: relative.into(),
                kind: "file".into(),
                bytes: 0,
                sha256: String::new(),
            },
            source: PathBuf::new(),
            remote: Value::Null,
            artifact: Arc::new(Value::Null),
            root: Value::Null,
        };
        assert!(require_distinct_paths(&[
            entry("Model"),
            entry("model-a.txt"),
            entry("model/mesh.fbx")
        ])
        .is_err());
        assert!(require_distinct_paths(&[entry("A/mesh.fbx"), entry("B/mesh.fbx")]).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unity_snapshot_never_reads_linked_managed_leaf() {
        let (service, root) = fixture();
        let original = seed_file(
            &service,
            &root,
            "package",
            "Example.unitypackage",
            b"authored payload",
        );
        let products = vec!["local:synthetic".into()];
        let leaf = service
            .list_unity_inventory(&products)
            .await
            .unwrap()
            .remove(0);
        let source = root.join(original["relativePath"].as_str().unwrap());
        let external = root.parent().unwrap().join("external-authored.bin");
        fs::write(&external, b"authored payload").unwrap();
        fs::remove_file(&source).unwrap();
        std::os::unix::fs::symlink(&external, &source).unwrap();
        assert!(service
            .list_unity_inventory(&products)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            service
                .prepare_unity_snapshot(
                    &Uuid::new_v4().to_string(),
                    &products,
                    &[leaf.leaf_ref],
                    Arc::new(AtomicBool::new(false))
                )
                .await
                .err()
                .unwrap()
                .to_string(),
            "unity_leaf_stale"
        );
        assert_eq!(fs::read(external).unwrap(), b"authored payload");
    }
}
