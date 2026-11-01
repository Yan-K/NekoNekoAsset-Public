use super::*;

#[test]
fn download_scheduler_cas_does_not_revive_cancelled_queue_snapshots() {
    let f = Fixture::new();
    for action in ["require-auth", "clear-auth", "claim"] {
        let mut queued = json!({"id":"authored-scheduler","type":"download","state":"queued","fileIds":[]});
        if action == "clear-auth" {
            queued["stage"] = json!("authRequired");
            queued["errorCode"] = json!("auth_required");
        }
        f.service.catalogue.put("job", "authored-scheduler", queued.clone()).unwrap();
        f.service.cancel_job("authored-scheduler").unwrap();
        let cancelled = f.service.catalogue.get("job", "authored-scheduler").unwrap().unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        let mut stale = queued.clone();
        let (state, error) = match action {
            "require-auth" => {
                stale["stage"] = json!("authRequired");
                ("queued", Some("auth_required"))
            }
            "clear-auth" => {
                stale.as_object_mut().unwrap().remove("stage");
                ("queued", None)
            }
            _ => ("running", None),
        };
        assert!(!f.service.put_queued_download(&queued, &mut stale, state, error).unwrap(), "{action}");
        assert_eq!(f.service.catalogue.get("job", "authored-scheduler").unwrap().unwrap(), cancelled, "{action}");
    }
}

#[test]
fn download_scheduler_cas_claim_keeps_late_cancellation_and_queue_flags() {
    let f = Fixture::new();
    let queued = json!({"id":"authored-scheduler","type":"download","state":"queued","fileIds":[],"cancelRequested":true});
    f.service.catalogue.put("job", "authored-scheduler", queued.clone()).unwrap();
    let mut running = queued.clone();
    let token = f.service.cancellation("authored-scheduler");
    assert!(f.service.put_queued_download(&queued, &mut running, "running", None).unwrap());
    assert_eq!(running["cancelRequested"], true);
    assert_eq!(f.service.cancel_job("authored-scheduler").unwrap()["cancelRequested"], true);
    assert!(token.load(Ordering::Acquire));
    let cancelled_running = f.service.catalogue.get("job", "authored-scheduler").unwrap().unwrap();
    assert_eq!(cancelled_running["state"], "running");
    assert_eq!(cancelled_running["cancelRequested"], true);
    let mut stale = queued.clone();
    assert!(!f.service.put_queued_download(&queued, &mut stale, "running", None).unwrap());
    assert_eq!(f.service.catalogue.get("job", "authored-scheduler").unwrap().unwrap(), cancelled_running);
}

#[test]
fn cancellation_during_restore_and_after_completion_preserves_terminal_facts() {
    let f = Fixture::new();
    let running = json!({"id":"restore","type":"storage:restore_backup","state":"running"});
    let permit = f.service.catalogue.begin_restore_work().unwrap();
    f.service.catalogue.put("job", "restore", running).unwrap();
    assert_eq!(f.service.cancel_job("restore").unwrap()["cancelRequested"], true);
    assert!(f.service.cancellation("restore").load(Ordering::Acquire));
    assert_eq!(f.service.catalogue.get("job", "restore").unwrap().unwrap()["state"], "running");
    let completed = json!({"id":"restore","type":"storage:restore_backup","state":"completed","result":{"restoredFiles":3}});
    f.service.catalogue.put("job", "restore", completed.clone()).unwrap();
    assert_eq!(f.service.cancel_job("restore").unwrap()["cancelRequested"], false);
    assert_eq!(f.service.catalogue.get("job", "restore").unwrap().unwrap(), completed);
    drop(permit);
}

#[tokio::test]
async fn cancel_downloads_stops_only_the_requested_products_active_jobs() {
    let f = Fixture::new();
    for (id, product, state) in [
        ("queued-a", "a", "queued"),
        ("running-a", "a", "running"),
        ("queued-b", "b", "queued"),
        ("done-a", "a", "completed"),
    ] {
        f.service.catalogue.put("job", id, json!({"id":id,"type":"download","productId":product,"state":state,"fileIds":[]})).unwrap();
    }
    let running = f.service.cancellation("running-a");
    let job = |id: &str| f.service.catalogue.get("job", id).unwrap().unwrap();
    assert_eq!(f.service.cancel_downloads(&json!({"productIds":["a"]})).await.unwrap()["cancelled"], 2);
    assert_eq!(job("queued-a")["state"], "cancelled");
    assert_eq!(job("running-a")["state"], "running");
    assert_eq!(job("running-a")["cancelRequested"], true);
    assert!(running.load(Ordering::Acquire));
    assert_eq!(job("queued-b")["state"], "queued");
    assert_eq!(job("done-a")["state"], "completed");
    // Without products every remaining active download stops; a repeat changes nothing.
    assert_eq!(f.service.cancel_downloads(&json!({})).await.unwrap()["cancelled"], 1);
    assert_eq!(job("queued-b")["state"], "cancelled");
    assert_eq!(f.service.cancel_downloads(&json!({})).await.unwrap()["cancelled"], 0);
}

#[tokio::test]
async fn bulk_download_skips_products_without_files_and_one_explicit_product_reports_it() {
    let f = Fixture::new();
    f.seed(1, true);
    f.service.catalogue.put("product", "43", json!({"id":"43","provider":"booth","title":"Authored empty"})).unwrap();
    f.service.catalogue.put("entitlement", "empty", json!({"id":"empty","productId":"43","accountId":"test-account","fileIds":[],"manifestComplete":true})).unwrap();
    let bulk = f.service.enqueue(&json!({"productIds":["42","43"]})).await.unwrap();
    assert_eq!(bulk["jobIds"].as_array().unwrap().len(), 1);
    assert!(f.service.catalogue.related("job", "43").unwrap().is_empty());
    assert!(f.service.catalogue.get("download_intent", "43").unwrap().is_none());
    let single = f.service.enqueue(&json!({"productIds":["43"]})).await.unwrap();
    let job = f.service.catalogue.get("job", single["jobIds"][0].as_str().unwrap()).unwrap().unwrap();
    assert_eq!((job["state"].as_str(), job["errorCode"].as_str()), (Some("needsAction"), Some("manifest_incomplete")));
}

#[tokio::test]
async fn a_cancelling_download_does_not_absorb_an_explicit_new_request() {
    let f = Fixture::new();
    f.seed(1, true);
    let first = f.service.enqueue(&json!({"productIds":["42"]})).await.unwrap();
    let first_id = first["jobIds"][0].as_str().unwrap();
    let mut running = f.service.catalogue.get("job", first_id).unwrap().unwrap();
    running["state"] = json!("running");
    f.service.catalogue.put("job", first_id, running).unwrap();
    f.service.cancel_job(first_id).unwrap();
    let next = f.service.enqueue(&json!({"productIds":["42"]})).await.unwrap();
    assert_ne!(next["jobIds"][0], first["jobIds"][0]);
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 2);
}

#[tokio::test]
async fn stale_automatic_enqueue_rechecks_removed_or_disconnected_accounts_after_queue_pause() {
    for action in ["remove-keep", "disconnect", "remove-products"] {
        let f = Fixture::new();
        f.seed(1, true);
        let gate = f.service.pause_download_queue().await;
        let service = f.service.clone();
        let waiting = tokio::spawn(async move {
            service
                .enqueue(&json!({"productIds":["42"],"automatic":true}))
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "existing queue gate must pause enqueue"
        );
        if action == "disconnect" {
            f.service.catalogue.put("account", "test-account", json!({"id":"test-account","store":"BOOTH","status":"disconnected","disconnected":true})).unwrap();
        } else {
            f.service
                .catalogue
                .remove_account("test-account", action == "remove-products")
                .unwrap();
        }
        drop(gate);
        assert_eq!(
            waiting.await.unwrap().unwrap()["jobIds"],
            json!([]),
            "{action}"
        );
        assert!(f.service.catalogue.list("job").unwrap().is_empty());
        assert!(f
            .service
            .catalogue
            .get("download_intent", "42")
            .unwrap()
            .is_none());
        if action == "remove-products" {
            assert_eq!(
                f.service
                    .enqueue(&json!({"productIds":["42"]}))
                    .await
                    .unwrap_err()
                    .to_string(),
                "product_missing"
            );
        } else {
            assert_eq!(
                f.service
                    .catalogue
                    .command("product_details", json!({"id":"42"}))
                    .unwrap()["status"],
                "notDownloaded"
            );
        }
        // The same ordinary automatic enqueue still works for a present enabled
        // account; stale suppression is not a global scheduling disable.
        f.seed(1, true);
        let result = f
            .service
            .enqueue(&json!({"productIds":["42"],"automatic":true}))
            .await
            .unwrap();
        assert_eq!(result["jobIds"].as_array().unwrap().len(), 1);
        assert_eq!(
            f.service.catalogue.list("job").unwrap()[0]["state"],
            "queued"
        );
        assert_eq!(
            f.service
                .catalogue
                .get("download_intent", "42")
                .unwrap()
                .unwrap()["wantDownload"],
            true
        );
    }
}

struct Fixture {
    path: PathBuf,
    service: StorageService,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("nna-storage-synthetic-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path.join("data")).unwrap();
        let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
        let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
        let service = StorageService::new(catalogue, stores).unwrap();
        std::fs::create_dir_all(path.join("library")).unwrap();
        service
            .catalogue
            .put(
                "root",
                "test-root",
                json!({"id":"test-root","path":path.join("library"),"status":"online"}),
            )
            .unwrap();
        Self { path, service }
    }
    fn seed(&self, count: usize, complete: bool) {
        let c = &self.service.catalogue;
        c.put(
            "product",
            "42",
            json!({"id":"42","title":"Synthetic clothing","category":"Clothes"}),
        )
        .unwrap();
        c.put(
            "account",
            "test-account",
            json!({"id":"test-account","store":"BOOTH","status":"connected"}),
        )
        .unwrap();
        let mut ids = Vec::new();
        for n in 0..count {
            let id = format!("test-file-{n}");
            c.put("remote_file",&id,json!({"id":id,"productId":"42","accountId":"test-account","sourceFileId":n.to_string(),"name":format!("fixture-{n}.unitypackage"),"revision":"r1"})).unwrap();
            ids.push(id);
        }
        c.put("entitlement","test-entitlement",json!({"id":"test-entitlement","productId":"42","accountId":"test-account","fileIds":ids,"manifestComplete":complete})).unwrap();
    }
    fn artifact(&self, index: usize) -> Value {
        let file = self
            .service
            .catalogue
            .get("remote_file", &format!("test-file-{index}"))
            .unwrap()
            .unwrap();
        let id = artifact_id(&file);
        let directory = format!(
            "assets/{}",
            io::readable_component("Synthetic clothing", "42")
        );
        let relative = format!("{directory}/fixture-{index}/fixture-{index}.unitypackage");
        let path = self.path.join("library").join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"synthetic unity package bytes").unwrap();
        let (hash, size) = io::hash_file(&path).unwrap();
        let artifact = json!({"id":id,"fileId":file["id"],"productId":"42","rootId":"test-root","relativePath":relative,"productDirectory":directory,"revision":"r1","hash":hash,"sizeBytes":size,"status":"ready","originalAvailable":true});
        self.service
            .catalogue
            .put("artifact", &id, artifact.clone())
            .unwrap();
        artifact
    }
    fn extracted_archive(&self, index: usize, retained: bool) -> (Value, PathBuf, PathBuf) {
        let mut file = self
            .service
            .catalogue
            .get("remote_file", &format!("test-file-{index}"))
            .unwrap()
            .unwrap();
        file["name"] = json!(format!("ProductA-{index}.zip"));
        self.service
            .catalogue
            .put("remote_file", file["id"].as_str().unwrap(), file.clone())
            .unwrap();
        let product_relative = format!(
            "assets/{}",
            io::readable_component("Synthetic clothing", "42")
        );
        let original_relative =
            format!("{product_relative}/revision-{index}/original/ProductA-{index}.zip");
        let extracted_relative = format!("{product_relative}/extracted/ProductA-{index}");
        let original = self.path.join("library").join(&original_relative);
        let extracted = self.path.join("library").join(&extracted_relative);
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::create_dir_all(extracted.join("empty")).unwrap();
        std::fs::write(extracted.join("model.fbx"), b"verified synthetic model").unwrap();
        if retained {
            std::fs::write(&original, b"synthetic complete archive").unwrap();
        }
        let (hash, size) = io::hash_file(&extracted.join("model.fbx")).unwrap();
        let id = artifact_id(&file);
        let artifact = json!({"id":id,"fileId":file["id"],"productId":"42","rootId":"test-root","relativePath":original_relative,"productDirectory":product_relative,"revision":"r1","hash":io::key("synthetic complete archive"),"sizeBytes":b"synthetic complete archive".len(),"status":"ready","originalAvailable":retained,"extractedPath":extracted_relative,"extractedInventory":[{"relativePath":"model.fbx","hash":hash,"sizeBytes":size},{"relativePath":"empty","hash":"","sizeBytes":0,"isDirectory":true}]});
        self.service
            .catalogue
            .put("artifact", &id, artifact.clone())
            .unwrap();
        (artifact, original, extracted)
    }
    /// Authored bytes begin at the verified-transfer boundary: no HTTP, login or
    /// resolver is involved. Use production binding/guarded publication and
    /// partial cleanup before exercising the actual native extraction lifecycle.
    fn publish_authored_download(
        &self,
        index: usize,
        name: &str,
        bytes: &[u8],
    ) -> (Value, PathBuf) {
        use std::io::Write;
        let file_id = format!("test-file-{index}");
        let mut observed = self
            .service
            .catalogue
            .get("remote_file", &file_id)
            .unwrap()
            .unwrap();
        observed["name"] = json!(name);
        observed["revision"] = json!("unknown");
        observed["requiresContentVerification"] = json!(true);
        observed["sourceObservationToken"] = json!(format!("authored-source-{index}"));
        self.service
            .catalogue
            .put("remote_file", &file_id, observed.clone())
            .unwrap();
        let root = self.path.join("library");
        let part = io::safe_join(
            &root,
            &format!(".nna-partials/pending-{}.part", io::key(&file_id)),
        )
        .unwrap();
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        let _partial = io::PartialGuard::new(part.clone());
        let mut output = std::fs::File::create(&part).unwrap();
        output.write_all(bytes).unwrap();
        output.sync_all().unwrap();
        drop(output);
        let (hash, size) = io::hash_file(&part).unwrap();
        let current = transfer::bind_downloaded_file(
            observed.clone(),
            format!("sha256:{hash}"),
            "contentHash".into(),
            size,
        );
        let directory = self.service.managed_product_directory("42").unwrap();
        let source_directory =
            io::unused_relative(&root, &format!("{directory}/download-{index}")).unwrap();
        let relative = format!("{source_directory}/original/{name}");
        let destination = io::safe_join(&root, &relative).unwrap();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::rename(&part, &destination).unwrap();
        let id = artifact_id(&current);
        let artifact = json!({"id":id,"productId":"42","fileId":file_id,"rootId":"test-root","relativePath":relative,"productDirectory":directory,"revision":current["revision"],"hash":hash,"sizeBytes":size,"status":"downloaded","originalAvailable":true});
        let pending_id = format!("pending-{}", io::key(&file_id));
        self.service
            .catalogue
            .put(
                "artifact",
                &pending_id,
                json!({"id":pending_id,"productId":"42","fileId":file_id,"rootId":"test-root","relativePath":relative,"status":"downloading","originalAvailable":false}),
            )
            .unwrap();
        let prior = self.service.catalogue.get("artifact", &id).unwrap();
        self.service
            .commit_download_records(
                vec![
                    ("remote_file".into(), file_id.clone(), current),
                    ("artifact".into(), id.clone(), artifact.clone()),
                ],
                vec![
                    ("remote_file".into(), file_id.clone(), Some(observed)),
                    ("artifact".into(), id, prior),
                ],
            )
            .unwrap();
        self.service
            .delete_pending_download_record(&file_id)
            .unwrap();
        assert!(self
            .service
            .catalogue
            .get("artifact", &pending_id)
            .unwrap()
            .is_none());
        (artifact, destination)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { /* Open SQLite handles are dropped after this method on Windows. Fixtures are isolated temp files. */
    }
}

fn authored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        let options = zip::write::SimpleFileOptions::default();
        if name.ends_with('/') {
            writer.add_directory(*name, options).unwrap();
        } else {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

fn local_job(f: &Fixture, sources: &[PathBuf], mode: &str) -> Value {
    let result = f.service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4().to_string(),"mode":mode,"sources":sources,"category":"Clothes","name":"Synthetic local product"})).unwrap();
    f.service
        .catalogue
        .get("job", result["jobId"].as_str().unwrap())
        .unwrap()
        .unwrap()
}

#[test]
fn local_import_single_preserves_names_empty_directories_originals_and_truthful_metadata() {
    let f = Fixture::new();
    let first = f.path.join("source-a/衣装");
    let second = f.path.join("source-b/衣装");
    std::fs::create_dir_all(first.join("nested/empty")).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        first.join("nested/model.unitypackage"),
        b"first synthetic payload",
    )
    .unwrap();
    std::fs::write(second.join("notes.txt"), b"second synthetic payload").unwrap();
    let thumbnail = f.path.join("data/media/local-thumbnail.png");
    std::fs::create_dir_all(thumbnail.parent().unwrap()).unwrap();
    std::fs::write(&thumbnail, b"independently authored thumbnail payload").unwrap();
    let accepted = f.service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4().to_string(),"mode":"single","sources":[first,second],"category":"Tool","name":"Synthetic grouped tool","thumbnailPath":"media/local-thumbnail.png"})).unwrap();
    let mut job = f
        .service
        .catalogue
        .get("job", accepted["jobId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    f.service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    let id = job["productIds"][0].as_str().unwrap();
    let product = f.service.catalogue.get("product", id).unwrap().unwrap();
    assert!(id.starts_with("local:"));
    assert_eq!(product["provider"], "local");
    assert_eq!(product["category"], "Tool");
    assert_eq!(
        f.service.catalogue.get("overlay", id).unwrap().unwrap()["category"],
        "Tool"
    );
    let effective = f
        .service
        .catalogue
        .command("product_details", json!({"id":id}))
        .unwrap();
    assert_eq!(effective["category"], "Tool");
    assert_eq!(
        f.service.catalogue.get("overlay", id).unwrap().unwrap()["thumbnailPath"],
        "media/local-thumbnail.png"
    );
    assert_eq!(
        PathBuf::from(effective["previewUrl"].as_str().unwrap()),
        thumbnail
    );
    let visible = f
        .service
        .catalogue
        .command("query", json!({"category":"Tool"}))
        .unwrap();
    assert!(visible["matchedIds"]
        .as_array()
        .unwrap()
        .contains(&json!(id)));
    assert_eq!(
        PathBuf::from(
            visible["products"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .unwrap()["previewUrl"]
                .as_str()
                .unwrap()
        ),
        thumbnail
    );
    assert!(product["price"].is_null());
    assert_eq!(product["acquired"], "");
    assert!(product["importedAt"].is_string());
    assert!(!f.path.join("library/.nna-import-partials").exists());
    assert_eq!(product["localManifest"]["complete"], true);
    assert_eq!(
        product["localManifest"]["fileIds"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(strings(&product["localManifest"]["directories"])
        .iter()
        .any(|d| d.ends_with("衣装/nested/empty")));
    let (files, complete) = f.service.remote_files(id).unwrap();
    assert!(complete);
    assert!(files.iter().all(|v| v["originKind"] == "local"
        && v["accountId"].is_null()
        && v["sourceUrl"].is_null()));
    for artifact in f.service.catalogue.related("artifact", id).unwrap() {
        assert_eq!(artifact["status"], "ready");
        assert_eq!(artifact["originalAvailable"], true);
        io::verify(
            &io::safe_join(
                &f.path.join("library"),
                artifact["relativePath"].as_str().unwrap(),
            )
            .unwrap(),
            artifact["hash"].as_str().unwrap(),
            artifact["sizeBytes"].as_u64().unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        std::fs::read(first.join("nested/model.unitypackage")).unwrap(),
        b"first synthetic payload"
    );
    assert_eq!(
        std::fs::read(second.join("notes.txt")).unwrap(),
        b"second synthetic payload"
    );
    assert_eq!(job["completedProductIds"], job["productIds"]);
    let manifest = serde_json::to_string(&f.service.catalogue.manifest().unwrap()).unwrap();
    assert!(!manifest.contains(&first.to_string_lossy().replace('\\', "\\\\")));
    assert!(f.service.catalogue.list("checkpoint").unwrap().is_empty());
    let backup = f.path.join("local-full.nna-backup");
    f.service
        .disk_operation(
            "export_backup",
            &json!({"path":backup}),
            &AtomicBool::new(false),
        )
        .unwrap();
    let restored = portable::restore_backup(
        &backup,
        &f.path.join("restored-copy"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(restored
        .payload
        .iter()
        .any(|e| e.is_directory && e.relative_path.ends_with("衣装/nested/empty")));
    let restored_thumbnail = restored
        .payload
        .iter()
        .find(|e| e.relative_path == "media/local-thumbnail.png")
        .unwrap();
    assert_eq!(
        std::fs::read(
            f.path
                .join("restored-copy")
                .join(&restored_thumbnail.archive_path)
        )
        .unwrap(),
        b"independently authored thumbnail payload"
    );
}

#[test]
fn local_import_duplicate_reuses_product_and_draft_submission_is_idempotent() {
    let f = Fixture::new();
    let source = f.path.join("original.unitypackage");
    std::fs::write(&source, b"synthetic repeated content").unwrap();
    let draft = uuid::Uuid::new_v4().to_string();
    let args = json!({"draftId":draft,"sources":[source],"category":"Clothes"});
    let first = f.service.prepare_local_import(&args).unwrap();
    assert_eq!(
        first,
        f.service
            .prepare_local_import(&args)
            .unwrap()
            .as_object()
            .map(|v| json!({"jobId":v["jobId"],"productIds":v["productIds"]}))
            .unwrap()
    );
    let mut first_job = f
        .service
        .catalogue
        .get("job", first["jobId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    f.service
        .run_local_import(
            &mut first_job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    let first_id = first_job["productIds"][0].as_str().unwrap();
    f.service
        .catalogue
        .command(
            "overlay",
            json!({"ids":[first_id],"patch":{"category":"Tool","note":"Retain human metadata","thumbnailPath":"media/human-thumbnail.png"}}),
        )
        .unwrap();
    let mut second_job = local_job(&f, &[source.clone()], "single");
    second_job["localPlan"]["metadata"]["thumbnailPath"] = json!("media/duplicate-thumbnail.png");
    f.service
        .run_local_import(
            &mut second_job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(second_job["productIds"], first_job["productIds"]);
    assert_eq!(
        second_job["completedProductIds"],
        first_job["completedProductIds"]
    );
    assert_eq!(f.service.catalogue.list("product").unwrap().len(), 1);
    assert_eq!(f.service.catalogue.list("artifact").unwrap().len(), 1);
    assert_eq!(f.service.catalogue.list("acquisition").unwrap().len(), 1);
    let effective = f
        .service
        .catalogue
        .command("product_details", json!({"id":first_id}))
        .unwrap();
    assert_eq!(effective["category"], "Tool");
    assert_eq!(effective["note"], "Retain human metadata");
    assert_eq!(
        f.service
            .catalogue
            .get("overlay", first_id)
            .unwrap()
            .unwrap()["thumbnailPath"],
        "media/human-thumbnail.png"
    );
    assert!(source.exists());
}

#[test]
fn local_import_restart_uses_verified_copy_when_completed_source_has_gone() {
    let f = Fixture::new();
    let first = f.path.join("a.unitypackage");
    let second = f.path.join("b.txt");
    let held = f.path.join("b-held.txt");
    std::fs::write(&first, b"completed synthetic content").unwrap();
    std::fs::write(&second, b"unfinished synthetic content").unwrap();
    let mut job = local_job(&f, &[first.clone(), second.clone()], "single");
    std::fs::rename(&second, &held).unwrap();
    assert!(f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(f.service.catalogue.list("checkpoint").unwrap().len(), 1);
    assert!(f.service.catalogue.list("product").unwrap().is_empty());
    let completed_copy = f.service.catalogue.list("checkpoint").unwrap()[0].clone();
    let cancel = AtomicBool::new(true);
    assert_eq!(
        io::stable_error(
            &f.service
                .run_local_import(&mut job, Path::new("missing-synthetic-sevenzip"), &cancel)
                .unwrap_err()
        ),
        "cancelled"
    );
    io::verify(
        &io::safe_join(
            &f.path.join("library"),
            completed_copy["relativePath"].as_str().unwrap(),
        )
        .unwrap(),
        completed_copy["hash"].as_str().unwrap(),
        completed_copy["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
    assert!(first.exists() && held.exists());
    let remapped = f.path.join("remapped-library");
    std::fs::create_dir_all(&remapped).unwrap();
    let mut root = f
        .service
        .catalogue
        .get("root", "test-root")
        .unwrap()
        .unwrap();
    root["path"] = json!(remapped);
    f.service.catalogue.put("root", "test-root", root).unwrap();
    // The first copy's checkpoint survives remapping, but bytes are still at
    // the old root. Retry must recover that missing copy from unchanged input.
    assert!(f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false)
        )
        .is_err());
    let remapped_copy =
        io::safe_join(&remapped, completed_copy["relativePath"].as_str().unwrap()).unwrap();
    io::verify(
        &remapped_copy,
        completed_copy["hash"].as_str().unwrap(),
        completed_copy["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
    assert!(first.exists() && held.exists());
    let old_copy = io::safe_join(
        &f.path.join("library"),
        completed_copy["relativePath"].as_str().unwrap(),
    )
    .unwrap();
    io::verify(
        &old_copy,
        completed_copy["hash"].as_str().unwrap(),
        completed_copy["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
    std::fs::write(&remapped_copy, b"synthetic damaged retained bytes").unwrap();
    assert!(f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(
        std::fs::read(&remapped_copy).unwrap(),
        b"synthetic damaged retained bytes"
    );
    assert_eq!(
        std::fs::read(&first).unwrap(),
        b"completed synthetic content"
    );
    std::fs::copy(&old_copy, &remapped_copy).unwrap();
    std::fs::remove_file(&first).unwrap();
    std::fs::rename(&held, &second).unwrap();
    f.service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(f.service.catalogue.list("artifact").unwrap().len(), 2);
    assert!(f.service.catalogue.list("checkpoint").unwrap().is_empty());
}

#[test]
fn local_import_batch_failure_keeps_prior_group_completed_and_managed_archive_original() {
    let f = Fixture::new();
    let folder = f.path.join("a-folder");
    let archive = f.path.join("b-corrupt.zip");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("model.unitypackage"), b"first complete product").unwrap();
    std::fs::write(&archive, b"synthetic invalid zip").unwrap();
    let mut job = local_job(&f, &[folder, archive.clone()], "batch");
    let error = f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(io::stable_error(&error), "archive_tool_unavailable");
    assert_eq!(job["completedProductIds"], json!([job["productIds"][0]]));
    assert_eq!(job["progress"], 50.0);
    let first_artifact = &f
        .service
        .catalogue
        .related("artifact", job["productIds"][0].as_str().unwrap())
        .unwrap()[0];
    assert_eq!(first_artifact["status"], "ready");
    let second_artifact = &f
        .service
        .catalogue
        .related("artifact", job["productIds"][1].as_str().unwrap())
        .unwrap()[0];
    assert_eq!(second_artifact["status"], "extractionFailed");
    assert_eq!(second_artifact["originalAvailable"], true);
    assert_eq!(std::fs::read(&archive).unwrap(), b"synthetic invalid zip");
    f.service
        .put_job(&mut job, "needsAction", Some("archive_tool_unavailable"))
        .unwrap();
    let attention = f
        .service
        .catalogue
        .command("query", json!({"quick":"attention","includeFiles":false}))
        .unwrap();
    assert_eq!(attention["matched"], 1);
    assert_eq!(attention["products"][0]["id"], job["productIds"][1]);
    io::verify(
        &io::safe_join(
            &f.path.join("library"),
            second_artifact["relativePath"].as_str().unwrap(),
        )
        .unwrap(),
        second_artifact["hash"].as_str().unwrap(),
        second_artifact["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
    // Retry must re-run failed extraction rather than mistaking its own marker for a duplicate.
    assert!(f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(job["completedProductIds"].as_array().unwrap().len(), 1);
}

#[test]
fn local_import_batch_failure_does_not_block_later_groups_or_recheck_completed_membership() {
    let f = Fixture::new();
    let archive = f.path.join("a-corrupt.zip");
    let folder_b = f.path.join("b-folder");
    let folder_c = f.path.join("c-folder");
    std::fs::write(&archive, b"authored invalid archive").unwrap();
    for (folder, bytes) in [(&folder_b, b"independent B"), (&folder_c, b"independent C")] {
        std::fs::create_dir(folder).unwrap();
        std::fs::write(folder.join("model.unitypackage"), bytes).unwrap();
    }
    let mut job = local_job(&f, &[archive, folder_b, folder_c], "batch");
    let run = |job: &mut Value| f.service.run_local_import(job, Path::new("missing-synthetic-sevenzip"), &AtomicBool::new(false));
    assert_eq!(io::stable_error(&run(&mut job).unwrap_err()), "archive_tool_unavailable");
    assert_eq!(job["completedGroupIndexes"], json!([1, 2]));
    assert_eq!(job["completedProductIds"].as_array().unwrap().len(), 2);
    assert_eq!(job["result"]["failedGroups"].as_array().unwrap().len(), 1);
    for index in [1, 2] {
        let id = job["productIds"][index].as_str().unwrap();
        let mut product = f.service.catalogue.get("product", id).unwrap().unwrap();
        product["localManifest"]["lastMutationId"] = json!("later-user-membership-change");
        f.service.catalogue.put("product", id, product.clone()).unwrap();
        assert_eq!(f.service.catalogue.related("artifact", id).unwrap()[0]["status"], "ready");
    }
    assert_eq!(io::stable_error(&run(&mut job).unwrap_err()), "archive_tool_unavailable");
    assert_eq!(job["result"]["failedGroups"].as_array().unwrap().len(), 1);
    assert_eq!(job["completedProductIds"].as_array().unwrap().len(), 2);
}

#[test]
#[cfg_attr(not(windows), ignore = "requires explicit NNA_7ZIP on non-Windows")]
fn local_import_preserves_leading_space_names_in_an_archive_with_trailing_data() {
    let f = Fixture::new();
    let source = f.path.join("authored-leading-space.zip");
    let mut bytes = authored_zip(&[(" leading.txt", b"literal space"), ("nested/ inner.txt", b"nested space")]);
    bytes.extend_from_slice(b"authored harmless trailing data");
    std::fs::write(&source, &bytes).unwrap();
    let mut job = local_job(&f, &[source.clone()], "single");
    f.service.run_local_import(&mut job, &native_seven_zip_test_tool(), &AtomicBool::new(false)).unwrap();
    let artifact = &f.service.catalogue.related("artifact", job["productIds"][0].as_str().unwrap()).unwrap()[0];
    assert_eq!(artifact["status"], "ready");
    let extracted = io::safe_join(&f.path.join("library"), artifact["extractedPath"].as_str().unwrap()).unwrap();
    assert_eq!(std::fs::read(extracted.join(" leading.txt")).unwrap(), b"literal space");
    assert_eq!(std::fs::read(extracted.join("nested/ inner.txt")).unwrap(), b"nested space");
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}

#[test]
#[cfg_attr(not(windows), ignore = "requires explicit NNA_7ZIP on non-Windows")]
fn local_import_retry_reuses_only_recorded_unchanged_complete_extraction() {
    let tool = native_seven_zip_test_tool();
    let f = Fixture::new();
    let source = f.path.join("a-complete.zip");
    let failed = f.path.join("b-corrupt.zip");
    std::fs::write(
        &source,
        authored_zip(&[("model.unitypackage", b"authored reusable package")]),
    )
    .unwrap();
    std::fs::write(&failed, b"authored invalid archive").unwrap();
    let original = io::hash_file(&source).unwrap();
    let mut job = local_job(&f, &[source.clone(), failed.clone()], "single");
    assert!(f
        .service
        .run_local_import(&mut job, &tool, &AtomicBool::new(false))
        .is_err());
    let artifacts = f.service.catalogue.list("artifact").unwrap();
    let completed = artifacts
        .iter()
        .find(|a| {
            a["relativePath"]
                .as_str()
                .unwrap()
                .ends_with("a-complete.zip")
        })
        .unwrap()
        .clone();
    assert_eq!(completed["status"], "ready");
    assert_eq!(
        artifacts
            .iter()
            .filter(|a| a["status"] == "extractionFailed")
            .count(),
        1
    );
    let library = f.path.join("library");
    let extracted = io::safe_join(&library, completed["extractedPath"].as_str().unwrap()).unwrap();
    let member = extracted.join("model.unitypackage");
    let missing_tool = f.path.join("missing-synthetic-sevenzip");
    // The sibling still fails, but this known successful ZIP must stay Ready
    // even when invoking the extractor is impossible: no new output was written.
    let error = f
        .service
        .run_local_import(&mut job, &missing_tool, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(io::stable_error(&error), "archive_tool_unavailable");
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", completed["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        completed
    );
    assert_eq!(
        std::fs::read(&member).unwrap(),
        b"authored reusable package"
    );
    assert!(!library.join(".nna-import-staging").exists());

    // A dropped old volume must prevent reuse even if every currently supplied
    // member still matches the saved bytes and inventory.
    let mut old_volume = completed.clone();
    old_volume["id"] = json!("authored-dropped-volume");
    old_volume["fileId"] = json!("authored-dropped-file");
    let old_relative = Path::new(completed["relativePath"].as_str().unwrap())
        .with_file_name("a-complete.z01")
        .to_string_lossy()
        .replace('\\', "/");
    old_volume["relativePath"] = json!(old_relative);
    f.service
        .catalogue
        .put("artifact", "authored-dropped-volume", old_volume)
        .unwrap();
    assert!(f
        .service
        .run_local_import(&mut job, &missing_tool, &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", completed["id"].as_str().unwrap())
            .unwrap()
            .unwrap()["status"],
        "extractionFailed"
    );
    f.service
        .catalogue
        .delete("artifact", "authored-dropped-volume")
        .unwrap();

    // An existing output directory alone is not proof of successful publication.
    let mut no_inventory = completed.clone();
    no_inventory
        .as_object_mut()
        .unwrap()
        .remove("extractedInventory");
    f.service
        .catalogue
        .put("artifact", completed["id"].as_str().unwrap(), no_inventory)
        .unwrap();
    assert!(f
        .service
        .run_local_import(&mut job, &missing_tool, &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", completed["id"].as_str().unwrap())
            .unwrap()
            .unwrap()["status"],
        "extractionFailed"
    );

    // User-modified output still takes the existing full-extraction fallback,
    // leaving that tree intact when extraction cannot run.
    f.service
        .catalogue
        .put(
            "artifact",
            completed["id"].as_str().unwrap(),
            completed.clone(),
        )
        .unwrap();
    std::fs::write(&member, b"authored retained user edit").unwrap();
    assert!(f
        .service
        .run_local_import(&mut job, &missing_tool, &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", completed["id"].as_str().unwrap())
            .unwrap()
            .unwrap()["status"],
        "extractionFailed"
    );
    assert_eq!(
        std::fs::read(&member).unwrap(),
        b"authored retained user edit"
    );
    assert_eq!(io::hash_file(&source).unwrap(), original);
    io::verify(
        &io::safe_join(&library, completed["relativePath"].as_str().unwrap()).unwrap(),
        &original.0,
        original.1,
    )
    .unwrap();
    assert_eq!(std::fs::read(&failed).unwrap(), b"authored invalid archive");
}

#[test]
fn local_import_rejected_publication_does_not_skip_group_on_retry() {
    let f = Fixture::new();
    let first = f.path.join("first-folder");
    let second = f.path.join("second-folder");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let first_input = first.join("model.unitypackage");
    let second_input = second.join("model.unitypackage");
    let held = f.path.join("held-second.unitypackage");
    std::fs::write(&first_input, b"prior completed synthetic group").unwrap();
    std::fs::write(&second_input, b"retry unpublished synthetic group").unwrap();
    let mut job = local_job(&f, &[first, second], "batch");
    std::fs::rename(&second_input, &held).unwrap();
    assert!(f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false)
        )
        .is_err());
    let completed = job["completedProductIds"].clone();
    let completed_indexes = job["completedGroupIndexes"].clone();
    assert_eq!(completed_indexes, json!([0]));
    let first_id = job["productIds"][0].as_str().unwrap().to_owned();
    let prior_product = f
        .service
        .catalogue
        .get("product", &first_id)
        .unwrap()
        .unwrap();
    std::fs::rename(&held, &second_input).unwrap();
    // Force a deterministic catalogue publication rejection after copying the
    // next group, without a filesystem failpoint or a new test framework.
    job["localPlan"]["metadata"]["category"] = json!("invalid synthetic category");
    let error = f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(error.to_string().contains("catalogue.invalid_category"));
    assert_eq!(job["completedProductIds"], completed);
    assert_eq!(job["completedGroupIndexes"], completed_indexes);
    assert_eq!(f.service.catalogue.list("product").unwrap().len(), 1);
    assert!(f
        .service
        .catalogue
        .get("product", job["productIds"][1].as_str().unwrap())
        .unwrap()
        .is_none());
    assert_eq!(f.service.catalogue.list("checkpoint").unwrap().len(), 1);
    // Match process_local_job's terminal write and reopen its durable job.
    f.service
        .put_job(&mut job, "needsAction", Some("storage_failed"))
        .unwrap();
    let mut retry = f
        .service
        .catalogue
        .get("job", job["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(retry["completedGroupIndexes"], json!([0]));
    retry["localPlan"]["metadata"]["category"] = json!("Clothes");
    f.service
        .run_local_import(
            &mut retry,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(retry["completedProductIds"], retry["productIds"]);
    assert_eq!(retry["completedGroupIndexes"], json!([0, 1]));
    assert_eq!(f.service.catalogue.list("product").unwrap().len(), 2);
    assert_eq!(
        f.service
            .catalogue
            .get("product", &first_id)
            .unwrap()
            .unwrap(),
        prior_product
    );
    assert!(f.service.catalogue.list("checkpoint").unwrap().is_empty());
    assert_eq!(
        std::fs::read(first_input).unwrap(),
        b"prior completed synthetic group"
    );
    assert_eq!(
        std::fs::read(second_input).unwrap(),
        b"retry unpublished synthetic group"
    );
}

#[test]
fn local_import_changed_source_refuses_publication_and_conflicting_draft_resubmission() {
    let f = Fixture::new();
    let source = f.path.join("changed.unitypackage");
    std::fs::write(&source, b"planned synthetic bytes").unwrap();
    let args =
        json!({"draftId":uuid::Uuid::new_v4().to_string(),"sources":[source],"category":"Base"});
    let result = f.service.prepare_local_import(&args).unwrap();
    let mut changed = args.clone();
    changed["name"] = json!("Changed form for an accepted draft");
    assert!(f
        .service
        .prepare_local_import(&changed)
        .unwrap_err()
        .to_string()
        .contains("local_draft_changed"));
    let mut job = f
        .service
        .catalogue
        .get("job", result["jobId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    std::fs::write(&source, b"different length synthetic payload").unwrap();
    let error = f
        .service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(error.to_string().contains("local_source_changed"));
    assert!(f.service.catalogue.list("product").unwrap().is_empty());
    assert!(f.service.catalogue.list("artifact").unwrap().is_empty());
    assert_eq!(
        std::fs::read(&source).unwrap(),
        b"different length synthetic payload"
    );
}

#[test]
#[cfg_attr(not(windows), ignore = "requires explicit NNA_7ZIP on non-Windows")]
fn local_import_real_7zip_preserves_managed_zip_nested_archive_and_unitypackage() {
    use std::io::Write;
    let tool = native_seven_zip_test_tool();
    let f = Fixture::new();
    let source = f.path.join("local-source.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&source).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    writer.add_directory("衣装/empty/", options).unwrap();
    writer
        .start_file("衣装/model.unitypackage", options)
        .unwrap();
    writer
        .write_all(b"synthetic unity package retained")
        .unwrap();
    writer.start_file("衣装/nested.zip", options).unwrap();
    writer
        .write_all(b"synthetic nested zip remains unexpanded")
        .unwrap();
    writer.finish().unwrap();
    let original = io::hash_file(&source).unwrap();
    let mut job = local_job(&f, &[source.clone()], "single");
    f.service
        .run_local_import(&mut job, &tool, &AtomicBool::new(false))
        .unwrap();
    let artifact = &f.service.catalogue.list("artifact").unwrap()[0];
    assert_eq!(artifact["status"], "ready");
    assert_eq!(artifact["originalAvailable"], true);
    assert_eq!(io::hash_file(&source).unwrap(), original);
    io::verify(
        &io::safe_join(
            &f.path.join("library"),
            artifact["relativePath"].as_str().unwrap(),
        )
        .unwrap(),
        &original.0,
        original.1,
    )
    .unwrap();
    let extracted = f
        .service
        .verify_extracted_at(artifact, &f.path.join("library"), &AtomicBool::new(false))
        .unwrap();
    assert!(extracted.join("衣装/empty").is_dir());
    assert_eq!(
        std::fs::read(extracted.join("衣装/model.unitypackage")).unwrap(),
        b"synthetic unity package retained"
    );
    assert_eq!(
        std::fs::read(extracted.join("衣装/nested.zip")).unwrap(),
        b"synthetic nested zip remains unexpanded"
    );
    assert!(!extracted.join("衣装/nested").exists());

    // Explicit verification finds a missing member; Retry must publish a new
    // complete tree without replacing the damaged tree or either archive copy.
    std::fs::remove_file(extracted.join("衣装/model.unitypackage")).unwrap();
    std::fs::write(
        extracted.join("user-retained.txt"),
        b"synthetic retained edit",
    )
    .unwrap();
    f.service.put_job(&mut job, "completed", None).unwrap();
    f.service
        .disk_operation(
            "verify_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        f.service.catalogue.list("artifact").unwrap()[0]["status"],
        "missing"
    );
    let product = job["productIds"][0].as_str().unwrap().to_owned();
    let job_id = job["id"].as_str().unwrap().to_owned();
    assert_eq!(
        f.service
            .retry_local_product_locked(&product, &f.service.catalogue.list("job").unwrap())
            .unwrap(),
        Some(job_id.clone())
    );
    job = f.service.catalogue.get("job", &job_id).unwrap().unwrap();
    f.service
        .run_local_import(&mut job, &tool, &AtomicBool::new(false))
        .unwrap();
    let repaired = f.service.catalogue.list("artifact").unwrap()[0].clone();
    assert_eq!(repaired["status"], "ready");
    assert_eq!(repaired["originalAvailable"], true);
    assert_ne!(repaired["extractedPath"], artifact["extractedPath"]);
    let repaired_path = f
        .service
        .verify_extracted_at(&repaired, &f.path.join("library"), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        std::fs::read(repaired_path.join("衣装/model.unitypackage")).unwrap(),
        b"synthetic unity package retained"
    );
    assert!(repaired_path.join("衣装/empty").is_dir());
    assert!(!repaired_path.join("衣装/nested").exists());
    assert_eq!(
        std::fs::read(extracted.join("user-retained.txt")).unwrap(),
        b"synthetic retained edit"
    );
    assert!(!extracted.join("衣装/model.unitypackage").exists());
    assert_eq!(io::hash_file(&source).unwrap(), original);
    io::verify(
        &io::safe_join(
            &f.path.join("library"),
            repaired["relativePath"].as_str().unwrap(),
        )
        .unwrap(),
        &original.0,
        original.1,
    )
    .unwrap();
    assert_eq!(f.service.catalogue.list("product").unwrap().len(), 1);
    assert_eq!(job["completedProductIds"], json!([product]));

    // Retrying a group with a recorded usable repair must reuse that tree,
    // even though its original extraction destination remains damaged.
    let mut retry_artifact = repaired.clone();
    retry_artifact["status"] = json!("extractionFailed");
    f.service
        .catalogue
        .put("artifact", repaired["id"].as_str().unwrap(), retry_artifact)
        .unwrap();
    f.service.put_job(&mut job, "completed", None).unwrap();
    f.service
        .retry_local_product_locked(&product, &f.service.catalogue.list("job").unwrap())
        .unwrap();
    job = f.service.catalogue.get("job", &job_id).unwrap().unwrap();
    f.service
        .run_local_import(&mut job, &tool, &AtomicBool::new(false))
        .unwrap();
    let repeated = f.service.catalogue.list("artifact").unwrap()[0].clone();
    assert_eq!(repeated["status"], "ready");
    assert_eq!(repeated["extractedPath"], repaired["extractedPath"]);
    assert_eq!(
        std::fs::read(extracted.join("user-retained.txt")).unwrap(),
        b"synthetic retained edit"
    );
}

#[tokio::test]
async fn local_import_retry_clears_cancellation_without_creating_another_job() {
    let f = Fixture::new();
    let source = f.path.join("cancelled.unitypackage");
    std::fs::write(&source, b"synthetic cancelled import").unwrap();
    let mut job = local_job(&f, &[source], "single");
    let id = job["id"].as_str().unwrap().to_owned();
    job["cancelRequested"] = json!(true);
    f.service.put_job(&mut job, "cancelled", None).unwrap();
    f.service.cancellation(&id).store(true, Ordering::Release);
    let response = f.service.retry_local_import(&id).await.unwrap();
    assert_eq!(response["jobId"], id);
    let mut resumed = f.service.catalogue.get("job", &id).unwrap().unwrap();
    assert_eq!(resumed["state"], "queued");
    assert_eq!(resumed["cancelRequested"], false);
    assert!(!f.service.cancellation(&id).load(Ordering::Acquire));
    f.service
        .run_local_import(
            &mut resumed,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 1);
    assert_eq!(f.service.catalogue.list("product").unwrap().len(), 1);
}

#[tokio::test]
async fn local_import_download_routes_to_original_job_repairs_missing_copy_and_never_networks() {
    let f = Fixture::new();
    let first = f.path.join("a-kept.unitypackage");
    let second = f.path.join("b-repaired.txt");
    std::fs::write(&first, b"positive managed copy survives").unwrap();
    std::fs::write(&second, b"original available for repair").unwrap();
    let mut job = local_job(&f, &[first.clone(), second], "single");
    f.service
        .run_local_import(
            &mut job,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    f.service.put_job(&mut job, "completed", None).unwrap();
    let product = job["productIds"][0].as_str().unwrap().to_owned();
    let artifacts = f.service.catalogue.related("artifact", &product).unwrap();
    let missing = artifacts
        .iter()
        .find(|a| {
            a["relativePath"]
                .as_str()
                .unwrap()
                .ends_with("b-repaired.txt")
        })
        .unwrap();
    std::fs::remove_file(&first).unwrap();
    std::fs::remove_file(
        io::safe_join(
            &f.path.join("library"),
            missing["relativePath"].as_str().unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let response = f
        .service
        .enqueue(&json!({"productIds":[product]}))
        .await
        .unwrap();
    assert_eq!(response["jobIds"], json!([job["id"]]));
    let mut resumed = f
        .service
        .catalogue
        .get("job", job["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(resumed["completedProductIds"], json!([]));
    f.service
        .run_local_import(
            &mut resumed,
            Path::new("missing-synthetic-sevenzip"),
            &AtomicBool::new(false),
        )
        .unwrap();
    f.service.put_job(&mut resumed, "completed", None).unwrap();
    assert_eq!(
        f.service
            .enqueue(&json!({"productIds":[product]}))
            .await
            .unwrap()["jobIds"],
        json!([])
    );
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 1);
    assert!(f
        .service
        .catalogue
        .list("download_intent")
        .unwrap()
        .is_empty());
    // Portable metadata has no device source plan; it must request explicit local sources.
    f.service
        .catalogue
        .delete("job", job["id"].as_str().unwrap())
        .unwrap();
    let missing = f
        .service
        .catalogue
        .get("artifact", missing["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    std::fs::remove_file(
        io::safe_join(
            &f.path.join("library"),
            missing["relativePath"].as_str().unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let error = f
        .service
        .enqueue(&json!({"productIds":[product]}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("local_source_required"));
    assert!(f.service.catalogue.list("job").unwrap().is_empty());
}

#[test]
fn local_import_rejects_empty_batch_loose_overlap_and_library_ancestor_without_publication() {
    let f = Fixture::new();
    let empty = f.path.join("empty");
    std::fs::create_dir_all(empty.join("nested")).unwrap();
    assert!(f
        .service
        .prepare_local_import(
            &json!({"draftId":uuid::Uuid::new_v4().to_string(),"sources":[empty]})
        )
        .unwrap_err()
        .to_string()
        .contains("no_import_files"));
    let loose = f.path.join("loose.txt");
    std::fs::write(&loose, b"loose synthetic file").unwrap();
    assert!(f
        .service
        .prepare_local_import(
            &json!({"draftId":uuid::Uuid::new_v4().to_string(),"mode":"batch","sources":[loose]})
        )
        .unwrap_err()
        .to_string()
        .contains("local_batch_requires_folder_or_zip"));
    assert!(f
        .service
        .prepare_local_import(
            &json!({"draftId":uuid::Uuid::new_v4().to_string(),"sources":[f.path]})
        )
        .unwrap_err()
        .to_string()
        .contains("local_source_contains_library"));
    let nested = f.path.join("tree");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("file.txt"), b"nested file").unwrap();
    assert!(f.service.prepare_local_import(&json!({"draftId":uuid::Uuid::new_v4().to_string(),"sources":[nested,nested.join("file.txt")]})).unwrap_err().to_string().contains("local_source_overlap"));
    assert!(f.service.catalogue.list("job").unwrap().is_empty());
    assert!(f.service.catalogue.list("product").unwrap().is_empty());
}

#[test]
fn cleaned_archives_open_their_own_verified_extraction_and_product_folder_spans_variants() {
    let f = Fixture::new();
    f.seed(2, true);
    let (_, _, first) = f.extracted_archive(0, false);
    let (_, _, second) = f.extracted_archive(1, false);
    for (id, expected) in [("test-file-0", first), ("test-file-1", second)] {
        assert_eq!(
            f.service
                .resolve_open_target("open_file", &json!({"fileId":id}))
                .unwrap(),
            expected
        );
    }
    let expected = f
        .path
        .join("library/assets")
        .join(io::readable_component("Synthetic clothing", "42"));
    assert_eq!(
        f.service
            .resolve_open_target("open_product_folder", &json!({"productId":"42"}))
            .unwrap(),
        expected
    );
    let availability = f
        .service
        .product_availability(&json!({"productId":"42"}))
        .unwrap();
    assert_eq!(availability["folderAvailable"], true);
    let ids = strings(&availability["openableFileIds"]);
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"test-file-0".to_owned()) && ids.contains(&"test-file-1".to_owned()));
}

#[test]
fn archive_open_prefers_completed_directory_and_integrity_remains_explicit() {
    let f = Fixture::new();
    f.seed(1, true);
    let (artifact, original, extracted) = f.extracted_archive(0, true);
    let args = json!({"fileId":"test-file-0"});
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    std::fs::write(&original, b"corrupt original bytes").unwrap();
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    assert_eq!(
        strings(
            &f.service
                .product_availability(&json!({"productId":"42"}))
                .unwrap()["openableFileIds"]
        ),
        vec!["test-file-0".to_owned()]
    );
    // Opening a completed directory does not read its payload. Explicit
    // verification continues to detect edits and missing members.
    std::fs::write(extracted.join("model.fbx"), b"modified synthetic model").unwrap();
    assert!(f
        .service
        .verify_extracted_at(&artifact, &f.path.join("library"), &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    // The original is a fallback only when the extracted directory is absent.
    std::fs::write(&original, b"synthetic complete archive").unwrap();
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    std::fs::remove_file(extracted.join("model.fbx")).unwrap();
    assert!(f
        .service
        .verify_extracted_at(&artifact, &f.path.join("library"), &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    assert_eq!(
        strings(
            &f.service
                .product_availability(&json!({"productId":"42"}))
                .unwrap()["openableFileIds"]
        ),
        vec!["test-file-0".to_owned()]
    );
    std::fs::write(extracted.join("model.fbx"), b"verified synthetic model").unwrap();
    let moved = extracted.with_extension("moved");
    std::fs::rename(&extracted, &moved).unwrap();
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        original
    );
    std::fs::rename(&moved, &extracted).unwrap();
    // A stale availability flag after cleanup does not hide a verified usable extraction.
    std::fs::remove_file(original).unwrap();
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    // Non-archive files continue to open the verified retained file itself.
    let artifact = f.artifact(0);
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        f.path
            .join("library")
            .join(artifact["relativePath"].as_str().unwrap())
    );
}

#[test]
fn extracted_target_requires_completed_state_and_explicit_verification_rejects_bad_inventory() {
    let f = Fixture::new();
    f.seed(1, true);
    let (mut artifact, _, extracted) = f.extracted_archive(0, false);
    let args = json!({"fileId":"test-file-0"});
    let product = json!({"productId":"42"});
    let id = artifact["id"].as_str().unwrap().to_owned();
    for status in [
        "queued",
        "downloading",
        "extracting",
        "unknown",
        "missing",
        "extractionFailed",
    ] {
        artifact["status"] = json!(status);
        f.service
            .catalogue
            .put("artifact", &id, artifact.clone())
            .unwrap();
        assert!(f.service.resolve_open_target("open_file", &args).is_err());
        assert!(
            strings(&f.service.product_availability(&product).unwrap()["openableFileIds"])
                .is_empty()
        );
    }
    artifact["status"] = json!("ready");
    f.service
        .catalogue
        .put("artifact", &id, artifact.clone())
        .unwrap();
    // Same-size edits are checked on explicit verification, not each directory open.
    std::fs::write(extracted.join("model.fbx"), b"modified synthetic model").unwrap();
    assert!(f
        .service
        .verify_extracted_at(&artifact, &f.path.join("library"), &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        f.service.resolve_open_target("open_file", &args).unwrap(),
        extracted
    );
    std::fs::remove_file(extracted.join("model.fbx")).unwrap();
    assert!(f
        .service
        .verify_extracted_at(&artifact, &f.path.join("library"), &AtomicBool::new(false))
        .is_err());
    assert_eq!(
        strings(&f.service.product_availability(&product).unwrap()["openableFileIds"]),
        vec!["test-file-0"]
    );
    artifact["extractedInventory"] = json!([]);
    f.service
        .catalogue
        .put("artifact", &id, artifact.clone())
        .unwrap();
    assert!(f.service.resolve_open_target("open_file", &args).is_err());
    artifact["extractedInventory"] =
        json!([{"relativePath":"../escape.bin","hash":io::key("escape"),"sizeBytes":6}]);
    // Supplemental inventory is accepted as metadata. The content verifier
    // owns its member-path check; opening consumes only the safe base path.
    f.service
        .catalogue
        .put("artifact", &id, artifact.clone())
        .unwrap();
    let before = f.service.catalogue.get("artifact", &id).unwrap().unwrap();
    assert!(f
        .service
        .verify_extracted_at(&artifact, &f.path.join("library"), &AtomicBool::new(false))
        .is_err());
    // The folder action doesn't consume inventory paths. Its own target must
    // still stay contained in the mapped root.
    artifact["extractedInventory"] =
        json!([{"relativePath":"model.fbx","hash":io::key("escape"),"sizeBytes":6}]);
    artifact["extractedPath"] = json!("../escape");
    let error = f
        .service
        .catalogue
        .put("artifact", &id, artifact.clone())
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("catalogue.invalid_relative_path"));
    assert_eq!(
        f.service.catalogue.get("artifact", &id).unwrap().unwrap(),
        before
    );
    assert!(f
        .service
        .verify_extracted_at(&artifact, &f.path.join("library"), &AtomicBool::new(false))
        .is_err());
    // A failed explicit verification records the actionable fault before the
    // cheap open action consults its durable completed-state claim.
    let verification = f
        .service
        .disk_operation(
            "verify_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(verification["missing"], 1);
    assert!(f.service.resolve_open_target("open_file", &args).is_err());
    assert!(
        strings(&f.service.product_availability(&product).unwrap()["openableFileIds"]).is_empty()
    );
}

#[test]
fn product_folder_requires_recorded_containing_namespace_without_guessing_paths() {
    let f = Fixture::new();
    f.seed(1, true);
    let (mut artifact, _, extracted) = f.extracted_archive(0, false);
    let expected = f
        .path
        .join("library/assets")
        .join(io::readable_component("Synthetic clothing", "42"));
    assert_eq!(
        f.service
            .resolve_open_target("open_product_folder", &json!({"productId":"42"}))
            .unwrap(),
        expected
    );
    assert_eq!(
        f.service
            .resolve_open_target("open_file", &json!({"fileId":"test-file-0"}))
            .unwrap(),
        extracted
    );
    artifact["productDirectory"] = json!("assets/unrelated-product");
    f.service
        .catalogue
        .put(
            "artifact",
            artifact["id"].as_str().unwrap(),
            artifact.clone(),
        )
        .unwrap();
    assert!(f
        .service
        .resolve_open_target("open_product_folder", &json!({"productId":"42"}))
        .is_err());
    assert_eq!(
        f.service
            .product_availability(&json!({"productId":"42"}))
            .unwrap()["folderAvailable"],
        false
    );
}

#[test]
fn open_targets_follow_current_root_mapping_and_refuse_offline_or_unknown_revision() {
    let f = Fixture::new();
    f.seed(1, true);
    let (_, _, extracted) = f.extracted_archive(0, false);
    let canonical = f.path.join("library").canonicalize().unwrap();
    f.service
        .catalogue
        .put(
            "root",
            "test-root",
            json!({"id":"test-root","path":canonical,"status":"online"}),
        )
        .unwrap();
    let args = json!({"fileId":"test-file-0"});
    assert_eq!(
        f.service
            .resolve_open_target("open_file", &args)
            .unwrap()
            .canonicalize()
            .unwrap(),
        extracted.canonicalize().unwrap()
    );
    std::fs::rename(f.path.join("library"), f.path.join("offline-library")).unwrap();
    assert!(f.service.resolve_open_target("open_file", &args).is_err());
    assert_eq!(
        f.service
            .product_availability(&json!({"productId":"42"}))
            .unwrap()["rootStatus"],
        "offline"
    );
    std::fs::rename(f.path.join("offline-library"), f.path.join("library")).unwrap();
    let mut remote = f
        .service
        .catalogue
        .get("remote_file", "test-file-0")
        .unwrap()
        .unwrap();
    remote["revision"] = json!("unknown");
    f.service
        .catalogue
        .put("remote_file", "test-file-0", remote)
        .unwrap();
    assert!(f.service.resolve_open_target("open_file", &args).is_err());
    assert!(strings(
        &f.service
            .product_availability(&json!({"productId":"42"}))
            .unwrap()["openableFileIds"]
    )
    .is_empty());
}

#[test]
fn preferences_and_revision_identity_are_deterministic() {
    assert_eq!(download_count(&Value::Null), 3);
    assert_eq!(download_count(&json!(25)), 6);
    assert_eq!(download_count(&json!(0)), 1);
    assert_eq!(download_count(&json!(7)), 6);
    assert_eq!(download_count(&json!(4)), 4);
    let a = json!({"id":"f","revision":"r1","sourceUrl":"https://booth.pm/x"});
    let mut b = a.clone();
    b["sourceUrl"] = json!("https://booth.pm/y");
    assert_eq!(artifact_id(&a), artifact_id(&b));
    b["revision"] = json!("r2");
    assert_ne!(artifact_id(&a), artifact_id(&b));
}

#[tokio::test]
async fn single_file_enqueue_is_bounded_and_preserves_product_intent_and_siblings() {
    let f = Fixture::new();
    f.seed(2, false);
    let retained = f.artifact(1);
    let original = io::safe_join(
        &f.path.join("library"),
        retained["relativePath"].as_str().unwrap(),
    )
    .unwrap();
    let result = f
        .service
        .enqueue_file(&json!({"fileId":"test-file-0","redownload":true}))
        .await
        .unwrap();
    let id = result["jobIds"][0].as_str().unwrap();
    let job = f.service.catalogue.get("job", id).unwrap().unwrap();
    assert_eq!(job["selectedFileIds"], json!(["test-file-0"]));
    assert_eq!(job["fileIds"], json!(["test-file-0"]));
    assert_eq!(job["redownload"], true);
    assert_eq!(job["manifestComplete"], false);
    assert!(f
        .service
        .catalogue
        .get("download_intent", "42")
        .unwrap()
        .is_none());
    assert_eq!(
        f.service
            .enqueue_file(&json!({"fileId":"test-file-0","redownload":true}))
            .await
            .unwrap()["jobIds"][0],
        id
    );
    let sibling_job = f
        .service
        .enqueue_file(&json!({"fileId":"test-file-1"}))
        .await
        .unwrap();
    assert_ne!(sibling_job["jobIds"][0], id);
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 2);
    assert_eq!(
        selected_download_files(
            f.service.remote_files("42").unwrap().0,
            &job["selectedFileIds"]
        )
        .unwrap()
        .len(),
        1
    );
    assert!(selected_download_files(
        f.service.remote_files("42").unwrap().0,
        &json!(["not-in-manifest"])
    )
    .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", retained["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        retained
    );
    io::verify(
        &original,
        retained["hash"].as_str().unwrap(),
        retained["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn independent_file_clicks_queue_behind_running_work_and_all_scope_reuses_coverage() {
    let f = Fixture::new();
    f.seed(3, true);
    let first_args = json!({"fileId":"test-file-0"});
    let second_args = json!({"fileId":"test-file-1"});
    let (first, second) = tokio::join!(
        f.service.enqueue_file(&first_args),
        f.service.enqueue_file(&second_args)
    );
    let first = first.unwrap()["jobIds"][0].as_str().unwrap().to_owned();
    let second = second.unwrap()["jobIds"][0].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    let mut running = f.service.catalogue.get("job", &first).unwrap().unwrap();
    f.service.put_job(&mut running, "running", None).unwrap();
    assert_eq!(
        f.service
            .enqueue_file(&json!({"fileId":"test-file-0"}))
            .await
            .unwrap()["jobIds"],
        json!([first])
    );
    assert_eq!(
        f.service
            .enqueue_file(&json!({"fileId":"test-file-1"}))
            .await
            .unwrap()["jobIds"],
        json!([second])
    );
    let all = f
        .service
        .enqueue(&json!({"productIds":["42"]}))
        .await
        .unwrap();
    let all_id = all["jobIds"][0].as_str().unwrap();
    let all_job = f.service.catalogue.get("job", all_id).unwrap().unwrap();
    assert!(all_job["selectedFileIds"].is_null());
    assert_eq!(all_job["fileIds"].as_array().unwrap().len(), 3);
    assert_eq!(
        f.service
            .enqueue(&json!({"productIds":["42"]}))
            .await
            .unwrap(),
        all
    );
    assert_eq!(
        f.service
            .enqueue_file(&json!({"fileId":"test-file-2"}))
            .await
            .unwrap(),
        all
    );
    assert_eq!(
        f.service
            .enqueue_file(&json!({"fileId":"test-file-2","redownload":true}))
            .await
            .unwrap_err()
            .to_string(),
        "storage_busy"
    );
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 3);
    assert_eq!(
        f.service
            .catalogue
            .get("download_intent", "42")
            .unwrap()
            .unwrap()["wantDownload"],
        true
    );
}

#[test]
fn source_sync_reduces_only_matching_provider_launches_and_restores_saved_cap() {
    use crate::stores::StoreProvider;
    for (provider, sync, update) in [
        (StoreProvider::Booth, "boothSync", "boothUpdate"),
        (StoreProvider::Gumroad, "gumroadSync", "gumroadUpdate"),
        (StoreProvider::Jinxxy, "jinxxySync", "jinxxyUpdate"),
    ] {
        for kind in [sync, update] {
            let mut jobs = vec![json!({"type":kind,"state":"queued"})];
            assert_eq!(adaptive_download_count(&json!(6), Some(provider), &jobs), 1);
            jobs[0]["state"] = json!("running");
            assert_eq!(adaptive_download_count(&json!(4), Some(provider), &jobs), 1);
            for other in [
                StoreProvider::Booth,
                StoreProvider::Gumroad,
                StoreProvider::Jinxxy,
            ] {
                if other != provider {
                    assert_eq!(adaptive_download_count(&json!(4), Some(other), &jobs), 4);
                }
            }
            for state in ["completed", "failed", "cancelled", "needsAction"] {
                jobs[0]["state"] = json!(state);
                assert_eq!(adaptive_download_count(&json!(4), Some(provider), &jobs), 4);
            }
        }
    }
    let unrelated = vec![
        json!({"type":"boothLogin","state":"running"}),
        json!({"type":"download","store":"BOOTH","state":"running"}),
        json!({"type":"storage:import_local","state":"queued"}),
    ];
    assert_eq!(
        adaptive_download_count(&Value::Null, Some(StoreProvider::Booth), &unrelated),
        3
    );
}

#[test]
fn download_commit_cannot_overwrite_new_source_observation_and_retains_verified_bytes() {
    let f = Fixture::new();
    f.seed(1, true);
    let retained = f.artifact(0);
    let retained_path = io::safe_join(
        &f.path.join("library"),
        retained["relativePath"].as_str().unwrap(),
    )
    .unwrap();
    let mut observed = f
        .service
        .catalogue
        .get("remote_file", "test-file-0")
        .unwrap()
        .unwrap();
    observed["sourceObservationToken"] = json!("authored-sync-old");
    observed["requiresContentVerification"] = json!(true);
    f.service
        .catalogue
        .put("remote_file", "test-file-0", observed.clone())
        .unwrap();
    let pending_path = f.path.join("library/verified-new.bin");
    std::fs::write(&pending_path, b"verified actual transfer bytes").unwrap();
    let (hash, size) = io::hash_file(&pending_path).unwrap();
    let pending = json!({"id":"pending-authored-transfer","productId":"42","fileId":"test-file-0","rootId":"test-root","relativePath":"verified-new.bin","originalAvailable":true,"status":"downloaded","hash":hash,"sizeBytes":size});
    f.service
        .catalogue
        .put("artifact", "pending-authored-transfer", pending.clone())
        .unwrap();
    let mut finalized = observed.clone();
    finalized["revision"] = json!(format!("sha256:{hash}"));
    finalized["revisionBasis"] = json!("contentHash");
    finalized["requiresContentVerification"] = json!(false);
    finalized["sizeBytes"] = json!(size);
    let final_id = artifact_id(&finalized);
    let mut artifact = pending.clone();
    artifact["id"] = json!(final_id);
    artifact["revision"] = finalized["revision"].clone();
    let mut fresh = observed.clone();
    // A full refresh may retain the revision while advancing its observation
    // token. Comparing revision alone must not overwrite those new facts.
    fresh["sourceObservationToken"] = json!("authored-sync-new");
    f.service
        .catalogue
        .put("remote_file", "test-file-0", fresh.clone())
        .unwrap();
    let updates = vec![
        (
            "remote_file".into(),
            "test-file-0".into(),
            finalized.clone(),
        ),
        ("artifact".into(), final_id.clone(), artifact.clone()),
    ];
    let error = f
        .service
        .commit_download_records(
            updates.clone(),
            vec![("remote_file".into(), "test-file-0".into(), Some(observed))],
        )
        .unwrap_err();
    assert_eq!(io::stable_error(&error), "source_changed");
    assert_eq!(
        f.service
            .catalogue
            .get("remote_file", "test-file-0")
            .unwrap()
            .unwrap(),
        fresh
    );
    assert!(f
        .service
        .catalogue
        .get("artifact", &final_id)
        .unwrap()
        .is_none());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", "pending-authored-transfer")
            .unwrap()
            .unwrap(),
        pending
    );
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", retained["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        retained
    );
    io::verify(&pending_path, &hash, size).unwrap();
    io::verify(
        &retained_path,
        retained["hash"].as_str().unwrap(),
        retained["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
    // A fresh attempt can commit the actual revision without erasing the new
    // source token, and leaves previously published bytes available.
    finalized["sourceObservationToken"] = fresh["sourceObservationToken"].clone();
    f.service
        .commit_download_records(
            vec![
                (
                    "remote_file".into(),
                    "test-file-0".into(),
                    finalized.clone(),
                ),
                ("artifact".into(), final_id.clone(), artifact),
            ],
            vec![("remote_file".into(), "test-file-0".into(), Some(fresh))],
        )
        .unwrap();
    assert_eq!(
        f.service
            .catalogue
            .get("remote_file", "test-file-0")
            .unwrap()
            .unwrap(),
        finalized
    );
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", &final_id)
            .unwrap()
            .unwrap()["originalAvailable"],
        true
    );
    io::verify(
        &retained_path,
        retained["hash"].as_str().unwrap(),
        retained["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
}

#[test]
fn readable_managed_paths_are_contained_collision_safe_and_partial_cleanup_is_owned() {
    let f = Fixture::new();
    let root = f.path.join("library");
    for label in ["衣装 / shader:*", "CON.txt", "..", ""] {
        let component = io::readable_component(label, "synthetic-product");
        archive::validate_relative_path(&component).unwrap();
        assert!(!component.contains('/'));
    }
    let directory = format!(
        "assets/{}",
        io::readable_component("Synthetic outfit", "42")
    );
    let first = io::unused_relative(&root, &directory).unwrap();
    std::fs::create_dir_all(io::safe_join(&root, &first).unwrap()).unwrap();
    assert_ne!(io::unused_relative(&root, &directory).unwrap(), first);
    assert!(io::unused_relative(&root, "../outside").is_err());
    let originals = root.join("retained.zip");
    std::fs::write(&originals, b"completed original").unwrap();
    let partials = root.join(".nna-partials");
    std::fs::create_dir(&partials).unwrap();
    let owned = partials.join("pending-owned.part");
    let sibling = partials.join("pending-sibling.part");
    std::fs::write(&owned, b"incomplete owned bytes").unwrap();
    std::fs::write(&sibling, b"other operation").unwrap();
    {
        let _guard = io::PartialGuard::new(owned.clone());
    }
    assert!(!owned.exists());
    assert_eq!(std::fs::read(&sibling).unwrap(), b"other operation");
    assert_eq!(std::fs::read(&originals).unwrap(), b"completed original");
    {
        let _guard = io::PartialGuard::new(sibling);
    }
    assert!(!partials.exists());
    let empty_source = root.join(&directory).join("revision/original");
    let boundary = root.join(&directory);
    std::fs::create_dir_all(&empty_source).unwrap();
    io::remove_empty_parents(&empty_source, &boundary);
    assert!(!empty_source.exists());
    assert!(!boundary.join("revision").exists());
    assert!(boundary.is_dir());
    let guarded = boundary.join("retained/original");
    std::fs::create_dir_all(&guarded).unwrap();
    std::fs::write(guarded.join("manual.txt"), b"user retained sibling").unwrap();
    io::remove_empty_parents(&guarded, &boundary);
    assert_eq!(
        std::fs::read(guarded.join("manual.txt")).unwrap(),
        b"user retained sibling"
    );
    let outside = f.path.join("outside");
    std::fs::create_dir(&outside).unwrap();
    io::remove_empty_parents(&outside, &boundary);
    assert!(outside.is_dir());
}

#[test]
fn manifest_fingerprint_tracks_revalidation_and_file_delta_independent_of_order() {
    let a = json!({"id":"a","revision":"r1"});
    let b = json!({"id":"b","revision":"r2"});
    assert_eq!(
        fingerprint(&[a.clone(), b.clone()]),
        fingerprint(&[b.clone(), a.clone()])
    );
    let mut refresh = a.clone();
    refresh["requiresContentVerification"] = json!(true);
    assert_ne!(fingerprint(&[a.clone()]), fingerprint(&[refresh]));
    assert_ne!(fingerprint(&[a.clone()]), fingerprint(&[a, b]));
}

#[tokio::test]
#[cfg_attr(not(windows), ignore = "requires explicit NNA_7ZIP on non-Windows")]
async fn extraction_group_does_not_hash_unrelated_corrupt_originals() {
    use std::io::Write;
    let tool = native_seven_zip_test_tool();
    let f = Fixture::new();
    f.seed(2, true);
    let (mut artifact, original, _) = f.extracted_archive(0, true);
    let (_, unrelated, _) = f.extracted_archive(1, true);
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&original).unwrap());
    archive
        .start_file(
            "synthetic.unitypackage",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    archive.write_all(b"synthetic intact package").unwrap();
    archive.finish().unwrap();
    let (hash, size) = io::hash_file(&original).unwrap();
    artifact["hash"] = json!(hash);
    artifact["sizeBytes"] = json!(size);
    artifact["status"] = json!("downloaded");
    artifact.as_object_mut().unwrap().remove("extractedPath");
    artifact
        .as_object_mut()
        .unwrap()
        .remove("extractedInventory");
    f.service
        .catalogue
        .put(
            "artifact",
            artifact["id"].as_str().unwrap(),
            artifact.clone(),
        )
        .unwrap();
    // A different archive in this same account and variant must not become
    // a blocker, or even have its bytes read by this extraction attempt.
    std::fs::write(&unrelated, b"edited unrelated original").unwrap();
    let files = f.service.remote_files("42").unwrap().0;
    let root = f.service.root(Some("test-root")).unwrap();
    f.service
        .extract_for_download(
            &tool,
            &root,
            &mut artifact,
            &files,
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert_eq!(artifact["status"], "ready");
    assert_eq!(artifact["originalAvailable"], false);
    assert!(!original.exists());
    assert!(!original.parent().unwrap().exists());
    assert!(!original.parent().unwrap().parent().unwrap().exists());
    assert!(!f.path.join("library/.nna-staging").exists());
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"edited unrelated original"
    );
    let extracted = io::safe_join(
        &f.path.join("library"),
        artifact["extractedPath"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(extracted.join("synthetic.unitypackage")).unwrap(),
        b"synthetic intact package"
    );
}

#[tokio::test]
#[cfg_attr(not(windows), ignore = "requires explicit NNA_7ZIP on non-Windows")]
async fn verified_downloads_real_extraction_certification_and_reopen_preserve_ready_and_owned_cleanup(
) {
    let tool = native_seven_zip_test_tool();
    let f = Fixture::new();
    f.seed(3, false);
    let mut product = f.service.catalogue.get("product", "42").unwrap().unwrap();
    product["store"] = json!("BOOTH");
    product["provider"] = json!("booth");
    product["sourceCategory"] = json!("Clothes");
    product["sourceTags"] = json!(["authored-clothing", "authored-compatible"]);
    f.service
        .catalogue
        .put("product", "42", product.clone())
        .unwrap();
    let retained = f.artifact(0);
    let library = f.path.join("library");
    let retained_path =
        io::safe_join(&library, retained["relativePath"].as_str().unwrap()).unwrap();
    let partial = f
        .service
        .catalogue
        .command("product_details", json!({"id":"42"}))
        .unwrap();
    assert_eq!(partial["status"], "partial");
    assert_eq!(partial["downloaded"], 1);
    assert_eq!(partial["total"], 3);
    assert_eq!(partial["manifestComplete"], false);

    let bytes_a = authored_zip(&[
        ("衣装/empty/", b""),
        ("衣装/model.unitypackage", b"authored package bytes"),
        ("衣装/nested.zip", b"nested bytes remain unexpanded"),
    ]);
    let bytes_b = authored_zip(&[("manual/readme.txt", b"authored manual bytes")]);
    let (mut archive_a, original_a) = f.publish_authored_download(1, "outfit.zip", &bytes_a);
    let (mut archive_b, original_b) = f.publish_authored_download(2, "manual.zip", &bytes_b);
    let user_sibling = original_b.parent().unwrap().join("user-note.txt");
    std::fs::write(&user_sibling, b"authored user retained sibling").unwrap();
    let unrelated = library.join("unrelated-retained.bin");
    std::fs::write(&unrelated, b"authored unrelated retained bytes").unwrap();
    assert!(!library.join(".nna-partials").exists());
    let root = f.service.root(Some("test-root")).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    for artifact in [&mut archive_a, &mut archive_b] {
        let files = f.service.remote_files("42").unwrap().0;
        f.service
            .extract_for_download(&tool, &root, artifact, &files, &cancel)
            .await
            .unwrap();
        assert_eq!(artifact["status"], "ready");
        assert_eq!(artifact["originalAvailable"], false);
        assert!(f
            .service
            .artifact_verified(artifact, &library)
            .await
            .unwrap());
    }
    let uncertified = f
        .service
        .catalogue
        .command("product_details", json!({"id":"42"}))
        .unwrap();
    assert_eq!(uncertified["status"], "ready");
    assert_eq!(uncertified["downloaded"], 3);
    assert_eq!(uncertified["manifestComplete"], false);
    assert_eq!(uncertified["fullyDownloaded"], true);
    assert!(uncertified["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|file| file["status"] == "ready"));
    // Source coverage stays incomplete, while the observed files are usable.
    let mut entitlement = f
        .service
        .catalogue
        .get("entitlement", "test-entitlement")
        .unwrap()
        .unwrap();
    entitlement["manifestComplete"] = json!(true);
    f.service
        .catalogue
        .put("entitlement", "test-entitlement", entitlement.clone())
        .unwrap();
    let ready = f
        .service
        .catalogue
        .command("product_details", json!({"id":"42"}))
        .unwrap();
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["downloaded"], 3);
    assert_eq!(ready["total"], 3);
    assert_eq!(ready["manifestComplete"], true);
    assert_eq!(ready["fullyDownloaded"], true);
    assert!(!original_a.exists());
    assert!(!original_b.exists());
    assert!(!original_a.parent().unwrap().parent().unwrap().exists());
    assert_eq!(
        std::fs::read(&user_sibling).unwrap(),
        b"authored user retained sibling"
    );
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"authored unrelated retained bytes"
    );
    assert!(!library.join(".nna-staging").exists());
    assert!(!library.join(".nna-partials").exists());
    assert!(f
        .service
        .catalogue
        .list("artifact")
        .unwrap()
        .iter()
        .all(|artifact| !artifact["id"].as_str().unwrap().starts_with("pending-")));
    let extracted_a = f
        .service
        .verify_extracted_at(&archive_a, &library, &AtomicBool::new(false))
        .unwrap();
    assert!(extracted_a.join("衣装/empty").is_dir());
    assert_eq!(
        std::fs::read(extracted_a.join("衣装/model.unitypackage")).unwrap(),
        b"authored package bytes"
    );
    assert_eq!(
        std::fs::read(extracted_a.join("衣装/nested.zip")).unwrap(),
        b"nested bytes remain unexpanded"
    );
    assert!(!extracted_a.join("衣装/nested").exists());
    for index in [1, 2] {
        let file = f
            .service
            .catalogue
            .get("remote_file", &format!("test-file-{index}"))
            .unwrap()
            .unwrap();
        assert_eq!(file["requiresContentVerification"], false);
        assert_eq!(
            file["sourceObservationToken"],
            format!("authored-source-{index}")
        );
    }
    assert_eq!(
        f.service.catalogue.get("product", "42").unwrap().unwrap(),
        product
    );
    io::verify(
        &retained_path,
        retained["hash"].as_str().unwrap(),
        retained["sizeBytes"].as_u64().unwrap(),
    )
    .unwrap();
    let fixture_path = f.path.clone();
    drop(f);

    let catalogue = Catalogue::open(&fixture_path.join("data")).unwrap();
    assert_eq!(
        catalogue
            .command("product_details", json!({"id":"42"}))
            .unwrap()["status"],
        "ready"
    );
    assert_eq!(catalogue.get("product", "42").unwrap().unwrap(), product);
    assert_eq!(
        catalogue
            .get("entitlement", "test-entitlement")
            .unwrap()
            .unwrap(),
        entitlement
    );
    let booth = Arc::new(crate::booth::BoothService::new(catalogue.clone()).unwrap());
    let stores = Arc::new(StoreService::new(catalogue.clone(), booth).unwrap());
    let reopened = StorageService::new(catalogue.clone(), stores).unwrap();
    let verified = reopened
        .disk_operation(
            "verify_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(verified["verified"], 3);
    assert_eq!(verified["missing"], 0);
    let durable = catalogue
        .command("product_details", json!({"id":"42"}))
        .unwrap();
    assert_eq!(durable["status"], "ready");
    assert_eq!(durable["fullyDownloaded"], true);
    assert_eq!(durable["downloaded"], 3);
    assert_eq!(
        std::fs::read(&user_sibling).unwrap(),
        b"authored user retained sibling"
    );
    assert_eq!(
        std::fs::read(&unrelated).unwrap(),
        b"authored unrelated retained bytes"
    );
    drop(reopened);
    drop(catalogue);
    assert!(fixture_path.is_absolute());
    assert_eq!(fixture_path.parent().unwrap(), std::env::temp_dir());
    assert!(fixture_path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("nna-storage-synthetic-"));
    archive::validate_no_links(&fixture_path).unwrap();
    std::fs::remove_dir_all(fixture_path).unwrap();
}

#[tokio::test]
#[cfg_attr(not(windows), ignore = "requires explicit NNA_7ZIP on non-Windows")]
async fn real_extraction_failure_retains_original_and_retry_cancel_are_distinct_from_partial() {
    let tool = native_seven_zip_test_tool();
    let f = Fixture::new();
    f.seed(2, true);
    let sibling = f.artifact(1);
    let library = f.path.join("library");
    let (mut failed, corrupt_original) =
        f.publish_authored_download(0, "broken.zip", b"authored corrupt zip bytes");
    let corrupt_state = io::hash_file(&corrupt_original).unwrap();
    let root = f.service.root(Some("test-root")).unwrap();
    let files = f.service.remote_files("42").unwrap().0;
    let error = f
        .service
        .extract_for_download(
            &tool,
            &root,
            &mut failed,
            &files,
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap_err();
    assert_eq!(io::stable_error(&error), "archive_corrupt");
    assert_eq!(failed["status"], "extractionFailed");
    assert_eq!(failed["originalAvailable"], true);
    assert_eq!(failed["errorCode"], "archive_corrupt");
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", failed["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        failed
    );
    assert_eq!(io::hash_file(&corrupt_original).unwrap(), corrupt_state);
    assert!(!library.join(".nna-staging").exists());
    assert_eq!(
        f.service
            .catalogue
            .command("product_details", json!({"id":"42"}))
            .unwrap()["status"],
        "attention"
    );
    let retry = f
        .service
        .enqueue_file(&json!({"fileId":"test-file-0"}))
        .await
        .unwrap();
    let mut retry_job = f
        .service
        .catalogue
        .get("job", retry["jobIds"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(retry_job["selectedFileIds"], json!(["test-file-0"]));
    assert_eq!(
        f.service
            .catalogue
            .command("product_details", json!({"id":"42"}))
            .unwrap()["status"],
        "processing"
    );
    let good_bytes = authored_zip(&[("repaired/model.unitypackage", b"authored repaired package")]);
    let (mut repaired, retry_original) = f.publish_authored_download(0, "broken.zip", &good_bytes);
    let files = f.service.remote_files("42").unwrap().0;
    let cancelled = f
        .service
        .extract_for_download(
            &tool,
            &root,
            &mut repaired,
            &files,
            &Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap_err();
    assert_eq!(io::stable_error(&cancelled), "cancelled");
    assert_eq!(repaired["originalAvailable"], true);
    assert!(retry_original.is_file());
    f.service
        .put_job(&mut retry_job, "cancelled", None)
        .unwrap();
    let partial = f
        .service
        .catalogue
        .command("product_details", json!({"id":"42"}))
        .unwrap();
    assert_eq!(partial["status"], "partial");
    assert!(partial["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["id"] == "test-file-0" && file["status"] == "downloaded"));
    let final_retry = f
        .service
        .enqueue_file(&json!({"fileId":"test-file-0"}))
        .await
        .unwrap();
    assert_ne!(final_retry, retry);
    let mut final_job = f
        .service
        .catalogue
        .get("job", final_retry["jobIds"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    let files = f.service.remote_files("42").unwrap().0;
    f.service
        .extract_for_download(
            &tool,
            &root,
            &mut repaired,
            &files,
            &Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    f.service
        .put_job(&mut final_job, "completed", None)
        .unwrap();
    assert_eq!(repaired["status"], "ready");
    assert_eq!(repaired["errorCode"], Value::Null);
    assert_eq!(repaired["originalAvailable"], false);
    assert!(!retry_original.exists());
    assert!(!retry_original.parent().unwrap().parent().unwrap().exists());
    assert_eq!(io::hash_file(&corrupt_original).unwrap(), corrupt_state);
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", sibling["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        sibling
    );
    assert!(f
        .service
        .artifact_verified(&sibling, &library)
        .await
        .unwrap());
    assert!(!library.join(".nna-staging").exists());
    assert!(!library.join(".nna-partials").exists());
    let ready = f
        .service
        .catalogue
        .command("product_details", json!({"id":"42"}))
        .unwrap();
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["manifestComplete"], true);
    assert_eq!(ready["fullyDownloaded"], true);
    let extracted = f
        .service
        .verify_extracted_at(&repaired, &library, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        std::fs::read(extracted.join("repaired/model.unitypackage")).unwrap(),
        b"authored repaired package"
    );
}

#[tokio::test]
async fn automatic_invalidation_survives_busy_and_offline_roots_without_repeated_full_scan() {
    let f = Fixture::new();
    f.seed(1, true);
    f.service
        .catalogue
        .command("preferences", json!({"patch":{"autoDownloadAll":true}}))
        .unwrap();
    let operation = f.service.operations.write().await;
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    assert!(f.service.catalogue.list("job").unwrap().is_empty());
    assert!(f
        .service
        .automatic_pending
        .lock()
        .unwrap()
        .products
        .contains("42"));
    drop(operation);
    std::fs::rename(f.path.join("library"), f.path.join("offline-library")).unwrap();
    f.service.reevaluate_automatic().await.unwrap();
    f.service.drain_automatic_changes().await.unwrap();
    assert!(f.service.automatic_pending.lock().unwrap().full);
    assert!(f.service.catalogue.list("job").unwrap().is_empty());
    std::fs::rename(f.path.join("offline-library"), f.path.join("library")).unwrap();
    f.service.drain_automatic_changes().await.unwrap();
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 1);
    let pending = f.service.automatic_pending.lock().unwrap();
    assert!(!pending.full && pending.products.is_empty());
}

#[tokio::test]
async fn automatic_source_delta_only_evaluates_touched_products_and_never_retries_same_failed_manifest(
) {
    let f = Fixture::new();
    f.seed(1, true);
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    assert!(f.service.catalogue.list("job").unwrap().is_empty());
    f.service
        .catalogue
        .command("preferences", json!({"patch":{"autoDownloadAll":true}}))
        .unwrap();
    f.service
        .catalogue
        .put(
            "product",
            "43",
            json!({"id":"43","title":"Other synthetic product"}),
        )
        .unwrap();
    f.service.catalogue.put("remote_file", "other-file", json!({"id":"other-file","productId":"43","accountId":"test-account","name":"other.psd","revision":"r1"})).unwrap();
    f.service.catalogue.put("entitlement", "other-entitlement", json!({"id":"other-entitlement","productId":"43","accountId":"test-account","fileIds":["other-file"],"manifestComplete":true})).unwrap();
    f.service
        .source_products_changed(&["42".into(), "42".into()])
        .await
        .unwrap();
    let mut jobs = f.service.catalogue.list("job").unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["productId"], "42");
    f.service
        .put_job(&mut jobs[0], "failed", Some("network_failed"))
        .unwrap();
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 1);
    let mut file = f
        .service
        .catalogue
        .get("remote_file", "test-file-0")
        .unwrap()
        .unwrap();
    file["revision"] = json!("r2");
    f.service
        .catalogue
        .put("remote_file", "test-file-0", file)
        .unwrap();
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    jobs = f.service.catalogue.list("job").unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().all(|job| job["productId"] == "42"));
    f.service.reevaluate_automatic().await.unwrap();
    assert_eq!(f.service.catalogue.related("job", "43").unwrap().len(), 1);
}

#[tokio::test]
async fn automatic_delta_preserves_selected_intent_and_rechecks_updates_after_active_job() {
    let f = Fixture::new();
    f.seed(1, true);
    f.service
        .catalogue
        .command("preferences", json!({"patch":{"autoDownloadAll":false}}))
        .unwrap();
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    assert!(f.service.catalogue.list("job").unwrap().is_empty());
    let first = f
        .service
        .enqueue(&json!({"productIds":["42"]}))
        .await
        .unwrap();
    let mut file = f
        .service
        .catalogue
        .get("remote_file", "test-file-0")
        .unwrap()
        .unwrap();
    file["revision"] = json!("r2");
    f.service
        .catalogue
        .put("remote_file", "test-file-0", file)
        .unwrap();
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    assert_eq!(f.service.catalogue.list("job").unwrap().len(), 1);
    let mut job = f
        .service
        .catalogue
        .get("job", first["jobIds"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    f.service.put_job(&mut job, "completed", None).unwrap();
    f.service
        .source_products_changed(&["42".into()])
        .await
        .unwrap();
    let jobs = f.service.catalogue.list("job").unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(jobs
        .iter()
        .any(|job| job["state"] == "queued" && job["automatic"] == true));
}

#[tokio::test]
async fn automatic_jobs_exclude_explicitly_disconnected_account_files() {
    let f = Fixture::new();
    f.seed(1, true);
    f.service
        .catalogue
        .put(
            "account",
            "removed-account",
            json!({"id":"removed-account","store":"BOOTH","disconnected":true}),
        )
        .unwrap();
    f.service.catalogue.put("remote_file","removed-file",json!({"id":"removed-file","productId":"42","accountId":"removed-account","name":"old.psd","revision":"r1"})).unwrap();
    f.service.catalogue.put("entitlement","removed-entitlement",json!({"id":"removed-entitlement","productId":"42","accountId":"removed-account","fileIds":["removed-file"],"manifestComplete":true})).unwrap();
    let queued = f
        .service
        .enqueue(&json!({"productIds":["42"],"automatic":true}))
        .await
        .unwrap();
    let job = f
        .service
        .catalogue
        .get("job", queued["jobIds"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job["fileIds"], json!(["test-file-0"]));
    assert_eq!(job["automatic"], true);
}

#[tokio::test]
async fn product_queue_includes_thirty_files_deduplicates_active_job_and_persists_intent() {
    let f = Fixture::new();
    f.seed(30, true);
    let first = f
        .service
        .enqueue(&json!({"productIds":["42"],"fileIds":["test-file-0"]}))
        .await
        .unwrap();
    let again = f
        .service
        .enqueue(&json!({"productIds":["42","42"]}))
        .await
        .unwrap();
    assert_eq!(first, again);
    let jobs = f.service.catalogue.list("job").unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["fileIds"].as_array().unwrap().len(), 30);
    assert_eq!(
        f.service
            .catalogue
            .get("download_intent", "42")
            .unwrap()
            .unwrap()["wantDownload"],
        true
    );
}

#[test]
fn entitlement_manifest_unknown_and_missing_metadata_are_not_complete() {
    let f = Fixture::new();
    f.seed(2, false);
    assert!(!f.service.remote_files("42").unwrap().1);
    let mut e = f
        .service
        .catalogue
        .get("entitlement", "test-entitlement")
        .unwrap()
        .unwrap();
    e["manifestComplete"] = json!(true);
    e["fileIds"]
        .as_array_mut()
        .unwrap()
        .push(json!("missing-metadata"));
    f.service
        .catalogue
        .put("entitlement", "test-entitlement", e)
        .unwrap();
    assert!(!f.service.remote_files("42").unwrap().1);
}

#[tokio::test]
async fn verified_files_survive_retry_and_partial_completion_requires_a_fault_for_attention() {
    let f = Fixture::new();
    f.seed(30, true);
    let mut saved = Vec::new();
    for i in 0..29 {
        saved.push(f.artifact(i));
    }
    let projection = || {
        let snapshot = f.service.catalogue.command("bootstrap", json!({})).unwrap();
        snapshot["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "42")
            .unwrap()
            .clone()
    };
    let product = projection();
    assert_eq!(product["status"], "partial");
    assert_eq!(product["downloaded"], 29);
    assert_eq!(product["total"], 30);
    assert_eq!(product["fullyDownloaded"], false);
    let queued = f
        .service
        .enqueue(&json!({"productIds":["42"]}))
        .await
        .unwrap();
    assert_eq!(projection()["status"], "processing");
    assert_eq!(projection()["fullyDownloaded"], false);
    let mut job = f
        .service
        .catalogue
        .get("job", queued["jobIds"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    // Match a failed stream: pending bytes have no bound revision; the failed job
    // supplies Attention while all 29 completed artifact facts remain intact.
    let pending_id = format!("pending-{}", io::key("test-file-29"));
    f.service
        .catalogue
        .put(
            "artifact",
            &pending_id,
            json!({
                "id":pending_id,"productId":"42","fileId":"test-file-29",
                "rootId":"test-root","revision":null,"status":"notDownloaded",
                "originalAvailable":false,"errorCode":"network_failed"
            }),
        )
        .unwrap();
    f.service
        .put_job(&mut job, "failed", Some("network_failed"))
        .unwrap();
    assert_eq!(projection()["status"], "attention");
    assert_eq!(projection()["fullyDownloaded"], false);
    let retry = f
        .service
        .enqueue(&json!({"productIds":["42"]}))
        .await
        .unwrap();
    assert_ne!(retry, queued);
    assert_eq!(projection()["status"], "processing");
    for artifact in saved {
        assert_eq!(
            f.service
                .catalogue
                .get("artifact", artifact["id"].as_str().unwrap())
                .unwrap()
                .unwrap(),
            artifact
        );
        assert!(f
            .service
            .artifact_verified(&artifact, &f.path.join("library"))
            .await
            .unwrap());
    }
    let mut retry_job = f
        .service
        .catalogue
        .get("job", retry["jobIds"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    f.service
        .put_job(&mut retry_job, "cancelled", None)
        .unwrap();
    assert_eq!(projection()["status"], "partial");
    assert_eq!(projection()["fullyDownloaded"], false);
    // Every currently observed file must be Ready; source coverage is separate.
    f.artifact(29);
    let product = projection();
    assert_eq!(product["status"], "ready");
    assert_eq!(product["downloaded"], 30);
    assert_eq!(product["fullyDownloaded"], true);
    let mut entitlement = f
        .service
        .catalogue
        .get("entitlement", "test-entitlement")
        .unwrap()
        .unwrap();
    entitlement["manifestComplete"] = json!(false);
    f.service
        .catalogue
        .put("entitlement", "test-entitlement", entitlement)
        .unwrap();
    assert_eq!(projection()["status"], "ready");
    assert_eq!(projection()["manifestComplete"], false);
    assert_eq!(projection()["fullyDownloaded"], true);
}

#[test]
fn offline_root_preserves_artifact_fact_and_missing_file_is_attention() {
    let f = Fixture::new();
    f.seed(1, true);
    let artifact = f.artifact(0);
    std::fs::rename(f.path.join("library"), f.path.join("offline-library")).unwrap();
    let result = f
        .service
        .disk_operation(
            "verify_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result["status"], "offline");
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", artifact["id"].as_str().unwrap())
            .unwrap()
            .unwrap()["status"],
        "ready"
    );
    std::fs::rename(f.path.join("offline-library"), f.path.join("library")).unwrap();
    std::fs::remove_file(
        f.path
            .join("library/assets")
            .join(io::readable_component("Synthetic clothing", "42"))
            .join("fixture-0/fixture-0.unitypackage"),
    )
    .unwrap();
    let result = f
        .service
        .disk_operation(
            "verify_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result["missing"], 1);
}

#[test]
fn migration_copy_verify_switch_retains_original_until_explicit_cleanup() {
    let f = Fixture::new();
    f.seed(1, true);
    f.artifact(0);
    let old = f
        .path
        .join("library/assets")
        .join(io::readable_component("Synthetic clothing", "42"))
        .join("fixture-0/fixture-0.unitypackage");
    let new = f.path.join("new-library");
    f.service
        .disk_operation(
            "migrate_root",
            &json!({"rootId":"test-root","path":new}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(old.exists());
    assert!(new
        .join("assets")
        .join(io::readable_component("Synthetic clothing", "42"))
        .join("fixture-0/fixture-0.unitypackage")
        .exists());
    assert_eq!(
        PathBuf::from(
            f.service.root(Some("test-root")).unwrap()["path"]
                .as_str()
                .unwrap()
        )
        .canonicalize()
        .unwrap(),
        new.canonicalize().unwrap()
    );
    std::fs::write(&old, b"locally edited").unwrap();
    assert!(f
        .service
        .disk_operation(
            "cleanup_old_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(old.exists());
}

#[test]
fn remap_and_migration_cancellation_never_switch_to_unverified_destination() {
    let f = Fixture::new();
    f.seed(1, true);
    f.artifact(0);
    let destination = f.path.join("new-library");
    std::fs::create_dir_all(&destination).unwrap();
    assert!(f
        .service
        .disk_operation(
            "remap_root",
            &json!({"rootId":"test-root","path":destination}),
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(f
        .service
        .disk_operation(
            "migrate_root",
            &json!({"rootId":"test-root","path":destination}),
            &AtomicBool::new(true)
        )
        .is_err());
    assert_eq!(
        f.service.root(Some("test-root")).unwrap()["path"],
        json!(f.path.join("library"))
    );
}

#[test]
fn incomplete_product_cannot_export_and_path_traversal_is_refused() {
    let f = Fixture::new();
    f.seed(2, true);
    f.artifact(0);
    assert!(f
        .service
        .disk_operation(
            "export_zip",
            &json!({"productIds":["42"],"path":f.path.join("export.zip")}),
            &AtomicBool::new(false)
        )
        .is_err());
    assert!(!f.path.join("export.zip").exists());
    assert!(io::safe_join(&f.path.join("library"), "../outside").is_err());
    assert!(io::safe_join(&f.path.join("library"), "a:stream").is_err());
}

#[test]
fn observed_ready_files_export_before_source_scan_closes() {
    let f = Fixture::new();
    f.seed(1, false);
    f.artifact(0);
    let destination = f.path.join("observed-ready.zip");
    let result = f.service.disk_operation(
        "export_zip",
        &json!({"productIds":["42"],"path":destination}),
        &AtomicBool::new(false),
    ).unwrap();
    assert_eq!(result["fileCount"], 1);
    assert!(destination.exists());
    assert_eq!(f.service.catalogue.command("product_details", json!({"id":"42"})).unwrap()["manifestComplete"], false);
}

#[test]
fn kept_file_whose_extraction_failed_still_exports_its_original() {
    let f = Fixture::new();
    f.seed(2, true);
    let current = f.artifact(0);
    let mut kept = f.artifact(1);
    // The store dropped the second file after its download; its extraction had failed.
    f.service.catalogue.put("entitlement","test-entitlement",json!({"id":"test-entitlement","productId":"42","accountId":"test-account","fileIds":["test-file-0"],"manifestComplete":true})).unwrap();
    kept["status"] = json!("extractionFailed");
    kept["errorCode"] = json!("archive_corrupt");
    let id = kept["id"].as_str().unwrap().to_owned();
    f.service.catalogue.put("artifact", &id, kept).unwrap();
    let result = f.service.disk_operation(
        "export_zip",
        &json!({"productIds":["42"],"path":f.path.join("kept.zip")}),
        &AtomicBool::new(false),
    ).unwrap();
    assert_eq!(result["fileCount"], 2);
    // The store's own files must still be ready.
    let mut failed = current;
    failed["status"] = json!("extractionFailed");
    let id = failed["id"].as_str().unwrap().to_owned();
    f.service.catalogue.put("artifact", &id, failed).unwrap();
    assert!(f.service.disk_operation(
        "export_zip",
        &json!({"productIds":["42"],"path":f.path.join("current.zip")}),
        &AtomicBool::new(false),
    ).is_err());
}

#[tokio::test]
async fn network_wait_is_cancellable_without_waiting_for_timeout() {
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        signal.store(true, Ordering::Release);
    });
    let start = std::time::Instant::now();
    assert!(
        io::cancellable(&cancel, tokio::time::sleep(Duration::from_secs(3600)))
            .await
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn metadata_restore_originals_recover_only_after_verified_remap_or_root_check() {
    let source = Fixture::new();
    source.seed(1, true);
    let original = source.artifact(0);
    let manifest = source.service.catalogue.manifest().unwrap();
    for action in ["remap_root", "verify_root"] {
        let restored = Fixture::new();
        restored
            .service
            .catalogue
            .restore_manifest(manifest.clone())
            .unwrap();
        let id = original["id"].as_str().unwrap();
        let unknown = restored
            .service
            .catalogue
            .get("artifact", id)
            .unwrap()
            .unwrap();
        assert_eq!(unknown["status"], "unknown");
        assert_eq!(unknown["restoreStatus"], "ready");
        assert_eq!(unknown["originalAvailable"], false);
        let old_root = restored.service.root(Some("test-root")).unwrap();
        let destination = restored.path.join(if action == "remap_root" {
            "mapped-library"
        } else {
            "library"
        });
        let target = destination.join(original["relativePath"].as_str().unwrap());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        // Same size, different bytes: path and size alone cannot restore Ready.
        std::fs::write(&target, b"synthetic unity package bytez").unwrap();
        let args = json!({"rootId":"test-root","path":destination});
        let invalid = restored
            .service
            .disk_operation(action, &args, &AtomicBool::new(false));
        if action == "remap_root" {
            assert!(invalid.is_err());
            assert_eq!(
                restored.service.root(Some("test-root")).unwrap()["path"],
                old_root["path"]
            );
        } else {
            assert_eq!(invalid.unwrap()["verified"], 0);
        }
        assert_eq!(
            restored
                .service
                .catalogue
                .get("artifact", id)
                .unwrap()
                .unwrap(),
            unknown
        );
        assert!(restored
            .service
            .resolve_open_target("open_file", &json!({"fileId":"test-file-0"}))
            .is_err());
        std::fs::write(&target, b"synthetic unity package bytes").unwrap();
        let result = restored
            .service
            .disk_operation(action, &args, &AtomicBool::new(false))
            .unwrap();
        if action == "verify_root" {
            assert_eq!(result["verified"], 1);
        }
        let recovered = restored
            .service
            .catalogue
            .get("artifact", id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered["status"], "ready");
        assert_eq!(recovered["originalAvailable"], true);
        assert!(recovered.get("restoreStatus").is_none());
        assert_eq!(
            restored
                .service
                .resolve_open_target("open_file", &json!({"fileId":"test-file-0"}))
                .unwrap()
                .canonicalize()
                .unwrap(),
            target.canonicalize().unwrap()
        );
    }
}

#[test]
fn metadata_restore_extracted_state_requires_verified_members_and_truthful_original_availability() {
    for action in ["remap_root", "verify_root"] {
        for retained in [false, true] {
            let source = Fixture::new();
            source.seed(1, true);
            let (artifact, original, source_tree) = source.extracted_archive(0, retained);
            let manifest = source.service.catalogue.manifest().unwrap();
            let restored = Fixture::new();
            restored
                .service
                .catalogue
                .restore_manifest(manifest)
                .unwrap();
            let id = artifact["id"].as_str().unwrap();
            let unknown = restored
                .service
                .catalogue
                .get("artifact", id)
                .unwrap()
                .unwrap();
            assert_eq!(unknown["status"], "unknown");
            assert_eq!(unknown["restoreStatus"], "ready");
            assert_eq!(unknown["originalAvailable"], false);
            let old_root = restored.service.root(Some("test-root")).unwrap();
            let destination = restored.path.join(if action == "remap_root" {
                "mapped-library"
            } else {
                "library"
            });
            let tree = destination.join(artifact["extractedPath"].as_str().unwrap());
            std::fs::create_dir_all(tree.join("empty")).unwrap();
            let member = tree.join("model.fbx");
            let correct = std::fs::read(source_tree.join("model.fbx")).unwrap();
            let mut damaged = correct.clone();
            damaged[0] ^= 1;
            std::fs::write(&member, &damaged).unwrap();
            let restored_original = destination.join(artifact["relativePath"].as_str().unwrap());
            if retained {
                std::fs::create_dir_all(restored_original.parent().unwrap()).unwrap();
                std::fs::copy(&original, &restored_original).unwrap();
            }
            let args = json!({"rootId":"test-root","path":destination});
            let failed = restored
                .service
                .disk_operation(action, &args, &AtomicBool::new(false));
            if action == "remap_root" {
                assert!(failed.is_err());
                assert_eq!(
                    restored.service.root(Some("test-root")).unwrap()["path"],
                    old_root["path"]
                );
            } else {
                let failed = failed.unwrap();
                assert_eq!(failed["verified"], 0);
                assert_eq!(failed["missing"], 1);
            }
            assert_eq!(
                restored
                    .service
                    .catalogue
                    .get("artifact", id)
                    .unwrap()
                    .unwrap(),
                unknown
            );
            assert!(restored
                .service
                .resolve_open_target("open_file", &json!({"fileId":"test-file-0"}))
                .is_err());
            assert_eq!(std::fs::read(&member).unwrap(), damaged);
            std::fs::write(&member, &correct).unwrap();
            let verified = restored
                .service
                .disk_operation(action, &args, &AtomicBool::new(false))
                .unwrap();
            if action == "verify_root" {
                assert_eq!(verified["verified"], 1);
            }
            let recovered = restored
                .service
                .catalogue
                .get("artifact", id)
                .unwrap()
                .unwrap();
            assert_eq!(recovered["status"], "ready");
            assert_eq!(recovered["originalAvailable"], retained);
            assert!(recovered.get("restoreStatus").is_none());
            assert_eq!(recovered["relativePath"], artifact["relativePath"]);
            assert_eq!(recovered["extractedPath"], artifact["extractedPath"]);
            assert_eq!(
                recovered["extractedInventory"],
                artifact["extractedInventory"]
            );
            assert_eq!(restored_original.exists(), retained);
            if retained {
                assert_eq!(
                    std::fs::read(&restored_original).unwrap(),
                    std::fs::read(&original).unwrap()
                );
            }
            assert_eq!(
                restored
                    .service
                    .resolve_open_target("open_file", &json!({"fileId":"test-file-0"}))
                    .unwrap()
                    .canonicalize()
                    .unwrap(),
                tree.canonicalize().unwrap()
            );
        }
    }
}

#[test]
fn full_backup_restore_preserves_files_empty_dirs_media_and_human_metadata() {
    let f = Fixture::new();
    f.seed(1, true);
    let mut a = f.artifact(0);
    let extracted = f.path.join("library/extracted/example");
    std::fs::create_dir_all(extracted.join("empty")).unwrap();
    std::fs::write(extracted.join("source.psd"), b"synthetic pixels").unwrap();
    let (hash, size) = io::hash_file(&extracted.join("source.psd")).unwrap();
    a["extractedPath"] = json!("extracted/example");
    a["extractedInventory"] = json!([{"relativePath":"empty","isDirectory":true,"hash":"","sizeBytes":0},{"relativePath":"source.psd","hash":hash,"sizeBytes":size,"isDirectory":false}]);
    f.service
        .catalogue
        .put("artifact", a["id"].as_str().unwrap(), a.clone())
        .unwrap();
    let media = f.path.join("data/media/fixture.png");
    std::fs::create_dir_all(media.parent().unwrap()).unwrap();
    std::fs::write(&media, b"independently authored test image payload").unwrap();
    f.service.catalogue.put("overlay","42",json!({"id":"42","title":"User name","note":"User note","rejectedTags":["rejected"],"thumbnailPath":"media/fixture.png"})).unwrap();
    f.service
        .catalogue
        .put(
            "acquisition",
            "acq",
            json!({"id":"acq","productId":"42","accountId":"test-account","source":"gift"}),
        )
        .unwrap();
    let backup = f.path.join("backup.nna-backup.zip");
    f.service
        .disk_operation(
            "export_backup",
            &json!({"path":backup}),
            &AtomicBool::new(false),
        )
        .unwrap();
    let destination = f.path.join("restored");
    std::fs::create_dir_all(&destination).unwrap();
    f.service
        .disk_operation(
            "restore_backup",
            &json!({"path":backup,"destinationPath":destination}),
            &AtomicBool::new(false),
        )
        .unwrap();
    let overlay = f.service.catalogue.get("overlay", "42").unwrap().unwrap();
    assert_eq!(overlay["title"], "User name");
    assert_eq!(overlay["rejectedTags"], json!(["rejected"]));
    assert!(f
        .path
        .join("data")
        .join(overlay["thumbnailPath"].as_str().unwrap())
        .exists());
    assert!(f
        .service
        .catalogue
        .get("acquisition", "acq")
        .unwrap()
        .is_some());
    assert!(f
        .service
        .catalogue
        .get("entitlement", "test-entitlement")
        .unwrap()
        .is_some());
    let root = f.service.root(Some("test-root")).unwrap();
    let root = PathBuf::from(root["path"].as_str().unwrap());
    assert!(root.starts_with(&destination));
    assert!(root.join("extracted/example/empty").is_dir());
    let restored = f
        .service
        .catalogue
        .get("artifact", a["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(restored["status"], "ready");
    assert_eq!(restored["originalAvailable"], true);
}

#[test]
fn backup_export_refuses_missing_current_or_conflict_thumbnail_payloads() {
    let f = Fixture::new();
    f.seed(1, true);
    f.artifact(0);
    f.service.catalogue.put("overlay", "42", json!({"id":"42","thumbnailPath":"media/missing.png"})).unwrap();
    let backup = f.path.join("missing-media.zip");
    let error = f.service.disk_operation("export_backup", &json!({"path":backup}), &AtomicBool::new(false)).unwrap_err();
    assert!(error.to_string().contains("backup_payload_missing"));
    assert!(!backup.exists());
    let mut manifest = f.service.catalogue.manifest().unwrap();
    manifest["records"]["overlay"][0].as_object_mut().unwrap().remove("thumbnailPath");
    manifest["media"] = json!(["media/retained-conflict.png"]);
    assert!(f.service.backup_payload(&manifest).is_err());
}

#[test]
fn backup_export_uses_captured_artifacts_after_the_live_records_change() {
    let f = Fixture::new();
    f.seed(1, true);
    let artifact = f.artifact(0);
    let manifest = f.service.catalogue.manifest().unwrap();
    f.service.catalogue.delete("artifact", artifact["id"].as_str().unwrap()).unwrap();
    let payload = f.service.backup_payload(&manifest).unwrap();
    assert_eq!(payload.len(), 1);
    let backup = f.path.join("captured-backup.zip");
    portable::create_backup(&backup, manifest, &payload, &AtomicBool::new(false)).unwrap();
    let restored = portable::restore_backup(&backup, &f.path.join("captured-restore"), &AtomicBool::new(false)).unwrap();
    assert_eq!(restored.payload.len(), 1);
    assert_eq!(restored.catalogue["records"]["artifact"][0], artifact);
}

#[test]
fn backup_restore_keeps_only_explicit_running_local_restore_job_observable() {
    let f = Fixture::new();
    f.seed(1, true);
    f.artifact(0);
    let catalogue = &f.service.catalogue;
    catalogue
        .put(
            "job",
            "other-device-restore",
            json!({"id":"other-device-restore","type":"storage:restore_backup","state":"running"}),
        )
        .unwrap();
    catalogue
        .put(
            "job",
            "old-download",
            json!({"id":"old-download","type":"download","state":"completed"}),
        )
        .unwrap();
    catalogue
        .put(
            "checkpoint",
            "old-checkpoint",
            json!({"id":"old-checkpoint"}),
        )
        .unwrap();
    catalogue
        .put("download_intent", "42", json!({"id":"42","selected":true}))
        .unwrap();
    let backup = f.path.join("active-job-backup.zip");
    f.service
        .disk_operation(
            "export_backup",
            &json!({"path":backup}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(catalogue.manifest().unwrap()["records"]
        .get("job")
        .is_none());
    let active = json!({"id":"this-device-restore","type":"storage:restore_backup","state":"running","createdAt":"2026-10-10T00:00:00Z","updatedAt":"2026-10-10T00:00:01Z"});
    catalogue
        .put("job", "this-device-restore", active.clone())
        .unwrap();
    f.service
        .restore(
            &json!({"path":backup,"destinationPath":f.path.join("active-job-restored")}),
            &AtomicBool::new(false),
            Some("this-device-restore"),
        )
        .unwrap();
    // Observe immediately after atomic replacement, before launch_operation can
    // re-record completion. The active job must never require that later write.
    assert_eq!(
        catalogue.command("state", json!({})).unwrap()["jobs"],
        json!([active])
    );
    assert!(catalogue
        .get("job", "other-device-restore")
        .unwrap()
        .is_none());
    assert!(catalogue.get("job", "old-download").unwrap().is_none());
    assert!(catalogue.list("checkpoint").unwrap().is_empty());
    assert!(catalogue.list("download_intent").unwrap().is_empty());
    // Without an explicit local owner, ordinary restore retains no old jobs.
    f.service
        .disk_operation(
            "restore_backup",
            &json!({"path":backup,"destinationPath":f.path.join("ordinary-restored")}),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(catalogue.list("job").unwrap().is_empty());
}

#[test]
fn backup_manifest_omitted_ready_payload_cannot_replace_catalogue() {
    let f = Fixture::new();
    f.seed(1, true);
    let a = f.artifact(0);
    let manifest = f.service.catalogue.manifest().unwrap();
    let backup = f.path.join("incomplete.zip");
    portable::create_backup(&backup, manifest, &[], &AtomicBool::new(false)).unwrap();
    assert!(f
        .service
        .disk_operation(
            "restore_backup",
            &json!({"path":backup,"destinationPath":f.path.join("restore")}),
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", a["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        a
    );
    assert_eq!(
        f.service.root(Some("test-root")).unwrap()["path"],
        json!(f.path.join("library"))
    );
}

#[test]
fn archive_deletion_crash_reconciles_only_with_verified_extracted_inventory() {
    let f = Fixture::new();
    f.seed(1, true);
    let mut a = f.artifact(0);
    let path = f.path.join("library/extracted/body.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"verified extraction").unwrap();
    let (hash, size) = io::hash_file(&path).unwrap();
    a["extractedPath"] = json!("extracted");
    a["extractedInventory"] = json!([{"relativePath":"body.txt","hash":hash,"sizeBytes":size}]);
    a["originalDeletionPending"] = json!(true);
    f.service
        .catalogue
        .put("artifact", a["id"].as_str().unwrap(), a.clone())
        .unwrap();
    std::fs::remove_file(
        f.path
            .join("library/assets")
            .join(io::readable_component("Synthetic clothing", "42"))
            .join("fixture-0/fixture-0.unitypackage"),
    )
    .unwrap();
    f.service.reconcile_archive_cleanup().unwrap();
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", a["id"].as_str().unwrap())
            .unwrap()
            .unwrap()["originalAvailable"],
        false
    );
    assert!(path.exists());
}

#[test]
fn cancelled_verification_never_marks_completed_artifacts_missing() {
    let f = Fixture::new();
    f.seed(1, true);
    let a = f.artifact(0);
    assert!(f
        .service
        .disk_operation(
            "verify_root",
            &json!({"rootId":"test-root"}),
            &AtomicBool::new(true)
        )
        .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", a["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        a
    );
}

#[test]
fn empty_extraction_inventory_cannot_restore_ready_or_replace_catalogue() {
    let f = Fixture::new();
    f.seed(1, true);
    let a = f.artifact(0);
    let mut manifest = f.service.catalogue.manifest().unwrap();
    let artifact = &mut manifest["records"]["artifact"][0];
    artifact["originalAvailable"] = json!(false);
    artifact["extractedPath"] = json!("nonexistent");
    artifact["extractedInventory"] = json!([]);
    let backup = f.path.join("invalid-empty.zip");
    portable::create_backup(&backup, manifest, &[], &AtomicBool::new(false)).unwrap();
    assert!(f
        .service
        .disk_operation(
            "restore_backup",
            &json!({"path":backup,"destinationPath":f.path.join("restore")}),
            &AtomicBool::new(false)
        )
        .is_err());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", a["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        a
    );
}

#[test]
fn product_availability_uses_actual_original_and_extracted_paths() {
    let f = Fixture::new();
    f.seed(1, true);
    let mut a = f.artifact(0);
    let args = json!({"productId":"42"});
    assert_eq!(
        f.service.product_availability(&args).unwrap()["openableFileIds"],
        json!(["test-file-0"])
    );
    a["originalAvailable"] = json!(false);
    f.service
        .catalogue
        .put("artifact", a["id"].as_str().unwrap(), a.clone())
        .unwrap();
    let unavailable = f.service.product_availability(&args).unwrap();
    assert_eq!(unavailable["folderAvailable"], false);
    assert_eq!(unavailable["openableFileIds"], json!([]));
}

#[test]
fn restart_discards_only_owned_partial_and_preserves_completed_artifact() {
    let f = Fixture::new();
    f.seed(2, true);
    let ready = f.artifact(0);
    let root = f.path.join("library");
    std::fs::create_dir_all(root.join(".nna-partials")).unwrap();
    std::fs::write(root.join(".nna-partials/pending-test.part"), b"partial").unwrap();
    std::fs::write(root.join("keep.bin"), b"unrelated original").unwrap();
    f.service.catalogue.put("artifact","pending-test",json!({"id":"pending-test","productId":"42","fileId":"test-file-1","rootId":"test-root","relativePath":"assets/incomplete.bin","status":"downloading","originalAvailable":false})).unwrap();
    f.service.reconcile_archive_cleanup().unwrap();
    assert!(!root.join(".nna-partials/pending-test.part").exists());
    assert!(!root.join(".nna-partials").exists());
    assert!(root.join("keep.bin").exists());
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", ready["id"].as_str().unwrap())
            .unwrap()
            .unwrap(),
        ready
    );
    assert_eq!(
        f.service
            .catalogue
            .get("artifact", "pending-test")
            .unwrap()
            .unwrap()["status"],
        "notDownloaded"
    );
}
