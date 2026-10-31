//! Explicit removal of downloaded store files. Product, source file and
//! entitlement records stay, so the files can be downloaded again.
use super::local_delete::{checked_tree, contains, product_directory, remove_checked_tree};
use super::*;
use std::{collections::BTreeSet, fs};

/// While a product's bytes are removed, no download for it can be queued.
pub(super) struct DeleteClaim {
    claimed: Arc<Mutex<HashSet<String>>>,
    ids: Vec<String>,
}
impl Drop for DeleteClaim {
    fn drop(&mut self) {
        if let Ok(mut claimed) = self.claimed.lock() {
            for id in &self.ids {
                claimed.remove(id);
            }
        }
    }
}

/// Bytes an artifact owns: its retained original and its extraction.
fn owned_paths(artifact: &Value) -> Vec<&str> {
    let original = artifact["relativePath"]
        .as_str()
        .filter(|_| artifact["originalAvailable"] == true);
    original
        .into_iter()
        .chain(artifact["extractedPath"].as_str())
        .collect()
}

/// `assets/<product>` in lower case; managed paths can only overlap below it.
fn product_top(path: &str) -> Option<String> {
    let mut parts = path.split(['/', '\\']);
    Some(format!("{}/{}", parts.next()?, parts.next()?).to_lowercase())
}

impl StorageService {
    pub(super) async fn delete_downloads(&self, args: &Value) -> Result<Value> {
        let (products, file) = if let Some(id) = args["fileId"].as_str() {
            ensure!(args.get("productIds").is_none(), "invalid_arguments");
            let file = self
                .catalogue
                .get("remote_file", id)?
                .ok_or_else(|| anyhow::anyhow!("file_missing"))?;
            (
                vec![io::required_string(&file, "productId")?.to_owned()],
                Some(id.to_owned()),
            )
        } else {
            let ids = strings(&args["productIds"]);
            ensure!(
                !ids.is_empty()
                    && args["productIds"].as_array().map(Vec::len) == Some(ids.len())
                    && ids.iter().all(|id| !id.is_empty())
                    && ids.iter().collect::<HashSet<_>>().len() == ids.len(),
                "invalid_arguments"
            );
            (ids, None)
        };
        for id in &products {
            let product = self
                .catalogue
                .get("product", id)?
                .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
            // Offline products hold their only copy; they use Remove or Delete instead.
            ensure!(product["provider"] != "local", "invalid_arguments");
        }
        let work = self.catalogue.begin_catalogue_work()?;
        let operation = self
            .operations
            .clone()
            .try_read_owned()
            .map_err(|_| anyhow::anyhow!("storage_busy"))?;
        let claim = {
            let _gate = self.queue_gate.lock().await;
            let busy = self.catalogue.list("job")?.iter().any(|job| {
                matches!(job["state"].as_str(), Some("queued" | "running"))
                    && (job["type"] == "download" || job["type"] == "unity:import")
                    && std::iter::once(&job["productId"])
                        .chain(job["productIds"].as_array().into_iter().flatten())
                        .filter_map(Value::as_str)
                        .any(|id| products.iter().any(|product| product == id))
            });
            let mut claimed = self.deleting.lock().unwrap();
            ensure!(
                !busy && products.iter().all(|id| !claimed.contains(id)),
                "storage_busy"
            );
            claimed.extend(products.iter().cloned());
            DeleteClaim {
                claimed: self.deleting.clone(),
                ids: products.clone(),
            }
        };
        let service = self.clone();
        tokio::task::spawn_blocking(move || {
            // Dropping the IPC future must not release these while files are removed.
            let _guards = (work, operation, claim);
            service.remove_downloads(&products, file.as_deref())
        })
        .await
        .map_err(|_| anyhow::anyhow!("storage_failed"))?
    }

    fn remove_downloads(&self, products: &[String], file: Option<&str>) -> Result<Value> {
        let (targets, kept): (Vec<_>, Vec<_>) =
            self.catalogue
                .list("artifact")?
                .into_iter()
                .partition(|artifact| {
                    artifact["productId"]
                        .as_str()
                        .is_some_and(|id| products.iter().any(|product| product == id))
                        && file.is_none_or(|file| artifact["fileId"] == file)
                });
        let mut tops = HashSet::new();
        for artifact in &targets {
            let directory = io::required_string(artifact, "productDirectory")?;
            product_directory(directory)?;
            tops.extend(product_top(directory));
        }
        // Bytes still recorded by another file or product stay in place.
        let mut references = Vec::new();
        for artifact in &kept {
            let same_product = artifact["productId"]
                .as_str()
                .is_some_and(|id| products.iter().any(|product| product == id));
            for field in ["relativePath", "extractedPath", "productDirectory"] {
                let Some(path) = artifact[field].as_str() else {
                    continue;
                };
                if (field != "productDirectory" || !same_product)
                    && product_top(path).is_none_or(|top| tops.contains(&top))
                {
                    references.push((artifact["rootId"].as_str().unwrap_or(""), path));
                }
            }
        }
        // Check every path before removing any of them.
        let mut plan: Vec<(String, String, PathBuf, PathBuf)> = Vec::new();
        for artifact in &targets {
            let directory = Path::new(io::required_string(artifact, "productDirectory")?);
            let root_id = io::required_string(artifact, "rootId")?;
            for relative in owned_paths(artifact) {
                let path = archive::validate_relative_path(relative)
                    .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
                ensure!(
                    contains(directory, &path)
                        && path.components().count() > directory.components().count(),
                    "local_delete_unsafe_path"
                );
                if plan.iter().any(|(root, planned, ..)| root == root_id && planned == relative)
                    || references.iter().any(|(root, other)| {
                        *root == root_id
                            && archive::validate_relative_path(other).map_or(true, |other| {
                                contains(&other, &path) || contains(&path, &other)
                            })
                    })
                {
                    continue;
                }
                let root = self.delete_root(Some(root_id))?;
                let absolute = io::safe_join(&root, relative)
                    .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
                match fs::symlink_metadata(&absolute) {
                    Ok(_) => {
                        checked_tree(&absolute)?;
                        ensure!(
                            contains(&root.join("assets"), &absolute.canonicalize()?),
                            "local_delete_unsafe_path"
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                plan.push((root_id.to_owned(), relative.to_owned(), absolute, root.join("assets")));
            }
        }
        let mut failed = HashSet::new();
        for (root_id, relative, absolute, boundary) in &plan {
            let removed = match fs::symlink_metadata(absolute) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
                Ok(metadata) if metadata.is_dir() => remove_checked_tree(absolute),
                Ok(_) => io::reject_link(absolute)
                    .and_then(|_| fs::remove_file(absolute).map_err(Into::into)),
            };
            if removed.is_err() {
                failed.insert((root_id.as_str(), relative.as_str()));
            } else if let Some(parent) = absolute.parent() {
                io::remove_empty_parents(parent, boundary);
            }
        }
        // A file left partly on disk is shown as missing until a retry removes the rest.
        let mut records = Vec::new();
        let mut expected = Vec::new();
        let mut incomplete = BTreeSet::new();
        for artifact in &targets {
            let id = io::required_string(artifact, "id")?.to_owned();
            let root = artifact["rootId"].as_str().unwrap_or("");
            let value = if owned_paths(artifact)
                .into_iter()
                .any(|path| failed.contains(&(root, path)))
            {
                incomplete.extend(artifact["fileId"].as_str());
                let mut missing = artifact.clone();
                missing["status"] = json!("missing");
                missing["errorCode"] = json!("download_delete_incomplete");
                missing["updatedAt"] = json!(now());
                Some(missing)
            } else {
                None
            };
            expected.push(("artifact".into(), id.clone(), Some(artifact.clone())));
            records.push(("artifact".into(), id, value));
        }
        if file.is_none() {
            // Explicit removal also stops automatic downloads until the next Download.
            for id in products {
                records.push((
                    "download_intent".into(),
                    id.clone(),
                    Some(json!({"id":id,"wantDownload":false})),
                ));
            }
        }
        ensure!(
            self.catalogue.write_many_if_unchanged(records, expected)?,
            "storage_busy"
        );
        ensure!(failed.is_empty(), "download_delete_incomplete");
        let deleted: BTreeSet<_> = targets
            .iter()
            .filter_map(|artifact| artifact["fileId"].as_str())
            .filter(|id| !incomplete.contains(id))
            .collect();
        Ok(json!({"deletedFileIds":deleted}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        path: PathBuf,
        library: PathBuf,
        service: StorageService,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("nna-download-delete-authored-{}", uuid::Uuid::new_v4()));
            let library = path.join("library");
            fs::create_dir_all(&library).unwrap();
            let catalogue = Catalogue::open(&path.join("data")).unwrap();
            let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
            let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
            let service = StorageService::new(catalogue, stores).unwrap();
            service
                .catalogue
                .put(
                    "root",
                    "root",
                    json!({"id":"root","path":library,"status":"online"}),
                )
                .unwrap();
            service
                .catalogue
                .put(
                    "account",
                    "account",
                    json!({"id":"account","store":"BOOTH","status":"connected"}),
                )
                .unwrap();
            Self {
                path,
                library,
                service,
            }
        }
        /// One authored store product whose files are all downloaded.
        fn product(&self, id: &str, files: &[&str]) -> String {
            let c = &self.service.catalogue;
            c.put("product",id,json!({"id":id,"provider":"booth","title":format!("Authored {id}"),"category":"Clothes"})).unwrap();
            let directory = format!("assets/{}", io::readable_component(id, id));
            for name in files {
                let file = format!("{id}-{name}");
                c.put("remote_file",&file,json!({"id":file,"productId":id,"accountId":"account","name":name,"revision":"r1"})).unwrap();
                let relative = format!("{directory}/{name}--dir/{name}");
                let path = self.library.join(&relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, format!("authored bytes {file}")).unwrap();
                let (hash, size) = io::hash_file(&path).unwrap();
                let artifact = format!("artifact-{file}");
                c.put("artifact",&artifact,json!({"id":artifact,"productId":id,"fileId":file,"rootId":"root","relativePath":relative,"productDirectory":directory,"revision":"r1","hash":hash,"sizeBytes":size,"status":"ready","originalAvailable":true})).unwrap();
            }
            let ids: Vec<_> = files.iter().map(|name| format!("{id}-{name}")).collect();
            c.put("entitlement",&format!("entitlement-{id}"),json!({"id":format!("entitlement-{id}"),"productId":id,"accountId":"account","fileIds":ids,"manifestComplete":true})).unwrap();
            directory
        }
        fn file_status(&self, product: &str, file: &str) -> String {
            let details = self
                .service
                .catalogue
                .command("product_details", json!({"id":product}))
                .unwrap();
            details["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == file)
                .unwrap()["status"]
                .as_str()
                .unwrap()
                .to_owned()
        }
        fn finish(self) {
            let Self { path, service, .. } = self;
            drop(service);
            assert!(path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("nna-download-delete-authored-"));
            remove_checked_tree(&path).unwrap();
        }
    }

    #[tokio::test]
    async fn product_delete_removes_downloads_and_keeps_records_and_user_files() {
        let f = Fixture::new();
        let directory = f.product("alpha", &["Body.unitypackage", "Texture.zip"]);
        f.product("beta", &["Other.unitypackage"]);
        // An extraction and a user-added file beside the managed outputs.
        let extracted = f.library.join(&directory).join("extracted/Texture--x");
        fs::create_dir_all(extracted.join("nested")).unwrap();
        fs::write(extracted.join("nested/texture.png"), b"authored texture").unwrap();
        let mut archive = f
            .service
            .catalogue
            .get("artifact", "artifact-alpha-Texture.zip")
            .unwrap()
            .unwrap();
        archive["extractedPath"] = json!(format!("{directory}/extracted/Texture--x"));
        f.service
            .catalogue
            .put("artifact", "artifact-alpha-Texture.zip", archive)
            .unwrap();
        fs::write(f.library.join(&directory).join("notes.txt"), b"user notes").unwrap();
        assert_eq!(f.file_status("alpha", "alpha-Body.unitypackage"), "ready");

        let result = f
            .service
            .delete_downloads(&json!({"productIds":["alpha"]}))
            .await
            .unwrap();
        assert_eq!(
            result["deletedFileIds"],
            json!(["alpha-Body.unitypackage", "alpha-Texture.zip"])
        );
        assert!(!extracted.exists());
        assert!(!f.library.join(&directory).join("Body.unitypackage--dir").exists());
        assert_eq!(
            fs::read(f.library.join(&directory).join("notes.txt")).unwrap(),
            b"user notes"
        );
        assert!(f.service.catalogue.related("artifact", "alpha").unwrap().is_empty());
        assert_eq!(f.service.catalogue.related("remote_file", "alpha").unwrap().len(), 2);
        assert_eq!(f.file_status("alpha", "alpha-Body.unitypackage"), "notDownloaded");
        assert_eq!(
            f.service.catalogue.get("download_intent", "alpha").unwrap().unwrap()["wantDownload"],
            false
        );
        // The unrelated product stays; download-all queues it but not the removed one.
        assert_eq!(f.service.catalogue.related("artifact", "beta").unwrap().len(), 1);
        f.service
            .catalogue
            .command("preferences", json!({"patch":{"autoDownloadAll":true}}))
            .unwrap();
        f.service
            .evaluate_automatic(&HashSet::from(["alpha".to_owned(), "beta".to_owned()]))
            .await
            .unwrap();
        let jobs = f.service.catalogue.list("job").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["productId"], "beta");
        f.finish();
    }

    #[tokio::test]
    async fn file_delete_keeps_bytes_another_file_still_records() {
        let f = Fixture::new();
        let directory = f.product("alpha", &["Body.unitypackage", "Copy.unitypackage"]);
        // Verified-content sharing: the second file records the first file's bytes.
        let shared = f
            .service
            .catalogue
            .get("artifact", "artifact-alpha-Body.unitypackage")
            .unwrap()
            .unwrap()["relativePath"]
            .clone();
        let mut copy = f
            .service
            .catalogue
            .get("artifact", "artifact-alpha-Copy.unitypackage")
            .unwrap()
            .unwrap();
        let own = f.library.join(copy["relativePath"].as_str().unwrap());
        copy["relativePath"] = shared.clone();
        f.service
            .catalogue
            .put("artifact", "artifact-alpha-Copy.unitypackage", copy)
            .unwrap();
        f.service
            .delete_downloads(&json!({"fileId":"alpha-Body.unitypackage"}))
            .await
            .unwrap();
        assert!(f.library.join(shared.as_str().unwrap()).is_file());
        assert!(own.is_file(), "an unrecorded file is never removed");
        assert_eq!(f.file_status("alpha", "alpha-Body.unitypackage"), "notDownloaded");
        assert_eq!(f.file_status("alpha", "alpha-Copy.unitypackage"), "ready");
        assert!(f.service.catalogue.get("download_intent", "alpha").unwrap().is_none());
        // Removing the last record also removes the shared bytes and empty folders.
        f.service
            .delete_downloads(&json!({"fileId":"alpha-Copy.unitypackage"}))
            .await
            .unwrap();
        assert!(!f.library.join(shared.as_str().unwrap()).exists());
        assert!(f.library.join(&directory).exists() && own.is_file());
        f.finish();
    }

    #[tokio::test]
    async fn delete_refuses_active_work_and_invalid_targets_without_removing_bytes() {
        let f = Fixture::new();
        let directory = f.product("alpha", &["Body.unitypackage"]);
        let bytes = f
            .library
            .join(&directory)
            .join("Body.unitypackage--dir/Body.unitypackage");
        for (id, job) in [
            ("download", json!({"id":"download","type":"download","productId":"alpha","state":"running"})),
            ("unity", json!({"id":"unity","type":"unity:import","productIds":["alpha"],"state":"queued"})),
        ] {
            f.service.catalogue.put("job", id, job).unwrap();
            assert_eq!(
                f.service
                    .delete_downloads(&json!({"productIds":["alpha"]}))
                    .await
                    .unwrap_err()
                    .to_string(),
                "storage_busy"
            );
            f.service.catalogue.delete("job", id).unwrap();
        }
        f.service
            .catalogue
            .put("product", "local", json!({"id":"local","provider":"local","title":"Authored local"}))
            .unwrap();
        for (args, code) in [
            (json!({"productIds":["local"]}), "invalid_arguments"),
            (json!({"productIds":["alpha","alpha"]}), "invalid_arguments"),
            (json!({"productIds":[]}), "invalid_arguments"),
            (json!({"productIds":["missing"]}), "product_missing"),
            (json!({"fileId":"missing"}), "file_missing"),
        ] {
            assert_eq!(
                f.service.delete_downloads(&args).await.unwrap_err().to_string(),
                code
            );
        }
        // A queued download for a product being removed is refused, not merged.
        let claim = DeleteClaim {
            claimed: f.service.deleting.clone(),
            ids: vec!["alpha".into()],
        };
        f.service.deleting.lock().unwrap().insert("alpha".into());
        assert_eq!(
            f.service
                .enqueue(&json!({"productIds":["alpha"]}))
                .await
                .unwrap_err()
                .to_string(),
            "storage_busy"
        );
        drop(claim);
        assert!(f.service.deleting.lock().unwrap().is_empty());
        assert!(bytes.is_file());
        assert_eq!(f.file_status("alpha", "alpha-Body.unitypackage"), "ready");
        f.finish();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_file_held_open_is_kept_and_shown_missing_until_retried() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        let directory = f.product("alpha", &["Body.unitypackage", "Hair.unitypackage"]);
        let held = f
            .library
            .join(&directory)
            .join("Body.unitypackage--dir/Body.unitypackage");
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&held)
            .unwrap();
        assert_eq!(
            f.service
                .delete_downloads(&json!({"productIds":["alpha"]}))
                .await
                .unwrap_err()
                .to_string(),
            "download_delete_incomplete"
        );
        assert!(held.is_file());
        assert_eq!(f.file_status("alpha", "alpha-Body.unitypackage"), "missing");
        assert_eq!(f.file_status("alpha", "alpha-Hair.unitypackage"), "notDownloaded");
        drop(handle);
        f.service
            .delete_downloads(&json!({"fileId":"alpha-Body.unitypackage"}))
            .await
            .unwrap();
        assert!(!held.exists());
        assert_eq!(f.file_status("alpha", "alpha-Body.unitypackage"), "notDownloaded");
        f.finish();
    }
}
