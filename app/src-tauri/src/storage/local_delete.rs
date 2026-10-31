//! Whole local-product deletion. Only checked managed copies are removed.
use super::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs};

const JOB_TYPE: &str = "storage:delete_local_products";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteDirectory {
    product_id: String,
    root_id: String,
    directory: String,
    staged_directory: String,
}

pub(super) fn contains(parent: &Path, child: &Path) -> bool {
    #[cfg(windows)]
    {
        let components = |path: &Path| {
            path.components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect::<Vec<_>>()
        };
        components(child).starts_with(&components(parent))
    }
    #[cfg(not(windows))]
    {
        child.starts_with(parent)
    }
}

pub(super) fn product_directory(value: &str) -> Result<()> {
    let path = archive::validate_relative_path(value)
        .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
    let parts: Vec<_> = path.components().collect();
    ensure!(
        parts.len() == 2 && parts[0].as_os_str() == "assets",
        "local_delete_unsafe_path"
    );
    ensure!(
        !parts[1]
            .as_os_str()
            .to_string_lossy()
            .starts_with(".nna-delete-"),
        "local_delete_unsafe_path"
    );
    Ok(())
}

/// Inspect the complete tree before staging/removal; never traverse a reparse point.
pub(super) fn checked_tree(path: &Path) -> Result<()> {
    let mut pending = vec![path.to_owned()];
    while let Some(path) = pending.pop() {
        io::reject_link(&path).map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else {
            ensure!(metadata.is_file(), "local_delete_unsafe_path");
        }
    }
    Ok(())
}

/// Explicit entry-by-entry removal preserves the no-link boundary at every step.
pub(super) fn remove_checked_tree(path: &Path) -> Result<()> {
    checked_tree(path)?;
    let mut pending = vec![(path.to_owned(), false)];
    while let Some((path, visited)) = pending.pop() {
        io::reject_link(&path).map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
        if fs::metadata(&path)?.is_dir() {
            if visited {
                fs::remove_dir(&path)?;
            } else {
                pending.push((path.clone(), true));
                for entry in fs::read_dir(&path)? {
                    pending.push((entry?.path(), false));
                }
            }
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

impl StorageService {
    pub(super) fn delete_root(&self, id: Option<&str>) -> Result<PathBuf> {
        let path = self.root_path(&self.root(id)?).map_err(|error| {
            if error.to_string().contains("unsafe_link") {
                anyhow::anyhow!("local_delete_unsafe_path")
            } else {
                error
            }
        })?;
        archive::validate_no_links(&path)
            .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
        Ok(path.canonicalize()?)
    }
    pub(super) async fn delete_local_products(
        &self,
        args: &Value,
        retry: Option<&str>,
    ) -> Result<Value> {
        // Keep both guards inside the blocking task: dropping an IPC future must
        // not release them while filesystem changes are still in progress.
        let work = self.catalogue.begin_restore_work()?;
        let operation = self
            .operations
            .clone()
            .try_write_owned()
            .map_err(|_| anyhow::anyhow!("storage_busy"))?;
        let service = self.clone();
        let args = args.clone();
        let retry = retry.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            let (_work, _operation) = (work, operation);
            service.run_local_delete(&args, retry.as_deref())
        })
        .await
        .map_err(|_| anyhow::anyhow!("storage_failed"))?
    }

    fn delete_plan(&self, ids: &[String], job_id: &str) -> Result<Vec<DeleteDirectory>> {
        let artifacts = self.catalogue.list("artifact")?;
        let jobs = self.catalogue.list("job")?;
        let mut directories = BTreeSet::new();
        for id in ids {
            let product = self
                .catalogue
                .get("product", id)?
                .ok_or_else(|| anyhow::anyhow!("product_missing"))?;
            ensure!(product["provider"] == "local", "local_product_required");
            if let Some(directory) = product["localManifest"]["productDirectory"].as_str() {
                product_directory(directory)?;
                directories.insert((
                    id.clone(),
                    io::required_string(&product["localManifest"], "rootId")?.to_owned(),
                    directory.to_owned(),
                ));
            }
            for artifact in artifacts.iter().filter(|a| a["productId"] == *id) {
                let directory = io::required_string(artifact, "productDirectory")?;
                product_directory(directory)?;
                for field in ["relativePath", "extractedPath"] {
                    if let Some(relative) = artifact[field].as_str() {
                        ensure!(
                            contains(
                                Path::new(directory),
                                &archive::validate_relative_path(relative)?
                            ),
                            "local_delete_unsafe_path"
                        );
                    }
                }
                directories.insert((
                    id.clone(),
                    io::required_string(artifact, "rootId")?.to_owned(),
                    directory.to_owned(),
                ));
            }
            // Membership may be empty, with user-edited bytes retained by file
            // removal. The durable import plan still owns its stable directory.
            for job in jobs.iter().filter(|j| j["type"] == "storage:import_local") {
                for group in job["localPlan"]["groups"].as_array().into_iter().flatten() {
                    if group["product_id"] == *id {
                        let prefix = io::required_string(group, "prefix")?;
                        product_directory(prefix)?;
                        directories.insert((
                            id.clone(),
                            io::required_string(job, "rootId")?.to_owned(),
                            prefix.to_owned(),
                        ));
                    }
                }
            }
        }
        let mut plan = Vec::new();
        for (product_id, root_id, directory) in directories {
            let root = self.delete_root(Some(&root_id))?;
            let target = io::safe_join(&root, &directory)
                .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
            self.require_unshared_delete_path(&product_id, &target)?;
            match fs::symlink_metadata(&target) {
                Ok(metadata) => {
                    ensure!(metadata.is_dir(), "local_delete_unsafe_path");
                    checked_tree(&target)?;
                    ensure!(
                        contains(&root.join("assets"), &target.canonicalize()?),
                        "local_delete_unsafe_path"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            ensure!(
                !plan.iter().any(|entry: &DeleteDirectory| {
                    self.delete_root(Some(&entry.root_id))
                        .and_then(|p| io::safe_join(&p, &entry.directory))
                        .is_ok_and(|p| contains(&p, &target) || contains(&target, &p))
                }),
                "local_delete_shared_path"
            );
            let staged_directory = format!("assets/.nna-delete-{job_id}-{}", plan.len());
            let staged = io::safe_join(&root, &staged_directory)?;
            ensure!(
                fs::symlink_metadata(staged)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
                "local_delete_unsafe_path"
            );
            plan.push(DeleteDirectory {
                product_id,
                root_id,
                directory,
                staged_directory,
            });
        }
        Ok(plan)
    }

    fn require_unshared_delete_path(&self, product: &str, target: &Path) -> Result<()> {
        for artifact in self
            .catalogue
            .list("artifact")?
            .into_iter()
            .filter(|a| a["productId"] != product)
        {
            let root = self.delete_root(artifact["rootId"].as_str())?;
            for field in ["productDirectory", "relativePath", "extractedPath"] {
                if let Some(relative) = artifact[field].as_str() {
                    let path = io::safe_join(&root, relative)?;
                    ensure!(
                        !contains(target, &path) && !contains(&path, target),
                        "local_delete_shared_path"
                    );
                }
            }
        }
        for job in self
            .catalogue
            .list("job")?
            .into_iter()
            .filter(|j| j["type"] == "storage:import_local")
        {
            for group in job["localPlan"]["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|g| g["product_id"] != product)
            {
                let root = self.delete_root(job["rootId"].as_str())?;
                let path = io::safe_join(&root, io::required_string(group, "prefix")?)?;
                ensure!(
                    !contains(target, &path) && !contains(&path, target),
                    "local_delete_shared_path"
                );
            }
        }
        for source in self
            .catalogue
            .list("product")?
            .into_iter()
            .filter(|p| p["id"] != product && p["provider"] == "local")
        {
            if let Some(directory) = source["localManifest"]["productDirectory"].as_str() {
                let root = self.delete_root(source["localManifest"]["rootId"].as_str())?;
                let path = io::safe_join(&root, directory)?;
                ensure!(
                    !contains(target, &path) && !contains(&path, target),
                    "local_delete_shared_path"
                );
            }
            for relative in strings(&source["localManifest"]["directories"]) {
                let root = self.delete_root(source["localManifest"]["rootId"].as_str())?;
                let path = io::safe_join(&root, &relative)?;
                ensure!(
                    !contains(target, &path) && !contains(&path, target),
                    "local_delete_shared_path"
                );
            }
        }
        Ok(())
    }

    fn delete_paths(
        &self,
        job_id: &str,
        index: usize,
        entry: &DeleteDirectory,
    ) -> Result<(PathBuf, PathBuf)> {
        uuid::Uuid::parse_str(job_id).map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
        product_directory(&entry.directory)?;
        ensure!(
            entry.staged_directory == format!("assets/.nna-delete-{job_id}-{index}"),
            "local_delete_unsafe_path"
        );
        let root = self.delete_root(Some(&entry.root_id))?;
        let original = io::safe_join(&root, &entry.directory)
            .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
        let staged = io::safe_join(&root, &entry.staged_directory)
            .map_err(|_| anyhow::anyhow!("local_delete_unsafe_path"))?;
        ensure!(
            contains(&root.join("assets"), &original) && contains(&root.join("assets"), &staged),
            "local_delete_unsafe_path"
        );
        if staged.exists() {
            ensure!(
                contains(&root.join("assets"), &staged.canonicalize()?),
                "local_delete_unsafe_path"
            );
        }
        Ok((original, staged))
    }

    fn rollback_delete(&self, id: &str, directories: &[DeleteDirectory]) -> Result<()> {
        for (index, entry) in directories.iter().enumerate().rev() {
            let (original, staged) = self.delete_paths(id, index, entry)?;
            if fs::symlink_metadata(&staged).is_ok() {
                ensure!(!original.exists(), "local_delete_rollback_failed");
                fs::rename(staged, original)?;
            }
        }
        Ok(())
    }

    fn run_local_delete(&self, args: &Value, retry: Option<&str>) -> Result<Value> {
        let mut job = if let Some(id) = retry {
            let job = self
                .catalogue
                .get("job", id)?
                .ok_or_else(|| anyhow::anyhow!("job_missing"))?;
            ensure!(
                job["type"] == JOB_TYPE && job["localDelete"].is_object(),
                "invalid_job"
            );
            ensure!(
                self.catalogue.library_root().is_none()
                    || job["originDeviceId"] == self.catalogue.device_id(),
                "local_source_required"
            );
            job
        } else {
            let values = args["productIds"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid_arguments"))?;
            let ids = strings(&args["productIds"]);
            ensure!(
                !ids.is_empty()
                    && ids.len() == values.len()
                    && ids.iter().all(|id| !id.is_empty())
                    && ids.iter().collect::<HashSet<_>>().len() == ids.len(),
                "invalid_arguments"
            );
            ensure!(
                !self
                    .catalogue
                    .list("job")?
                    .iter()
                    .any(|job| job["type"] == JOB_TYPE
                        && job["state"] != "completed"
                        && strings(&job["productIds"])
                            .iter()
                            .any(|id| ids.contains(id))),
                "storage_busy"
            );
            json!({"id":uuid::Uuid::new_v4().to_string(),"type":JOB_TYPE,"originDeviceId":self.catalogue.device_id(),"productIds":ids,"state":"running","createdAt":now(),"updatedAt":now(),"localDelete":{"recordsDeleted":false}})
        };
        let id = io::required_string(&job, "id")?.to_owned();
        let ids = strings(&job["productIds"]);
        if job["localDelete"]["recordsDeleted"] != true {
            if let Some(value) = job["localDelete"].get("directories") {
                let old: Vec<DeleteDirectory> = serde_json::from_value(value.clone())?;
                if self.rollback_delete(&id, &old).is_err() {
                    self.put_job(
                        &mut job,
                        "needsAction",
                        Some("local_delete_rollback_failed"),
                    )?;
                    bail!("local_delete_rollback_failed");
                }
            }
            let directories = self.delete_plan(&ids, &id)?;
            job["localDelete"]["directories"] = json!(directories);
            self.put_job(&mut job, "running", None)?;
            let staged = (|| -> Result<()> {
                for (index, entry) in directories.iter().enumerate() {
                    let (original, staged) = self.delete_paths(&id, index, entry)?;
                    if fs::symlink_metadata(&original).is_ok() {
                        fs::rename(original, staged)?;
                    }
                }
                let mut committed = job.clone();
                committed["localDelete"]["recordsDeleted"] = json!(true);
                self.catalogue
                    .delete_local_products(&ids, committed.clone())?;
                job = committed;
                Ok(())
            })();
            if let Err(error) = staged {
                let rollback_failed = self.rollback_delete(&id, &directories).is_err();
                let code = if rollback_failed {
                    "local_delete_rollback_failed"
                } else {
                    io::stable_error(&error)
                };
                self.put_job(&mut job, "needsAction", Some(code))?;
                if rollback_failed {
                    bail!("local_delete_rollback_failed");
                }
                // The durable job still owns these IDs after a successful
                // rollback. Surface its retry route instead of inviting a new
                // deletion that would correctly be blocked by that job.
                bail!("local_delete_retry_required");
            }
        }
        let directories: Vec<DeleteDirectory> =
            serde_json::from_value(job["localDelete"]["directories"].clone())?;
        self.put_job(&mut job, "running", None)?;
        let cleanup = (|| -> Result<()> {
            for (index, entry) in directories.iter().enumerate() {
                let (_, staged) = self.delete_paths(&id, index, entry)?;
                self.require_unshared_delete_path(&entry.product_id, &staged)?;
                match fs::symlink_metadata(&staged) {
                    Ok(_) => remove_checked_tree(&staged)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        })();
        if cleanup.is_err() {
            self.put_job(&mut job, "needsAction", Some("local_delete_cleanup_failed"))?;
            bail!("local_delete_cleanup_failed");
        }
        job["result"] = json!({"deletedProductIds":ids});
        self.put_job(&mut job, "completed", None)?;
        Ok(json!({"deletedProductIds":ids}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn create_junction(link: &Path, target: &Path) {
        use std::os::windows::process::CommandExt;
        let link = link
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(link.file_name().unwrap());
        let target = target.canonicalize().unwrap();
        let status = std::process::Command::new("cmd")
            .arg("/d")
            .arg("/c")
            .raw_arg(format!(
                "mklink /J \"{}\" \"{}\"",
                link.to_string_lossy().trim_start_matches(r"\\?\"),
                target.to_string_lossy().trim_start_matches(r"\\?\")
            ))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{} {}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
    }

    struct Fixture {
        path: PathBuf,
        service: StorageService,
    }
    impl Fixture {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/offline-delete")
                .join(format!(
                    "nna-local-delete-authored-{}",
                    uuid::Uuid::new_v4()
                ));
            fs::create_dir_all(&path).unwrap();
            let path = path.canonicalize().unwrap();
            #[cfg(windows)]
            let path = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
            let catalogue = Catalogue::open(&path.join("data")).unwrap();
            let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
            let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
            let service = StorageService::new(catalogue, stores).unwrap();
            fs::create_dir_all(path.join("library")).unwrap();
            service
                .catalogue
                .put(
                    "root",
                    "root",
                    json!({"id":"root","path":path.join("library"),"status":"online"}),
                )
                .unwrap();
            Self { path, service }
        }
        fn import(&self, name: &str) -> (String, PathBuf, PathBuf) {
            let original = self.path.join(format!("{name}.txt"));
            fs::write(&original, format!("Authored original {name}")).unwrap();
            let result = self.service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4(),"sources":[original],"name":name,"rootId":"root"})).unwrap();
            let mut job = self
                .service
                .catalogue
                .get("job", result["jobId"].as_str().unwrap())
                .unwrap()
                .unwrap();
            self.service
                .run_local_import(&mut job, Path::new("unused"), &AtomicBool::new(false))
                .unwrap();
            self.service.put_job(&mut job, "completed", None).unwrap();
            let id = job["productIds"][0].as_str().unwrap().to_owned();
            let directory = self.service.catalogue.related("artifact", &id).unwrap()[0]
                ["productDirectory"]
                .as_str()
                .unwrap()
                .to_owned();
            (id, self.path.join("library").join(directory), original)
        }
        fn finish(self) {
            let Self { path, service } = self;
            drop(service);
            let canonical = path.canonicalize().unwrap();
            let boundary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/offline-delete")
                .canonicalize()
                .unwrap();
            assert!(contains(&boundary, &canonical));
            assert!(canonical
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("nna-local-delete-authored-"));
            remove_checked_tree(&canonical).unwrap();
        }
    }

    #[tokio::test]
    async fn delete_local_products_removes_complete_owned_folders_and_preserves_originals() {
        let f = Fixture::new();
        let (a, dir_a, source_a) = f.import("Alpha");
        let (b, dir_b, source_b) = f.import("Beta");
        let (keep, dir_keep, _) = f.import("Keep");
        fs::create_dir_all(dir_a.join("edited/subfolder")).unwrap();
        fs::write(
            dir_a.join("edited/subfolder/user-added.txt"),
            b"owned user addition",
        )
        .unwrap();
        f.service
            .catalogue
            .put("overlay", &a, json!({"id":a,"note":"Authored note"}))
            .unwrap();
        f.service
            .catalogue
            .put(
                "collection",
                "collection",
                json!({"id":"collection","name":"Authored collection","productIds":[a,b,keep]}),
            )
            .unwrap();
        let result = f
            .service
            .delete_local_products(&json!({"productIds":[a,b]}), None)
            .await
            .unwrap();
        assert_eq!(result["deletedProductIds"], json!([a, b]));
        assert!(!dir_a.exists() && !dir_b.exists() && dir_keep.exists());
        assert_eq!(fs::read(source_a).unwrap(), b"Authored original Alpha");
        assert_eq!(fs::read(source_b).unwrap(), b"Authored original Beta");
        for kind in [
            "product",
            "overlay",
            "acquisition",
            "remote_file",
            "artifact",
        ] {
            assert!(f.service.catalogue.get(kind, &a).unwrap().is_none());
            assert!(f.service.catalogue.related(kind, &a).unwrap().is_empty());
        }
        assert_eq!(
            f.service
                .catalogue
                .get("collection", "collection")
                .unwrap()
                .unwrap()["productIds"],
            json!([keep])
        );
        assert!(f
            .service
            .catalogue
            .list("job")
            .unwrap()
            .iter()
            .filter(|j| j["type"] == "storage:import_local")
            .all(|j| !strings(&j["productIds"]).contains(&a)
                && !strings(&j["productIds"]).contains(&b)));
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_rejects_mixed_missing_duplicate_and_invalid_selection_atomically(
    ) {
        let f = Fixture::new();
        let (a, dir, original) = f.import("Local");
        f.service
            .catalogue
            .put(
                "product",
                "online",
                json!({"id":"online","provider":"booth","title":"Authored online"}),
            )
            .unwrap();
        for (args, code) in [
            (json!({"productIds":[a,"online"]}), "local_product_required"),
            (json!({"productIds":[a,"missing"]}), "product_missing"),
            (json!({"productIds":[a,a]}), "invalid_arguments"),
            (json!({"productIds":[a,7]}), "invalid_arguments"),
            (json!({"productIds":[]}), "invalid_arguments"),
        ] {
            assert_eq!(
                f.service
                    .delete_local_products(&args, None)
                    .await
                    .unwrap_err()
                    .to_string(),
                code
            );
            assert!(dir.exists() && original.exists());
            assert!(f.service.catalogue.get("product", &a).unwrap().is_some());
        }
        assert!(f
            .service
            .catalogue
            .list("job")
            .unwrap()
            .iter()
            .all(|j| j["type"] != JOB_TYPE));
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_rejects_unsafe_and_shared_directory_references() {
        let f = Fixture::new();
        let (a, directory, _) = f.import("Owned");
        let (other, _, _) = f.import("Other");
        let artifact = f.service.catalogue.related("artifact", &a).unwrap()[0].clone();
        let artifact_id = artifact["id"].as_str().unwrap();
        for unsafe_directory in ["assets", "media/Owned", "assets/Owned/child"] {
            let mut bad = artifact.clone();
            bad["productDirectory"] = json!(unsafe_directory);
            f.service
                .catalogue
                .put("artifact", artifact_id, bad)
                .unwrap();
            assert_eq!(
                f.service
                    .delete_local_products(&json!({"productIds":[a]}), None)
                    .await
                    .unwrap_err()
                    .to_string(),
                "local_delete_unsafe_path"
            );
            assert!(directory.exists());
        }
        f.service
            .catalogue
            .put("artifact", artifact_id, artifact.clone())
            .unwrap();
        let mut shared = artifact.clone();
        shared["id"] = json!("shared");
        shared["productId"] = json!(other);
        f.service
            .catalogue
            .put("artifact", "shared", shared)
            .unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "local_delete_shared_path"
        );
        assert!(directory.exists() && f.service.catalogue.get("product", &a).unwrap().is_some());
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_rolls_staging_back_when_catalogue_transaction_fails() {
        let f = Fixture::new();
        let (a, directory, original) = f.import("Rollback");
        let connection =
            rusqlite::Connection::open(f.service.catalogue.data_dir().join("catalogue.sqlite3"))
                .unwrap();
        connection.execute_batch("CREATE TRIGGER authored_delete_failure BEFORE DELETE ON entities WHEN OLD.kind='product' BEGIN SELECT RAISE(ABORT,'fixture deletion failure'); END;").unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "local_delete_retry_required"
        );
        assert!(directory.exists() && original.exists());
        assert!(f.service.catalogue.get("product", &a).unwrap().is_some());
        assert!(!f
            .service
            .catalogue
            .related("artifact", &a)
            .unwrap()
            .is_empty());
        let job = f
            .service
            .catalogue
            .list("job")
            .unwrap()
            .into_iter()
            .find(|j| j["type"] == JOB_TYPE)
            .unwrap();
        assert_eq!(job["localDelete"]["recordsDeleted"], false);
        assert_eq!(job["errorCode"], "storage_failed");
        assert_eq!(job["state"], "needsAction");
        connection
            .execute_batch("DROP TRIGGER authored_delete_failure;")
            .unwrap();
        drop(connection);
        f.service
            .delete_local_products(&json!({}), job["id"].as_str())
            .await
            .unwrap();
        assert!(!directory.exists());
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_refuses_active_work_and_queued_jobs() {
        let f = Fixture::new();
        let (a, dir, _) = f.import("Busy");
        let work = f.service.catalogue.begin_catalogue_work().unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "storage_busy"
        );
        drop(work);
        f.service
            .catalogue
            .put(
                "job",
                "busy",
                json!({"id":"busy","type":"unity:import","state":"queued"}),
            )
            .unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "storage_busy"
        );
        assert!(dir.exists());
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_keeps_empty_product_folder_ownership_after_activity_clear() {
        let f = Fixture::new();
        let (a, dir, original) = f.import("Empty");
        let mut source = f.service.catalogue.get("product", &a).unwrap().unwrap();
        source["localManifest"]["fileIds"] = json!([]);
        source["localManifest"]["complete"] = json!(false);
        f.service.catalogue.put("product", &a, source).unwrap();
        for artifact in f.service.catalogue.related("artifact", &a).unwrap() {
            f.service
                .catalogue
                .delete("artifact", artifact["id"].as_str().unwrap())
                .unwrap();
        }
        for file in f.service.catalogue.related("remote_file", &a).unwrap() {
            f.service
                .catalogue
                .delete("remote_file", file["id"].as_str().unwrap())
                .unwrap();
        }
        f.service
            .catalogue
            .command("clear_activity", json!({}))
            .unwrap();
        // File membership removal intentionally retained these edited managed bytes.
        fs::write(dir.join("retained-edited.txt"), b"authored retained edit").unwrap();
        f.service
            .delete_local_products(&json!({"productIds":[a]}), None)
            .await
            .unwrap();
        assert!(!dir.exists() && original.exists());
        // Pure metadata local records own no physical directory to guess.
        f.service
            .catalogue
            .put(
                "product",
                "local:metadata",
                json!({"id":"local:metadata","provider":"local","title":"Authored metadata only"}),
            )
            .unwrap();
        f.service
            .delete_local_products(&json!({"productIds":["local:metadata"]}), None)
            .await
            .unwrap();
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_restores_interrupted_precommit_staging_before_retry() {
        let f = Fixture::new();
        let (a, dir, original) = f.import("Interrupted");
        let id = uuid::Uuid::new_v4().to_string();
        let directories = f.service.delete_plan(&[a.clone()], &id).unwrap();
        let (_, staged) = f.service.delete_paths(&id, 0, &directories[0]).unwrap();
        let job = json!({"id":id,"type":JOB_TYPE,"originDeviceId":f.service.catalogue.device_id(),"productIds":[a],"state":"needsAction","errorCode":"interrupted","localDelete":{"directories":directories,"recordsDeleted":false}});
        f.service.catalogue.put("job", &id, job).unwrap();
        fs::rename(&dir, &staged).unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "storage_busy"
        );
        assert!(f.service.catalogue.get("product", &a).unwrap().is_some());
        f.service
            .delete_local_products(&json!({}), Some(&id))
            .await
            .unwrap();
        assert!(!dir.exists() && !staged.exists() && original.exists());
        f.finish();
    }

    #[tokio::test]
    async fn delete_local_products_keeps_other_batch_groups_without_recreating_deleted_group() {
        let f = Fixture::new();
        for name in ["A", "B"] {
            fs::create_dir(f.path.join(name)).unwrap();
            fs::write(f.path.join(name).join("authored.txt"), name.as_bytes()).unwrap();
        }
        let result = f.service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4(),"sources":[f.path.join("A"),f.path.join("B")],"mode":"batch","rootId":"root"})).unwrap();
        let mut job = f
            .service
            .catalogue
            .get("job", result["jobId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        f.service
            .run_local_import(&mut job, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        f.service.put_job(&mut job, "completed", None).unwrap();
        let removed = job["productIds"][0].as_str().unwrap().to_owned();
        let retained = job["productIds"][1].as_str().unwrap().to_owned();
        f.service
            .delete_local_products(&json!({"productIds":[removed]}), None)
            .await
            .unwrap();
        let mut pruned = f
            .service
            .catalogue
            .get("job", job["id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(pruned["productIds"], json!([retained]));
        assert_eq!(pruned["completedGroupIndexes"], json!([0]));
        assert_eq!(pruned["localPlan"]["groups"].as_array().unwrap().len(), 1);
        f.service
            .run_local_import(&mut pruned, Path::new("unused"), &AtomicBool::new(false))
            .unwrap();
        assert!(f
            .service
            .catalogue
            .get("product", &removed)
            .unwrap()
            .is_none());
        assert!(f
            .service
            .catalogue
            .get("product", &retained)
            .unwrap()
            .is_some());
        f.finish();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn delete_local_products_rejects_directory_junction_without_touching_target() {
        let f = Fixture::new();
        let (a, dir, _) = f.import("Junction");
        let outside = f.path.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("preserved.txt"), b"outside authored file").unwrap();
        let link = dir.join("junction");
        create_junction(&link, &outside);
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "local_delete_unsafe_path"
        );
        assert_eq!(
            fs::read(outside.join("preserved.txt")).unwrap(),
            b"outside authored file"
        );
        fs::remove_dir(link).unwrap();
        f.finish();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn delete_local_products_rejects_root_ancestor_junction_before_canonicalization() {
        let f = Fixture::new();
        let (a, dir, original) = f.import("LinkedRoot");
        let link = f.path.join("alias");
        create_junction(&link, &f.path.join("library"));
        fs::create_dir_all(f.path.join("library/nested")).unwrap();
        f.service
            .catalogue
            .put(
                "root",
                "root",
                json!({"id":"root","path":link.join("nested"),"status":"online"}),
            )
            .unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({"productIds":[a]}), None)
                .await
                .unwrap_err()
                .to_string(),
            "local_delete_unsafe_path"
        );
        assert!(dir.exists() && original.exists());
        fs::remove_dir(link).unwrap();
        f.finish();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn delete_local_products_cleanup_failure_survives_clear_and_retries_owned_staging() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        let f = Fixture::new();
        let (a, directory, original) = f.import("Cleanup");
        let id = uuid::Uuid::new_v4().to_string();
        let directories = f.service.delete_plan(&[a.clone()], &id).unwrap();
        let (_, staged) = f.service.delete_paths(&id, 0, &directories[0]).unwrap();
        let job = json!({"id":id,"type":JOB_TYPE,"originDeviceId":f.service.catalogue.device_id(),"productIds":[a],"state":"needsAction","localDelete":{"directories":directories,"recordsDeleted":true}});
        fs::rename(&directory, &staged).unwrap();
        f.service
            .catalogue
            .delete_local_products(&[a.clone()], job)
            .unwrap();
        let managed_file = fs::read_dir(&staged)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        // Deny deletion only after staging, so the real Windows cleanup fails
        // without preventing the preceding rename or using a production failpoint.
        let cleanup_blocker = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&managed_file)
            .unwrap();
        assert_eq!(
            f.service
                .delete_local_products(&json!({}), Some(&id))
                .await
                .unwrap_err()
                .to_string(),
            "local_delete_cleanup_failed"
        );
        assert!(f.service.catalogue.get("product", &a).unwrap().is_none() && original.exists());
        assert!(managed_file.exists());
        let job = f
            .service
            .catalogue
            .list("job")
            .unwrap()
            .into_iter()
            .find(|j| j["type"] == JOB_TYPE)
            .unwrap();
        assert_eq!(job["localDelete"]["recordsDeleted"], true);
        f.service
            .catalogue
            .command("clear_activity", json!({}))
            .unwrap();
        assert!(f
            .service
            .catalogue
            .get("job", job["id"].as_str().unwrap())
            .unwrap()
            .is_some());
        drop(cleanup_blocker);
        f.service
            .delete_local_products(&json!({}), job["id"].as_str())
            .await
            .unwrap();
        assert!(!staged.exists() && !directory.exists());
        f.finish();
    }
}
