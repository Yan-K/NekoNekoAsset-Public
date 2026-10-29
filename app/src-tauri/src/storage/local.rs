//! Device-only local import plans; input originals are never moved or removed.
use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    time::UNIX_EPOCH,
};

const JOB_TYPE: &str = "storage:import_local";
const ENTRY_LIMIT: usize = 1_000_000;

#[derive(Clone, Serialize, Deserialize)]
struct LocalFile {
    source: PathBuf,
    relative: String,
    size: u64,
    modified: u128,
}
#[derive(Clone, Serialize, Deserialize)]
struct LocalGroup {
    product_id: String,
    title: String,
    prefix: String,
    files: Vec<LocalFile>,
    directories: Vec<String>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    remove_file_ids: Vec<String>,
    #[serde(default)]
    membership: bool,
    #[serde(default)]
    expected_mutation: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct LocalPlan {
    draft_id: String,
    groups: Vec<LocalGroup>,
    metadata: Value,
}

fn signature(path: &Path) -> Result<(u64, u128)> {
    archive::validate_no_links(path)?;
    io::ensure_regular(path)?;
    let meta = fs::metadata(path)?;
    Ok((
        meta.len(),
        meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
    ))
}
fn path_name(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("archive_unsafe_path"))?;
    archive::validate_relative_path(name)?;
    Ok(name.to_owned())
}
fn enumerate(path: &Path, relative: &str, group: &mut LocalGroup) -> Result<()> {
    archive::validate_no_links(path)?;
    archive::validate_relative_path(relative)?;
    ensure!(
        group.files.len() + group.directories.len() < ENTRY_LIMIT,
        "local_import_too_many_files"
    );
    if path.is_dir() {
        group.directories.push(relative.to_owned());
        let mut children = fs::read_dir(path)?
            .map(|entry| Ok(entry?.path()))
            .collect::<Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            enumerate(&child, &format!("{relative}/{}", path_name(&child)?), group)?;
        }
    } else {
        let (size, modified) = signature(path)?;
        group.files.push(LocalFile {
            source: path.to_owned(),
            relative: relative.to_owned(),
            size,
            modified,
        });
    }
    Ok(())
}
fn path_contains(parent: &Path, child: &Path) -> bool {
    #[cfg(windows)]
    {
        let parent: Vec<_> = parent
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        let child: Vec<_> = child
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        child.starts_with(&parent)
    }
    #[cfg(not(windows))]
    {
        child.starts_with(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, StorageService) {
        let path =
            std::env::temp_dir().join(format!("nna-local-015-authored-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path.join("data")).unwrap();
        let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
        let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
        let service = StorageService::new(catalogue, stores).unwrap();
        fs::create_dir_all(path.join("library")).unwrap();
        service
            .catalogue
            .put(
                "root",
                "authored-root",
                json!({"id":"authored-root","path":path.join("library"),"status":"online"}),
            )
            .unwrap();
        (path, service)
    }

    #[test]
    fn local_loose_names_and_batch_metadata_are_independent() {
        let (path, service) = fixture();
        fs::create_dir_all(path.join("A")).unwrap();
        fs::create_dir_all(path.join("B")).unwrap();
        fs::write(path.join("A/readme.txt"), b"authored A").unwrap();
        fs::write(path.join("B/readme.txt"), b"authored B").unwrap();
        let result=service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4(),"sources":[path.join("A/readme.txt"),path.join("B/readme.txt")],"name":"Loose","rootId":"authored-root"})).unwrap();
        let job = service
            .catalogue
            .get("job", result["jobId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let plan: LocalPlan = serde_json::from_value(job["localPlan"].clone()).unwrap();
        assert_eq!(
            plan.groups[0]
                .files
                .iter()
                .map(|f| f.relative.as_str())
                .collect::<Vec<_>>(),
            vec!["readme.txt", "readme (2).txt"]
        );
        assert!(plan.groups[0].directories.is_empty());
        let result=service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4(),"mode":"batch","sources":[path.join("B"),path.join("A")],"rootId":"authored-root","products":[{"source":path.join("A"),"name":"Alpha","category":"Tool","tags":["A"],"sourceUrl":"https://booth.pm/en/items/123"},{"source":path.join("B"),"name":"Beta","category":"Shader","tags":["B"]}]})).unwrap();
        let mut job = service
            .catalogue
            .get("job", result["jobId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        service
            .run_local_import(&mut job, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        let products: Vec<_> = strings(&job["productIds"])
            .iter()
            .map(|id| service.catalogue.get("product", id).unwrap().unwrap())
            .collect();
        assert_eq!(products[0]["title"], "Alpha");
        assert_eq!(products[0]["category"], "Tool");
        assert_eq!(products[0]["tags"], json!(["A"]));
        assert_eq!(products[1]["title"], "Beta");
        assert_eq!(products[1]["category"], "Shader");
        assert_eq!(products[1]["tags"], json!(["B"]));
        drop(service);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn local_membership_add_remove_preserves_metadata_originals_and_modified_bytes() {
        let (path, service) = fixture();
        fs::write(path.join("one.txt"), b"authored one").unwrap();
        fs::write(path.join("two.txt"), b"authored two").unwrap();
        let queued = service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4(),"sources":[path.join("one.txt")],"name":"Authored local","category":"Tool","rootId":"authored-root"})).unwrap();
        let id = queued["productIds"][0].as_str().unwrap();
        let mut job = service
            .catalogue
            .get("job", queued["jobId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        service
            .run_local_import(&mut job, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        job["state"] = json!("completed");
        service
            .catalogue
            .put("job", job["id"].as_str().unwrap(), job.clone())
            .unwrap();
        let original = service.catalogue.get("product", id).unwrap().unwrap();
        let first = service.catalogue.related("artifact", id).unwrap()[0].clone();
        let remove = first["fileId"].as_str().unwrap();
        let args = json!({"draftId":uuid::Uuid::new_v4(),"productId":id,"rootId":"authored-root","addSources":[path.join("two.txt")],"removeFileIds":[remove]});
        let result = service.prepare_local_membership(&args).unwrap();
        assert_eq!(
            service.prepare_local_membership(&args).unwrap()["jobId"],
            result["jobId"]
        );
        let mut mutation = service
            .catalogue
            .get("job", result["jobId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        service
            .run_local_import(&mut mutation, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        mutation["state"] = json!("completed");
        service
            .catalogue
            .put("job", mutation["id"].as_str().unwrap(), mutation.clone())
            .unwrap();
        let product = service.catalogue.get("product", id).unwrap().unwrap();
        assert_eq!(product["title"], original["title"]);
        assert_eq!(product["category"], original["category"]);
        assert_eq!(strings(&product["localManifest"]["fileIds"]).len(), 1);
        assert!(!path
            .join("library")
            .join(first["relativePath"].as_str().unwrap())
            .exists());
        assert!(path.join("one.txt").exists());
        assert!(path.join("two.txt").exists());
        let remaining = service.catalogue.related("artifact", id).unwrap()[0].clone();
        let managed = path
            .join("library")
            .join(remaining["relativePath"].as_str().unwrap());
        fs::remove_file(&managed).unwrap();
        let repair_id = service
            .retry_local_product_locked(id, &service.catalogue.list("job").unwrap())
            .unwrap()
            .unwrap();
        assert_ne!(repair_id, queued["jobId"].as_str().unwrap());
        let mut repair = service.catalogue.get("job", &repair_id).unwrap().unwrap();
        service
            .run_local_import(&mut repair, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        repair["state"] = json!("completed");
        service.catalogue.put("job", &repair_id, repair).unwrap();
        assert!(managed.exists());
        assert_eq!(
            strings(
                &service.catalogue.get("product", id).unwrap().unwrap()["localManifest"]["fileIds"]
            )
            .len(),
            1
        );
        assert!(service
            .catalogue
            .get("remote_file", remove)
            .unwrap()
            .is_none());
        assert!(service
            .run_local_import(&mut job, Path::new("unused"), &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("local_membership_changed"));
        assert!(service
            .run_local_import(&mut mutation, Path::new("unused"), &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("local_membership_changed"));
        fs::write(&managed, b"user edited").unwrap();
        let result=service.prepare_local_membership(&json!({"draftId":uuid::Uuid::new_v4(),"productId":id,"rootId":"authored-root","removeFileIds":[remaining["fileId"]]})).unwrap();
        let mut job = service
            .catalogue
            .get("job", result["jobId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        service
            .run_local_import(&mut job, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(fs::read(&managed).unwrap(), b"user edited");
        assert_eq!(
            service.catalogue.get("product", id).unwrap().unwrap()["localManifest"]["fileIds"],
            json!([])
        );
        service
            .catalogue
            .put(
                "product",
                "online",
                json!({"id":"online","provider":"booth","title":"Authored online"}),
            )
            .unwrap();
        assert!(service.prepare_local_membership(&json!({"draftId":uuid::Uuid::new_v4(),"productId":"online","removeFileIds":["anything"]})).unwrap_err().to_string().contains("local_product_required"));
        drop(service);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn initial_category_distinguishes_default_from_explicit_choices_and_preserves_overlays() {
        let mut default_overlay = json!({"id":"local:synthetic","note":"keep"});
        apply_initial_category(&mut default_overlay, &json!({"category":"NonVRChat"}));
        assert!(default_overlay.get("category").is_none());
        assert_eq!(default_overlay["note"], "keep");

        for metadata in [
            json!({"category":"NonVRChat","categoryManual":true}),
            json!({"category":"Tool"}),
        ] {
            let mut overlay = json!({"id":"local:synthetic"});
            apply_initial_category(&mut overlay, &metadata);
            assert_eq!(overlay["category"], metadata["category"]);
            overlay["category"] = json!("Shader");
            apply_initial_category(&mut overlay, &metadata);
            assert_eq!(overlay["category"], "Shader");
        }

        let mut automatic_overlay = json!({"id":"local:synthetic"});
        apply_initial_category(
            &mut automatic_overlay,
            &json!({"category":"Tool","categoryManual":false}),
        );
        assert!(automatic_overlay.get("category").is_none());
    }
}
fn checkpoint_id(draft: &str, group: &LocalGroup, file: &LocalFile) -> String {
    format!(
        "local-import-file:{draft}:{}",
        io::key(&format!("{}\0{}", group.product_id, file.relative))
    )
}
fn file_id(product: &str, relative: &str) -> String {
    format!("local-file:{}", io::key(&format!("{product}\0{relative}")))
}
fn category_manual(metadata: &Value) -> bool {
    metadata["categoryManual"]
        .as_bool()
        .unwrap_or_else(|| metadata["category"].as_str().unwrap_or("NonVRChat") != "NonVRChat")
}
fn apply_initial_category(overlay: &mut Value, metadata: &Value) {
    if category_manual(metadata) && overlay.get("category").is_none() {
        overlay["category"] = metadata["category"].clone();
    }
}
fn local_error(error: &anyhow::Error) -> &'static str {
    for code in [
        "no_import_files",
        "local_source_changed",
        "local_import_too_many_files",
        "local_batch_requires_folder_or_zip",
        "local_membership_changed",
        "unsafe_link",
    ] {
        if error.to_string().contains(code) {
            return code;
        }
    }
    io::stable_error(error)
}

impl StorageService {
    fn cleanup_removed_local(&self, group: &LocalGroup) -> Result<()> {
        let artifacts = self.catalogue.list("artifact")?;
        for marker in self.catalogue.related("acquisition", &group.product_id)? {
            if marker["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("local-import-fingerprint:"))
            {
                self.catalogue
                    .delete("acquisition", io::required_string(&marker, "id")?)?;
            }
        }
        for removed in &group.remove_file_ids {
            for artifact in artifacts
                .iter()
                .filter(|a| a["productId"] == group.product_id && a["fileId"] == *removed)
            {
                let root = self.root(artifact["rootId"].as_str())?;
                let root_path = self.root_path(&root)?;
                let relative = io::required_string(artifact, "relativePath")?;
                // Same-path references, user edits and unverifiable outputs retain their bytes.
                let shared = artifacts.iter().any(|a| {
                    a["id"] != artifact["id"]
                        && a["rootId"] == artifact["rootId"]
                        && (a["relativePath"] == relative
                            || a["extractedPath"]
                                .as_str()
                                .is_some_and(|p| path_contains(Path::new(p), Path::new(relative))))
                });
                let path = io::safe_join(&root_path, relative)?;
                if !shared
                    && io::verify(
                        &path,
                        artifact["hash"].as_str().unwrap_or(""),
                        artifact["sizeBytes"].as_u64().unwrap_or(u64::MAX),
                    )
                    .is_ok()
                {
                    fs::remove_file(&path)?;
                }
                if let Some(extracted) = artifact["extractedPath"].as_str() {
                    if !artifacts.iter().any(|a| {
                        a["id"] != artifact["id"]
                            && a["rootId"] == artifact["rootId"]
                            && (a["extractedPath"].as_str().is_some_and(|p| {
                                path_contains(Path::new(p), Path::new(extracted))
                                    || path_contains(Path::new(extracted), Path::new(p))
                            }) || a["relativePath"]
                                .as_str()
                                .is_some_and(|p| path_contains(Path::new(extracted), Path::new(p))))
                    }) {
                        let target = io::safe_join(&root_path, extracted)?;
                        let entries: Vec<archive::VerifiedEntry> =
                            serde_json::from_value(artifact["extractedInventory"].clone())
                                .unwrap_or_default();
                        // Every owned file must still match; never recurse into unrelated output.
                        if !entries.is_empty()
                            && entries
                                .iter()
                                .filter(|entry| !entry.is_directory)
                                .all(|entry| {
                                    io::safe_join(&target, &entry.relative_path)
                                        .ok()
                                        .is_some_and(|p| {
                                            io::verify(&p, &entry.hash, entry.size_bytes).is_ok()
                                        })
                                })
                        {
                            for entry in entries.iter().filter(|entry| !entry.is_directory) {
                                fs::remove_file(io::safe_join(&target, &entry.relative_path)?)?;
                            }
                            let mut dirs: Vec<_> =
                                entries.iter().filter(|e| e.is_directory).collect();
                            dirs.sort_by_key(|e| std::cmp::Reverse(e.relative_path.len()));
                            for entry in dirs {
                                let _ =
                                    fs::remove_dir(io::safe_join(&target, &entry.relative_path)?);
                            }
                            let _ = fs::remove_dir(&target);
                        }
                    }
                }
                self.catalogue
                    .delete("artifact", io::required_string(artifact, "id")?)?;
            }
            self.catalogue.delete("remote_file", removed)?;
        }
        Ok(())
    }
    pub(super) async fn mutate_local_files(&self, args: &Value) -> Result<Value> {
        let _operation = self
            .operations
            .try_read()
            .map_err(|_| anyhow::anyhow!("storage_busy"))?;
        let _gate = self.queue_gate.lock().await;
        let service = self.clone();
        let args = args.clone();
        tokio::task::spawn_blocking(move || service.prepare_local_membership(&args)).await?
    }

    fn prepare_local_membership(&self, args: &Value) -> Result<Value> {
        let product_id = io::required_string(args, "productId")?;
        let product = self
            .catalogue
            .get("product", product_id)?
            .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
        ensure!(product["provider"] == "local", "local_product_required");
        let draft = uuid::Uuid::parse_str(io::required_string(args, "draftId")?)?.to_string();
        let id = format!("local-import:{draft}");
        let fingerprint = io::key(&serde_json::to_string(args)?);
        if let Some(job) = self.catalogue.get("job", &id)? {
            ensure!(
                job["requestFingerprint"] == fingerprint,
                "local_draft_changed"
            );
            return Ok(json!({"jobId":id,"productIds":[product_id],"state":job["state"]}));
        }
        ensure!(
            !self.catalogue.list("job")?.iter().any(|job| matches!(
                job["state"].as_str(),
                Some("queued" | "running")
            ) && strings(&job["productIds"])
                .contains(&product_id.to_owned())),
            "storage_busy"
        );
        let removes = strings(&args["removeFileIds"]);
        for key in ["removeFileIds", "addSources"] {
            ensure!(
                args.get(key).is_none()
                    || args[key]
                        .as_array()
                        .is_some_and(|values| values.iter().all(Value::is_string)),
                "invalid_arguments"
            );
        }
        let current = strings(&product["localManifest"]["fileIds"]);
        ensure!(
            removes.iter().all(|id| current.contains(id)),
            "file_missing"
        );
        let additions = strings(&args["addSources"]);
        ensure!(
            !removes.is_empty() || !additions.is_empty(),
            "invalid_arguments"
        );
        let artifacts = self.catalogue.related("artifact", product_id)?;
        let root_id = args["rootId"]
            .as_str()
            .or_else(|| artifacts.iter().find_map(|a| a["rootId"].as_str()));
        let root = self.root(root_id)?;
        let root_path = self.root_path(&root)?;
        let prefix = artifacts
            .iter()
            .find(|a| a["rootId"] == root["id"])
            .and_then(|a| a["productDirectory"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "assets/{}",
                    io::readable_component(
                        product["title"].as_str().unwrap_or("Local"),
                        product_id
                    )
                )
            });
        io::safe_join(&root_path, &prefix)?;
        let mut group = LocalGroup {
            product_id: product_id.to_owned(),
            title: product["title"].as_str().unwrap_or("Local").to_owned(),
            prefix,
            files: vec![],
            directories: vec![],
            metadata: None,
            remove_file_ids: removes,
            membership: true,
            expected_mutation: product["localManifest"]["lastMutationId"]
                .as_str()
                .map(str::to_owned),
        };
        let mut names = HashSet::new();
        for source in additions {
            let path = PathBuf::from(source);
            ensure!(path.is_absolute(), "root_not_absolute");
            archive::validate_no_links(&path)?;
            let path = path.canonicalize()?;
            ensure!(
                !(path.is_dir() && path_contains(&path, &root_path)),
                "local_source_contains_library"
            );
            let original = path_name(&path)?;
            let mut relative = original.clone();
            let mut n = 2;
            while names.contains(&relative.to_lowercase())
                || io::safe_join(&root_path, &format!("{}/{relative}", group.prefix))?.exists()
            {
                let stem = Path::new(&original).file_stem().unwrap().to_string_lossy();
                relative = match path
                    .extension()
                    .and_then(|e| e.to_str())
                    .filter(|_| path.is_file())
                {
                    Some(ext) => format!("{stem} ({n}).{ext}"),
                    None => format!("{original} ({n})"),
                };
                n += 1;
            }
            names.insert(relative.to_lowercase());
            enumerate(&path, &relative, &mut group)?;
        }
        let plan = LocalPlan {
            draft_id: draft,
            groups: vec![group],
            metadata: json!({}),
        };
        let job = json!({"id":id,"type":JOB_TYPE,"originDeviceId":self.catalogue.device_id(),"rootId":root["id"],"requestFingerprint":fingerprint,"productIds":[product_id],"completedProductIds":[],"localPlan":plan,"state":"queued","progress":0,"createdAt":now(),"updatedAt":now()});
        self.catalogue.put("job", &id, job)?;
        Ok(json!({"jobId":id,"productIds":[product_id],"state":"queued"}))
    }
    fn local_metadata(&self, args: &Value) -> Result<Value> {
        ensure!(
            args.get("categoryManual").is_none() || args["categoryManual"].is_boolean(),
            "invalid_arguments"
        );
        let category = args["category"].as_str().unwrap_or("NonVRChat");
        ensure!(
            [
                "Base",
                "Clothes",
                "Hair",
                "Accessory",
                "Texture",
                "Motion",
                "Tool",
                "Shader",
                "Gimmick",
                "World",
                "Udon",
                "NonVRChat"
            ]
            .contains(&category),
            "invalid_arguments"
        );
        let mut metadata = json!({"category":category,"categoryManual":category_manual(args),"tags":strings(&args["tags"])});
        if let Some(url) = args["sourceUrl"].as_str().filter(|s| !s.trim().is_empty()) {
            metadata["sourceUrl"] = json!(crate::native::normalize_public_url(url)?);
        }
        if let Some(thumbnail) = args["thumbnailPath"].as_str().filter(|s| !s.is_empty()) {
            archive::validate_relative_path(thumbnail)?;
            ensure!(thumbnail.starts_with("media/"), "invalid_arguments");
            io::ensure_regular(&io::safe_join(&self.catalogue.data_dir(), thumbnail)?)?;
            metadata["thumbnailPath"] = json!(thumbnail);
        }
        if let Some(url) = args["thumbnailUrl"].as_str().filter(|s| !s.is_empty()) {
            metadata["sourceThumbnailUrl"] = json!(crate::native::normalize_public_url(url)?);
        }
        Ok(metadata)
    }

    pub(super) async fn enqueue_local(&self, args: &Value) -> Result<Value> {
        let _work = self.catalogue.begin_catalogue_work()?;
        let _operation = self
            .operations
            .try_read()
            .map_err(|_| anyhow::anyhow!("storage_busy"))?;
        let _gate = self.queue_gate.lock().await;
        let service = self.clone();
        let args = args.clone();
        tokio::task::spawn_blocking(move || service.prepare_local_import(&args)).await?
    }

    pub(super) fn prepare_local_import(&self, args: &Value) -> Result<Value> {
        let draft = uuid::Uuid::parse_str(io::required_string(args, "draftId")?)?.to_string();
        let id = format!("local-import:{draft}");
        // A repeated submission resumes the durable draft; paths are not re-enumerated.
        if let Some(job) = self.catalogue.get("job", &id)? {
            ensure!(job["type"] == JOB_TYPE, "invalid_job");
            ensure!(
                job["requestFingerprint"] == io::key(&serde_json::to_string(args)?),
                "local_draft_changed"
            );
            return Ok(json!({"jobId":id,"productIds":job["productIds"],"state":job["state"]}));
        }
        let mode = args["mode"].as_str().unwrap_or("single");
        ensure!(matches!(mode, "single" | "batch"), "invalid_arguments");
        let raw = args["sources"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid_arguments"))?;
        ensure!(
            !raw.is_empty() && raw.len() <= 10_000 && raw.iter().all(Value::is_string),
            "invalid_arguments"
        );
        let root = self.root(args["rootId"].as_str())?;
        let root_path = self.root_path(&root)?.canonicalize()?;
        let category = args["category"].as_str().unwrap_or("NonVRChat");
        ensure!(
            [
                "Base",
                "Clothes",
                "Hair",
                "Accessory",
                "Texture",
                "Motion",
                "Tool",
                "Shader",
                "Gimmick",
                "World",
                "Udon",
                "NonVRChat"
            ]
            .contains(&category),
            "invalid_arguments"
        );
        let mut sources: Vec<PathBuf> = Vec::new();
        for value in raw {
            let source = PathBuf::from(value.as_str().unwrap());
            ensure!(source.is_absolute(), "root_not_absolute");
            archive::validate_no_links(&source)?;
            let source = source.canonicalize()?;
            ensure!(
                !(source.is_dir() && path_contains(&source, &root_path)),
                "local_source_contains_library"
            );
            if mode == "batch" {
                ensure!(
                    source.is_dir()
                        || source
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("zip")),
                    "local_batch_requires_folder_or_zip"
                );
            }
            if !sources.iter().any(|existing| {
                path_contains(existing, &source) && path_contains(&source, existing)
            }) {
                sources.push(source);
            }
        }
        sources.sort();
        // Do not copy an explicitly selected descendant twice.
        ensure!(
            !sources
                .iter()
                .any(|a| a.is_dir() && sources.iter().any(|b| a != b && path_contains(a, b))),
            "local_source_overlap"
        );
        let selections = if mode == "single" {
            vec![sources]
        } else {
            sources.into_iter().map(|s| vec![s]).collect()
        };
        let mut groups = Vec::new();
        for selected in selections {
            let item_metadata = args["products"].as_array().and_then(|items| {
                items.iter().find(|item| {
                    item["source"].as_str().is_some_and(|path| {
                        PathBuf::from(path).canonicalize().ok().as_ref() == Some(&selected[0])
                    })
                })
            });
            let fields = item_metadata.unwrap_or(args);
            let product_id = format!("local:{}", uuid::Uuid::new_v4());
            let title = if mode == "single" || item_metadata.is_some() {
                fields["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
            } else {
                None
            }
            .unwrap_or(path_name(&selected[0])?);
            let prefix = format!("assets/{}", io::readable_component(&title, &product_id));
            let mut group = LocalGroup {
                product_id,
                title,
                prefix,
                files: Vec::new(),
                directories: Vec::new(),
                metadata: item_metadata
                    .map(|value| self.local_metadata(value))
                    .transpose()?,
                remove_file_ids: Vec::new(),
                membership: false,
                expected_mutation: None,
            };
            let mut top_names = HashSet::new();
            for source in &selected {
                let original = path_name(source)?;
                let mut relative = original.clone();
                let mut suffix = 2;
                while !top_names.insert(relative.to_lowercase()) {
                    let path = Path::new(&original);
                    relative = match path
                        .extension()
                        .and_then(|v| v.to_str())
                        .filter(|_| source.is_file())
                    {
                        Some(ext) => format!(
                            "{} ({suffix}).{ext}",
                            path.file_stem().unwrap().to_string_lossy()
                        ),
                        None => format!("{original} ({suffix})"),
                    };
                    suffix += 1;
                }
                enumerate(source, &relative, &mut group)?;
            }
            ensure!(!group.files.is_empty(), "no_import_files");
            // Windows and portable archives must not alias distinct source entries.
            let mut names = HashSet::new();
            for relative in group
                .directories
                .iter()
                .chain(group.files.iter().map(|f| &f.relative))
            {
                ensure!(
                    names.insert(relative.to_lowercase()),
                    "archive_path_collision"
                );
            }
            group.directories.sort();
            groups.push(group);
        }
        let metadata = self.local_metadata(args)?;
        let plan = LocalPlan {
            draft_id: draft,
            groups,
            metadata,
        };
        let product_ids: Vec<_> = plan.groups.iter().map(|g| g.product_id.clone()).collect();
        let job = json!({"id":id,"type":JOB_TYPE,"originDeviceId":self.catalogue.device_id(),"rootId":root["id"],"requestFingerprint":io::key(&serde_json::to_string(args)?),"productIds":product_ids,"completedProductIds":[],"mode":mode,"localPlan":plan,"state":"queued","progress":0,"createdAt":now(),"updatedAt":now()});
        self.catalogue.put("job", &id, job)?;
        Ok(json!({"jobId":id,"productIds":product_ids}))
    }

    pub(super) async fn retry_local_import(&self, id: &str) -> Result<Value> {
        let _operation = self
            .operations
            .try_read()
            .map_err(|_| anyhow::anyhow!("storage_busy"))?;
        let _gate = self.queue_gate.lock().await;
        let mut job = self
            .catalogue
            .get("job", id)?
            .ok_or_else(|| anyhow::anyhow!("job_missing"))?;
        ensure!(
            job["type"] == JOB_TYPE && job["localPlan"].is_object(),
            "invalid_job"
        );
        ensure!(
            self.catalogue.library_root().is_none()
                || job["originDeviceId"] == self.catalogue.device_id(),
            "local_source_required"
        );
        if !matches!(
            job["state"].as_str(),
            Some("queued" | "running" | "completed")
        ) {
            job["cancelRequested"] = json!(false);
            self.cancellation(id).store(false, Ordering::Release);
            self.put_job(&mut job, "queued", None)?;
        }
        Ok(json!({"jobId":id,"productIds":job["productIds"]}))
    }

    /// Called under the existing queue gate; local products never request store tickets.
    pub(super) fn retry_local_product_locked(
        &self,
        product: &str,
        jobs: &[Value],
    ) -> Result<Option<String>> {
        let (files, complete) = self.remote_files(product)?;
        let mut ready = complete;
        for file in &files {
            let artifact = self.catalogue.get("artifact", &artifact_id(file))?;
            if let Some(artifact) = artifact.filter(|a| a["status"] == "ready") {
                let root = self.root_path(&self.root(artifact["rootId"].as_str())?)?;
                let path = io::safe_join(&root, io::required_string(&artifact, "relativePath")?)?;
                ready &= artifact["originalAvailable"] == true
                    && fs::metadata(path).is_ok_and(|m| {
                        m.is_file() && Some(m.len()) == artifact["sizeBytes"].as_u64()
                    });
            } else {
                ready = false;
            }
        }
        if ready {
            return Ok(None);
        }
        let source = self
            .catalogue
            .get("product", product)?
            .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
        if source["localManifest"]["lastMutationId"].is_string() {
            // Build only from current membership and bounded persisted source plans.
            // Never replay an old import or its old remove delta after a user edit.
            ensure!(!files.is_empty(), "local_source_required");
            let artifacts = self.catalogue.related("artifact", product)?;
            let first = files.first().unwrap();
            let artifact = artifacts
                .iter()
                .find(|a| a["fileId"] == first["id"])
                .ok_or_else(|| anyhow::anyhow!("local_source_required"))?;
            let prefix = io::required_string(artifact, "productDirectory")?.to_owned();
            let root = self.root(artifact["rootId"].as_str())?;
            let mut current_files = Vec::new();
            for file in &files {
                let selected = jobs
                    .iter()
                    .filter(|job| job["type"] == JOB_TYPE)
                    .filter(|job| {
                        self.catalogue.library_root().is_none()
                            || job["originDeviceId"] == self.catalogue.device_id()
                    })
                    .filter_map(|job| {
                        serde_json::from_value::<LocalPlan>(job["localPlan"].clone()).ok()
                    })
                    .flat_map(|plan| plan.groups)
                    .filter(|group| group.product_id == product && group.prefix == prefix)
                    .flat_map(|group| group.files)
                    .find(|candidate| {
                        file_id(product, &candidate.relative) == file["id"].as_str().unwrap_or("")
                    });
                current_files
                    .push(selected.ok_or_else(|| anyhow::anyhow!("local_source_required"))?);
            }
            let draft = uuid::Uuid::new_v4().to_string();
            let id = format!("local-import:{draft}");
            let group = LocalGroup {
                product_id: product.to_owned(),
                title: source["title"].as_str().unwrap_or("Local").to_owned(),
                prefix: prefix.clone(),
                files: current_files,
                directories: strings(&source["localManifest"]["directories"])
                    .iter()
                    .filter_map(|dir| dir.strip_prefix(&format!("{prefix}/")).map(str::to_owned))
                    .collect(),
                metadata: None,
                remove_file_ids: vec![],
                membership: true,
                expected_mutation: source["localManifest"]["lastMutationId"]
                    .as_str()
                    .map(str::to_owned),
            };
            let plan = LocalPlan {
                draft_id: draft,
                groups: vec![group],
                metadata: json!({}),
            };
            let job = json!({"id":id,"type":JOB_TYPE,"originDeviceId":self.catalogue.device_id(),"rootId":root["id"],"requestFingerprint":io::key(&serde_json::to_string(&plan)?),"productIds":[product],"completedProductIds":[],"localPlan":plan,"state":"queued","progress":0,"createdAt":now(),"updatedAt":now()});
            self.catalogue.put("job", &id, job)?;
            return Ok(Some(id));
        }
        let owner = jobs.iter().find(|job| {
            job["type"] == JOB_TYPE
                && (self.catalogue.library_root().is_none()
                    || job["originDeviceId"] == self.catalogue.device_id())
                && job["localPlan"]["groups"]
                    .as_array()
                    .is_some_and(|groups| groups.iter().any(|g| g["product_id"] == product))
        });
        let id = owner
            .and_then(|j| j["id"].as_str())
            .ok_or_else(|| anyhow::anyhow!("local_source_required"))?;
        let mut job = self
            .catalogue
            .get("job", id)?
            .ok_or_else(|| anyhow::anyhow!("job_missing"))?;
        if matches!(job["state"].as_str(), Some("queued" | "running")) {
            return Ok(Some(id.to_owned()));
        }
        let index = job["localPlan"]["groups"]
            .as_array()
            .and_then(|groups| groups.iter().position(|g| g["product_id"] == product))
            .ok_or_else(|| anyhow::anyhow!("local_source_required"))?;
        job["completedProductIds"] = json!(strings(&job["completedProductIds"])
            .into_iter()
            .filter(|id| id != product)
            .collect::<Vec<_>>());
        job["completedGroupIndexes"] = json!(job["completedGroupIndexes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|v| v != &json!(index))
            .collect::<Vec<_>>());
        job["cancelRequested"] = json!(false);
        self.cancellation(id).store(false, Ordering::Release);
        self.put_job(&mut job, "queued", None)?;
        Ok(Some(id.to_owned()))
    }

    fn copy_local_file(
        &self,
        root: &Path,
        draft: &str,
        group: &LocalGroup,
        file: &LocalFile,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        io::check_cancel(cancel)?;
        let checkpoint = checkpoint_id(draft, group, file);
        let relative = format!("{}/{}", group.prefix, file.relative);
        let target = io::safe_join(root, &relative)?;
        let recorded = self.catalogue.get("checkpoint", &checkpoint)?;
        if let Some(record) = recorded.as_ref() {
            ensure!(record["relativePath"] == relative, "file_integrity_failed");
            if target.try_exists()? {
                io::verify(
                    &target,
                    io::required_string(record, "hash")?,
                    record["sizeBytes"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?,
                )?;
                return Ok(record.clone());
            }
            // A remapped root may lack a saved copy. Recopy unchanged input;
            // an existing damaged target above is retained and never overwritten.
        }
        if target.exists() {
            // A published copy remains valid even if its original source is now unavailable.
            if let Some(descriptor) = self
                .catalogue
                .get("remote_file", &file_id(&group.product_id, &file.relative))?
            {
                if let Some(artifact) = self.catalogue.get("artifact", &artifact_id(&descriptor))? {
                    if artifact["relativePath"] == relative {
                        let hash = io::required_string(&artifact, "hash")?;
                        let size = artifact["sizeBytes"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("file_integrity_failed"))?;
                        io::verify(&target, hash, size)?;
                        let record = json!({"id":checkpoint,"kind":"localImportCopy","relativePath":relative,"hash":hash,"sizeBytes":size});
                        self.catalogue
                            .put("checkpoint", &checkpoint, record.clone())?;
                        return Ok(record);
                    }
                }
            }
        }
        ensure!(
            signature(&file.source)? == (file.size, file.modified),
            "local_source_changed"
        );
        io::space(root, file.size)?;
        fs::create_dir_all(target.parent().unwrap())?;
        io::safe_join(root, &relative)?;
        let partial_relative =
            format!(".nna-import-partials/{draft}/{}.part", io::key(&checkpoint));
        let partial = io::safe_join(root, &partial_relative)?;
        fs::create_dir_all(partial.parent().unwrap())?;
        let _empty_partials =
            io::EmptyDirectoryGuard(partial.parent().unwrap().parent().unwrap().to_path_buf());
        let _partial = io::PartialGuard::new(partial.clone());
        if partial.exists() {
            io::ensure_regular(&partial)?;
            fs::remove_file(&partial)?;
        }
        let mut input = fs::File::open(&file.source)?;
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial)?;
        let copied = (|| -> Result<(String, u64)> {
            let mut hash = Sha256::new();
            let mut size = 0u64;
            let mut buffer = [0u8; 128 * 1024];
            loop {
                io::check_cancel(cancel)?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
                hash.update(&buffer[..count]);
                size += count as u64;
            }
            output.sync_all()?;
            ensure!(
                size == file.size && signature(&file.source)? == (file.size, file.modified),
                "local_source_changed"
            );
            let hash = hex::encode(hash.finalize());
            if let Some(record) = recorded.as_ref() {
                ensure!(
                    record["hash"] == hash && record["sizeBytes"] == size,
                    "local_source_changed"
                );
            }
            io::verify(&partial, &hash, size)?;
            Ok((hash, size))
        })();
        drop(output);
        let (hash, size) = match copied {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_file(&partial);
                return Err(error);
            }
        };
        io::check_cancel(cancel)?;
        if target.exists() {
            // Crash between publishing a copy and recording its checkpoint.
            io::verify(&target, &hash, size)?;
            fs::remove_file(&partial)?;
        } else {
            // Hard-link is exclusive and publishes the already flushed file without overwriting.
            if fs::hard_link(&partial, &target).is_err() {
                // Managed roots on filesystems without hard links retain the exclusive copy path.
                io::copy_new_verified(&partial, &target, &hash, size, cancel)?;
            }
            fs::remove_file(&partial)?;
        }
        let record = json!({"id":checkpoint,"kind":"localImportCopy","relativePath":relative,"hash":hash,"sizeBytes":size});
        self.catalogue
            .put("checkpoint", &checkpoint, record.clone())?;
        Ok(record)
    }

    pub(super) fn run_local_import(
        &self,
        job: &mut Value,
        seven_zip: &Path,
        cancel: &AtomicBool,
    ) -> Result<()> {
        self.run_local_import_inner(job, seven_zip, cancel, None)
    }

    fn run_local_import_inner(
        &self,
        job: &mut Value,
        seven_zip: &Path,
        cancel: &AtomicBool,
        app: Option<&tauri::AppHandle>,
    ) -> Result<()> {
        let plan: LocalPlan = serde_json::from_value(job["localPlan"].clone())?;
        let root = self.root(job["rootId"].as_str())?;
        let root_path = self.root_path(&root)?;
        let mut first_error = None;
        let mut failures = Vec::new();
        for (index, group) in plan.groups.iter().enumerate() {
            let completed = job["completedGroupIndexes"]
                .as_array()
                .is_some_and(|a| a.contains(&json!(index)));
            if completed && !group.membership && plan.groups.len() > 1 {
                continue;
            }
            let outcome = (|| -> Result<()> {
            if let Some(product) = self.catalogue.get("product", &group.product_id)? {
                let current = product["localManifest"]["lastMutationId"].as_str();
                if group.membership {
                    ensure!(
                        current == group.expected_mutation.as_deref()
                            || current == Some(plan.draft_id.as_str()),
                        "local_membership_changed"
                    );
                } else {
                    ensure!(current.is_none(), "local_membership_changed");
                }
            }
            if completed {
                // Membership cleanup must still be bound to its captured
                // mutation. A later membership edit invalidates this replay.
                if group.membership {
                    self.cleanup_removed_local(group)?;
                }
                return Ok(());
            }
            io::check_cancel(cancel)?;
            for directory in &group.directories {
                fs::create_dir_all(io::safe_join(
                    &root_path,
                    &format!("{}/{directory}", group.prefix),
                )?)?;
            }
            let copies = group
                .files
                .iter()
                .map(|file| self.copy_local_file(&root_path, &plan.draft_id, group, file, cancel))
                .collect::<Result<Vec<_>>>()?;
            let facts: Vec<_> = group
                .files
                .iter()
                .zip(&copies)
                .map(|(file, copy)| json!([file.relative, copy["hash"], copy["sizeBytes"]]))
                .collect();
            let fingerprint = io::key(&serde_json::to_string(
                &json!({"files":facts,"directories":group.directories}),
            )?);
            let marker_id = format!("local-import-fingerprint:{fingerprint}");
            let duplicate = self
                .catalogue
                .get("acquisition", &marker_id)?
                .and_then(|v| v["productId"].as_str().map(str::to_owned));
            let duplicate = if let Some(id) =
                duplicate.filter(|id| id != &group.product_id && !group.membership)
            {
                self.catalogue
                    .get("product", &id)?
                    .filter(|p| p["provider"] == "local")
                    .map(|_| id)
            } else {
                None
            };
            let mut writes = Vec::new();
            let mut error = None;
            let product_id = if let Some(existing) = duplicate {
                existing
            } else {
                let file_ids: Vec<_> = group
                    .files
                    .iter()
                    .map(|f| file_id(&group.product_id, &f.relative))
                    .collect();
                let metadata = group.metadata.as_ref().unwrap_or(&plan.metadata);
                let mut product = json!({"id":group.product_id,"provider":"local","store":"Local Files","custom":true,"title":group.title,"sourceTitle":group.title,"category":metadata["category"],"tags":metadata["tags"],"sourceUrl":metadata["sourceUrl"],"thumbnailPath":metadata["thumbnailPath"],"price":null,"acquired":"","importedAt":now(),"localManifest":{"complete":true,"fileIds":file_ids,"directories":group.directories.iter().map(|d| format!("{}/{d}", group.prefix)).collect::<Vec<_>>(),"rootId":root["id"],"productDirectory":group.prefix}});
                product["previewUrl"] = metadata["sourceThumbnailUrl"].clone();
                if group.membership {
                    product = self
                        .catalogue
                        .get("product", &group.product_id)?
                        .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
                    ensure!(product["provider"] == "local", "local_product_required");
                    let mut ids = strings(&product["localManifest"]["fileIds"]);
                    ids.retain(|id| !group.remove_file_ids.contains(id));
                    ids.extend(file_ids.iter().cloned());
                    ids.sort();
                    ids.dedup();
                    product["localManifest"]["complete"] = json!(!ids.is_empty());
                    product["localManifest"]["fileIds"] = json!(ids);
                    product["localManifest"]["lastMutationId"] = json!(plan.draft_id);
                    let mut dirs = strings(&product["localManifest"]["directories"]);
                    dirs.extend(
                        group
                            .directories
                            .iter()
                            .map(|d| format!("{}/{d}", group.prefix)),
                    );
                    dirs.sort();
                    dirs.dedup();
                    product["localManifest"]["directories"] = json!(dirs);
                    product["localManifest"]["rootId"] = root["id"].clone();
                    product["localManifest"]["productDirectory"] = json!(group.prefix);
                }
                if self.catalogue.get("product", &group.product_id)?.is_none() {
                    let mut overlay = self
                        .catalogue
                        .get("overlay", &group.product_id)?
                        .unwrap_or(json!({"id":group.product_id}));
                    apply_initial_category(&mut overlay, metadata);
                    if metadata["thumbnailPath"].is_string()
                        && ["thumbnailPath", "sourceThumbnailUrl", "thumbnailHidden"]
                            .iter()
                            .all(|key| overlay.get(*key).is_none())
                    {
                        overlay["thumbnailPath"] = metadata["thumbnailPath"].clone();
                    }
                    writes.push(("overlay".into(), group.product_id.clone(), overlay));
                }
                writes.push(("product".into(), group.product_id.clone(), product));
                let mut artifacts = Vec::new();
                for ((file, copy), id) in group.files.iter().zip(&copies).zip(&file_ids) {
                    let revision = format!("sha256:{}", io::required_string(copy, "hash")?);
                    let descriptor = json!({"id":id,"productId":group.product_id,"provider":"local","originKind":"local","name":file.relative,"revision":revision,"sizeBytes":copy["sizeBytes"]});
                    let artifact_id = artifact_id(&descriptor);
                    let artifact = json!({"id":artifact_id,"fileId":id,"productId":group.product_id,"rootId":root["id"],"relativePath":copy["relativePath"],"productDirectory":group.prefix,"revision":revision,"hash":copy["hash"],"sizeBytes":copy["sizeBytes"],"status":"ready","originalAvailable":true});
                    writes.push(("remote_file".into(), id.clone(), descriptor));
                    artifacts.push(artifact);
                }
                // Expand only supplied managed archive files, never nested extraction output.
                let mut extracted = HashSet::new();
                for n in 0..artifacts.len() {
                    if extracted.contains(&n) {
                        continue;
                    }
                    let path = io::safe_join(
                        &root_path,
                        io::required_string(&artifacts[n], "relativePath")?,
                    )?;
                    if !archive::is_supported_archive(&path) {
                        continue;
                    }
                    // Explicitly selected split files can occupy separate selection containers.
                    // Match only entries from the same original directory, never unrelated trees.
                    let available = artifacts
                        .iter()
                        .zip(&group.files)
                        .filter(|(_, file)| file.source.parent() == group.files[n].source.parent())
                        .map(|(a, _)| {
                            io::safe_join(&root_path, io::required_string(a, "relativePath")?)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let prior = self
                        .catalogue
                        .get("artifact", io::required_string(&artifacts[n], "id")?)?;
                    let prior = prior.as_ref().filter(|a| {
                        a["rootId"] == root["id"]
                            && a["relativePath"] == artifacts[n]["relativePath"]
                    });
                    let result = self.extract_local_archive(
                        &root_path, group, &path, &available, prior, &artifacts, seven_zip, cancel,
                    );
                    match result {
                        Ok((relative, inventory, volumes)) => {
                            let unchanged = artifacts
                                .iter()
                                .filter(|a| {
                                    a["relativePath"]
                                        .as_str()
                                        .and_then(|r| io::safe_join(&root_path, r).ok())
                                        .is_some_and(|p| volumes.contains(&p))
                                })
                                .try_for_each(|a| {
                                    io::check_cancel(cancel)?;
                                    io::verify(
                                        &io::safe_join(
                                            &root_path,
                                            io::required_string(a, "relativePath")?,
                                        )?,
                                        io::required_string(a, "hash")?,
                                        a["sizeBytes"].as_u64().ok_or_else(|| {
                                            anyhow::anyhow!("file_integrity_failed")
                                        })?,
                                    )
                                });
                            if let Err(e) = unchanged {
                                if local_error(&e) == "cancelled" {
                                    return Err(e);
                                }
                                artifacts[n]["status"] = json!("extractionFailed");
                                artifacts[n]["errorCode"] = json!(local_error(&e));
                                error.get_or_insert(local_error(&e));
                                continue;
                            }
                            for (peer, artifact) in artifacts.iter_mut().enumerate() {
                                let peer_path = io::safe_join(
                                    &root_path,
                                    io::required_string(artifact, "relativePath")?,
                                )?;
                                if volumes.contains(&peer_path) {
                                    if inventory
                                        .as_array()
                                        .is_some_and(|entries| !entries.is_empty())
                                    {
                                        artifact["extractedPath"] = json!(relative);
                                        artifact["extractedInventory"] = inventory.clone();
                                    }
                                    extracted.insert(peer);
                                }
                            }
                        }
                        Err(e) if local_error(&e) == "cancelled" => return Err(e),
                        Err(e) => {
                            let code = local_error(&e);
                            artifacts[n]["status"] = json!("extractionFailed");
                            artifacts[n]["errorCode"] = json!(code);
                            error.get_or_insert(code);
                        }
                    }
                }
                writes.extend(
                    artifacts
                        .into_iter()
                        .map(|a| ("artifact".into(), a["id"].as_str().unwrap().to_owned(), a)),
                );
                if !group.membership {
                    writes.push(("acquisition".into(), marker_id.clone(), json!({"id":marker_id,"productId":group.product_id,"observationKind":"localImport","provider":"local","source":"local","importedAt":now()})));
                }
                group.product_id.clone()
            };
            let mut next_job = job.clone();
            next_job["productIds"][index] = json!(product_id);
            if error.is_none() {
                let mut completed = strings(&next_job["completedProductIds"]);
                if !completed.contains(&product_id) {
                    completed.push(product_id.clone());
                }
                next_job["completedProductIds"] = json!(completed);
                let mut indexes = next_job["completedGroupIndexes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                indexes.push(json!(index));
                next_job["completedGroupIndexes"] = json!(indexes);
                next_job["progress"] = json!(100.0 * (index + 1) as f64 / plan.groups.len() as f64);
            }
            next_job["updatedAt"] = json!(now());
            writes.push((
                "job".into(),
                io::required_string(job, "id")?.to_owned(),
                next_job.clone(),
            ));
            self.catalogue.put_many(writes)?;
            // The terminal worker must only persist completion facts that committed.
            *job = next_job;
            if let Some(app) = app {
                self.changed(app, job["id"].as_str());
            }
            if group.membership {
                self.cleanup_removed_local(group)?;
            }
            if let Some(code) = error {
                anyhow::bail!(code);
            }
            // Publication is durable before removing no-longer-needed checkpoints.
            for file in &group.files {
                // Cleanup cannot undo or fail a durably published/reused group.
                let _ = self
                    .catalogue
                    .delete("checkpoint", &checkpoint_id(&plan.draft_id, group, file));
            }
            if product_id != group.product_id {
                // Duplicate staging is ours; originals and the reused product are untouched.
                for copy in &copies {
                    let _ = fs::remove_file(io::safe_join(
                        &root_path,
                        io::required_string(copy, "relativePath")?,
                    )?);
                }
                let mut dirs = group.directories.clone();
                dirs.sort_by_key(|d| std::cmp::Reverse(d.len()));
                for dir in dirs {
                    let _ = fs::remove_dir(io::safe_join(
                        &root_path,
                        &format!("{}/{dir}", group.prefix),
                    )?);
                }
                let _ = fs::remove_dir(io::safe_join(&root_path, &group.prefix)?);
            }
            Ok(())
            })();
            if let Err(error) = outcome {
                let code = local_error(&error);
                if code == "cancelled" { return Err(error); }
                failures.push(json!({"groupIndex":index,"productId":group.product_id,"errorCode":code}));
                first_error.get_or_insert(error);
            }
        }
        job["result"] = json!({"productIds":job["productIds"],"completedProductIds":job["completedProductIds"],"failedGroups":failures});
        if let Some(error) = first_error { return Err(error); }
        Ok(())
    }

    fn extract_local_archive(
        &self,
        root: &Path,
        group: &LocalGroup,
        source: &Path,
        available: &[PathBuf],
        prior: Option<&Value>,
        artifacts: &[Value],
        tool: &Path,
        cancel: &AtomicBool,
    ) -> Result<(String, Value, Vec<PathBuf>)> {
        let mut relative = format!(
            "{}/extracted/{}",
            group.prefix,
            io::readable_component(
                source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Archive"),
                source
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("archive_unsafe_path"))?
            )
        );
        if let Some(prior) = prior
            .and_then(|a| a["extractedPath"].as_str())
            .filter(|p| p.starts_with(&format!("{}/extracted/", group.prefix)))
        {
            relative = prior.to_owned();
        }
        let mut target = io::safe_join(root, &relative)?;
        if let Some(prior) = prior.filter(|a| {
            a["status"] == "ready" && a["extractedPath"].as_str() == Some(relative.as_str())
        }) {
            if let Ok(inventory) = serde_json::from_value::<Vec<archive::VerifiedEntry>>(
                prior["extractedInventory"].clone(),
            ) {
                let sources = archive::validate_volume_group(source, available)?;
                let records = self.catalogue.related("artifact", &group.product_id)?;
                let mut previous_sources = HashSet::new();
                let mut unchanged = !inventory.is_empty();
                // Every source in the recorded extraction must still be attached
                // with the same bytes. A shorter or changed split set is not reusable.
                for record in records.iter().filter(|a| {
                    a["rootId"] == prior["rootId"]
                        && a["extractedPath"] == prior["extractedPath"]
                        && a["extractedInventory"] == prior["extractedInventory"]
                }) {
                    let Some(current) = artifacts.iter().find(|a| a["id"] == record["id"]) else {
                        unchanged = false;
                        break;
                    };
                    if record["status"] != "ready"
                        || record["originalAvailable"] != true
                        || [
                            "fileId",
                            "productId",
                            "rootId",
                            "relativePath",
                            "revision",
                            "hash",
                            "sizeBytes",
                        ]
                        .iter()
                        .any(|key| record[*key] != current[*key])
                    {
                        unchanged = false;
                        break;
                    }
                    let path = io::safe_join(root, io::required_string(record, "relativePath")?)?;
                    if !previous_sources.insert(path) {
                        unchanged = false;
                        break;
                    }
                }
                unchanged &= previous_sources.len() == sources.len()
                    && sources.iter().all(|path| previous_sources.contains(path));
                if unchanged {
                    let expected = inventory
                        .iter()
                        .map(|entry| archive::ArchiveEntry {
                            relative_path: entry.relative_path.clone(),
                            size_bytes: entry.size_bytes,
                            is_directory: entry.is_directory,
                            crc32: None,
                        })
                        .collect::<Vec<_>>();
                    let matches = archive::verify_extracted_inventory(&target, &expected, cancel)
                        .is_ok_and(|actual| actual == inventory);
                    io::check_cancel(cancel)?;
                    if matches {
                        // The caller verifies every original source again before publication.
                        return Ok((relative, prior["extractedInventory"].clone(), sources));
                    }
                }
            }
        }
        let staging = io::safe_join(
            root,
            &format!(".nna-import-staging/{}", io::key(&group.product_id)),
        )?;
        fs::create_dir_all(&staging)?;
        let _empty_staging_parent =
            io::EmptyDirectoryGuard(staging.parent().unwrap().to_path_buf());
        let _empty_staging = io::EmptyDirectoryGuard(staging.clone());
        let extraction = archive::extract_archive(tool, source, available, &staging, cancel)?;
        io::check_cancel(cancel)?;
        let inventory = serde_json::to_value(&extraction.inventory)?;
        if target.exists() {
            // A prior extraction can survive a crash before group publication.
            let expected = extraction
                .inventory
                .iter()
                .map(|entry| archive::ArchiveEntry {
                    relative_path: entry.relative_path.clone(),
                    size_bytes: entry.size_bytes,
                    is_directory: entry.is_directory,
                    crc32: None,
                })
                .collect::<Vec<_>>();
            let matches = archive::verify_extracted_inventory(&target, &expected, cancel)
                .is_ok_and(|actual| actual == extraction.inventory);
            io::check_cancel(cancel)?;
            if matches {
                return Ok((relative, inventory, extraction.source_paths.clone()));
            }
            // Retain damaged or user-modified trees. Only a fresh managed path is published.
            relative = io::unused_relative(root, &relative)?;
            target = io::safe_join(root, &relative)?;
        }
        fs::create_dir_all(target.parent().unwrap())?;
        fs::rename(&extraction.staging_path, &target)?;
        Ok((relative, inventory, extraction.source_paths.clone()))
    }

    pub(super) async fn process_local_job(&self, app: &tauri::AppHandle, mut job: Value) {
        let _operation = self.operations.write().await;
        let id = job["id"].as_str().unwrap_or("").to_owned();
        // Cancellation may be requested while waiting for the operation barrier.
        if let Ok(Some(current)) = self.catalogue.get("job", &id) {
            job = current;
        }
        if job["state"] != "queued" {
            return;
        }
        let cancel = self.cancellation(&id);
        if job["cancelRequested"] == true {
            cancel.store(true, Ordering::Release);
        }
        if self.put_job(&mut job, "running", None).is_err() {
            return;
        }
        self.jobs_changed(app, &id);
        #[cfg(windows)]
        let tool = app
            .path()
            .resource_dir()
            .map(|p| p.join("tools/7zip/7z.exe"));
        #[cfg(not(windows))]
        let tool: std::result::Result<PathBuf, tauri::Error> = Ok(PathBuf::from("7z"));
        let service = self.clone();
        let stop = cancel.clone();
        let worker_job = job.clone();
        let progress_app = app.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut job = worker_job;
            let result = tool.map_err(anyhow::Error::from).and_then(|tool| {
                service.run_local_import_inner(&mut job, &tool, &stop, Some(&progress_app))
            });
            (job, result)
        })
        .await;
        let result = match outcome {
            Ok((latest, result)) => {
                job = latest;
                result
            }
            Err(_) => Err(anyhow::anyhow!("storage_failed")),
        };
        let (state, error) = match &result {
            Ok(()) => ("completed", None),
            Err(e) if local_error(e) == "cancelled" => ("cancelled", None),
            Err(e) => ("needsAction", Some(local_error(e))),
        };
        let _ = self.put_job(&mut job, state, error);
        self.cancellations.lock().unwrap().remove(&id);
        self.changed(app, Some(&id));
    }
}
