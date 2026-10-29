use super::*;
use std::path::PathBuf;

#[test]
fn startup_recovery_isolates_orphan_jobs_and_keeps_later_jobs_resumable() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    account(catalogue, A, false);
    for (id, owner) in [("authored-orphan", B), ("authored-healthy", A)] {
        let job = json!({"id":id,"type":"boothSync","accountId":owner,"state":"running"});
        catalogue.put("job", id, job.clone()).unwrap();
        let recovered = fixture.service.prepare_recovered_job(job).unwrap();
        assert_eq!(recovered.is_some(), owner == A);
    }
    let orphan = catalogue.get("job", "authored-orphan").unwrap().unwrap();
    assert_eq!(orphan["state"], "needsAction");
    assert_eq!(orphan["errorCode"], "ACCOUNT_UNAVAILABLE");
    assert_eq!(catalogue.get("job", "authored-healthy").unwrap().unwrap()["state"], "running");
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn stale_retry_cannot_overwrite_the_first_retry_attempt() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    let previous = json!({"id":"authored-retry","type":"boothLogin","accountId":A,"state":"needsAction","errorCode":"RETRY_REQUIRED"});
    catalogue.put("job", "authored-retry", previous.clone()).unwrap();
    let mut running = fixture.service.requeue_job(&previous).unwrap();
    assert_eq!(running["state"], "queued");
    running["state"] = json!("running");
    running["progress"] = json!({"pages":5});
    catalogue.put("job", "authored-retry", running.clone()).unwrap();
    assert_eq!(code(&fixture.service.requeue_job(&previous).unwrap_err()), "JOB_BUSY");
    assert_eq!(catalogue.get("job", "authored-retry").unwrap().unwrap(), running);
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn successful_sync_clears_previous_action_state_without_replacing_identity() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    let account = json!({"id":A,"store":"BOOTH","status":"needsAction","state":"needsAction","identityVerified":true,"identityHash":"authored-identity","authenticatedAt":"2026-01-01T00:00:00Z"});
    catalogue.put("account", A, account.clone()).unwrap();
    let job = json!({"id":"authored-recovered-sync","type":"boothSync","accountId":A,"state":"running"});
    catalogue.put("job", "authored-recovered-sync", job.clone()).unwrap();
    fixture.service.finish_job(&job, Some("PUBLIC_METADATA_FAILED")).unwrap();
    assert_eq!(catalogue.get("account", A).unwrap().unwrap()["state"], "needsAction");
    catalogue.put("job", "authored-recovered-sync", job.clone()).unwrap();
    fixture.service.finish_job(&job, None).unwrap();
    let recovered = catalogue.get("account", A).unwrap().unwrap();
    assert_eq!(recovered["state"], "connected");
    assert_eq!(recovered["status"], "connected");
    assert_eq!(recovered["syncState"], "complete");
    for key in ["identityVerified", "identityHash", "authenticatedAt"] {
        assert_eq!(recovered[key], account[key]);
    }
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn completed_incomplete_attempt_retains_execution_and_source_diagnostics_in_database() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    let mut job = json!({"id":"authored-capture-lifecycle","type":"boothSync","accountId":A,
        "state":"queued","progress":{"stage":"collecting","pages":0}});
    crate::support::begin_attempt(&mut job);
    catalogue.put("job", "authored-capture-lifecycle", job.clone()).unwrap();
    let (_, trace) = crate::support_trace::capture(async {
        crate::support_trace::source_start("purchase");
        crate::support_trace::source_reason("purchase", "SOURCE_HEAD_CHANGED", 1);
        crate::support_trace::source_closure("purchase", true, 0, 1, 0, 0, false, false);
        crate::support_trace::source_done("purchase", Some(false), None);
        let mut durable = job.clone();
        durable["progress"] = json!({"stage":"finalizing","pages":9,"detailsCompleted":4});
        catalogue.put("job", "authored-capture-lifecycle", durable).unwrap();
    }).await;
    job["supportDiagnostics"] = trace;
    crate::support::end_attempt(&mut job);
    fixture.service.finish_job(&job, Some("SYNC_INCOMPLETE")).unwrap();
    let stored = catalogue.get("job", "authored-capture-lifecycle").unwrap().unwrap();
    assert_eq!(stored["state"], "needsAction");
    assert_eq!(stored["progress"]["pages"], 9);
    let report: Value = serde_json::from_str(&crate::support::report(&stored, &[], &[])).unwrap();
    assert_eq!(report["captureAvailable"], true);
    assert_eq!(report["capture"]["attemptState"], "finished");
    assert_eq!(report["capture"]["executionAppVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["supportDiagnostics"]["sources"]["purchase"]["reasons"]["SOURCE_HEAD_CHANGED"], 1);
    assert_eq!(report["supportDiagnostics"]["sources"]["purchase"]["lastClosure"]["headStable"], false);
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn auth_required_expires_sync_account_without_erasing_identity_or_assets() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    let account = json!({"id":A,"store":"BOOTH","status":"connected","state":"connected","authenticatedAt":"2026-01-01T00:00:00Z","identityVerified":true,"identityHash":"authored-identity","lastSyncAt":"2026-01-02T00:00:00Z"});
    catalogue.put("account", A, account.clone()).unwrap();
    let product = json!({"id":"authored-product","title":"Retained asset"});
    catalogue
        .put("product", "authored-product", product.clone())
        .unwrap();
    let job =
        json!({"id":"authored-auth-failure","type":"boothSync","accountId":A,"state":"running"});
    catalogue
        .put("job", "authored-auth-failure", job.clone())
        .unwrap();
    fixture
        .service
        .finish_job(&job, Some("AUTH_REQUIRED"))
        .unwrap();
    let expired = catalogue.get("account", A).unwrap().unwrap();
    assert_eq!(expired["status"], "expired");
    assert_eq!(expired["state"], "expired");
    for key in [
        "authenticatedAt",
        "identityVerified",
        "identityHash",
        "lastSyncAt",
    ] {
        assert_eq!(expired[key], account[key]);
    }
    assert_eq!(
        catalogue
            .get("product", "authored-product")
            .unwrap()
            .unwrap(),
        product
    );
    catalogue.put("account", A, account).unwrap();
    fixture
        .service
        .finish_job(&job, Some("NETWORK_UNAVAILABLE"))
        .unwrap();
    assert_eq!(
        catalogue.get("account", A).unwrap().unwrap()["status"],
        "connected"
    );
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn terminal_sync_persists_cancel_error_and_success_with_account_attempt_end() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    catalogue
        .put(
            "account",
            A,
            json!({"id":A,"store":"BOOTH","status":"connected"}),
        )
        .unwrap();
    let mut job = json!({"id":"synthetic-terminal","type":"boothSync","accountId":A,"state":"running","progress":{"stage":"enriching"}});
    catalogue
        .put("job", "synthetic-terminal", job.clone())
        .unwrap();
    catalogue.command("preferences", json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}})).unwrap();
    fixture
        .service
        .finish_job(&job, Some("PUBLIC_METADATA_FAILED"))
        .unwrap();
    assert_eq!(catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"], json!({}));
    let failed = catalogue.get("job", "synthetic-terminal").unwrap().unwrap();
    assert_eq!(failed["state"], "needsAction");
    assert_eq!(failed["errorCode"], "PUBLIC_METADATA_FAILED");
    assert!(failed["completedAt"].as_str().is_some());
    let account = catalogue.get("account", A).unwrap().unwrap();
    assert_eq!(account["lastSyncAttemptAt"], failed["completedAt"]);
    assert!(account["lastSyncAt"].is_null());
    assert_eq!(account["syncState"], "incomplete");
    assert_eq!(account["lastSyncErrorCode"], "PUBLIC_METADATA_FAILED");
    job["state"] = json!("cancelled");
    job["progress"] =
        json!({"stage":"collecting","pages":91,"detailsCompleted":533,"published":252});
    catalogue
        .put("job", "synthetic-terminal", job.clone())
        .unwrap();
    let mut stale = job.clone();
    stale["state"] = json!("running");
    stale["progress"] = json!({"stage":"collecting","pages":0,"products":0});
    catalogue.command("preferences", json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}})).unwrap();
    fixture.service.finish_job(&stale, None).unwrap();
    assert_eq!(catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"], json!({}));
    let cancelled = catalogue.get("job", "synthetic-terminal").unwrap().unwrap();
    assert_eq!(cancelled["progress"]["pages"], 91);
    assert_eq!(cancelled["progress"]["detailsCompleted"], 533);
    assert_eq!(cancelled["progress"]["published"], 252);
    assert_eq!(
        catalogue.get("job", "synthetic-terminal").unwrap().unwrap()["state"],
        "cancelled"
    );
    catalogue
        .put("job", "synthetic-terminal", stale.clone())
        .unwrap();
    catalogue.command("preferences", json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}})).unwrap();
    fixture.service.finish_job(&stale, None).unwrap();
    assert_eq!(catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"], json!({}));
    let complete = catalogue.get("job", "synthetic-terminal").unwrap().unwrap();
    assert_eq!(complete["state"], "completed");
    assert!(complete["errorCode"].is_null());
    assert_eq!(
        catalogue.get("account", A).unwrap().unwrap()["syncState"],
        "complete"
    );
    assert_eq!(
        catalogue.get("account", A).unwrap().unwrap()["lastSyncAt"],
        complete["completedAt"]
    );
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn entry_diagnostic_accepts_only_a_nonempty_account_argument() {
    assert_eq!(
        entry_diagnostic_account(&json!({"accountId":A})).unwrap(),
        A
    );
    for args in [
        json!({}),
        json!(null),
        json!({"accountId":""}),
        json!({"accountId":1}),
        json!({"accountId":A,"route":"https://evil.test/"}),
        json!({"accountId":A,"itemId":"900001"}),
        json!({"accountId":A,"source":"free"}),
    ] {
        assert_eq!(
            entry_diagnostic_account(&args).unwrap_err().to_string(),
            "INVALID_DIAGNOSTIC_ARGUMENTS"
        );
    }
}
#[test]
fn targeted_product_diagnostic_accepts_only_account_and_numeric_receipt() {
    assert_eq!(
        product_line_diagnostic_args(&json!({"accountId":A,"receiptId":"910001"})).unwrap(),
        (A, "910001")
    );
    for args in [
        json!({}),
        json!(null),
        json!({"accountId":A}),
        json!({"accountId":"","receiptId":"910001"}),
        json!({"accountId":A,"receiptId":910001}),
        json!({"accountId":A,"receiptId":""}),
        json!({"accountId":A,"receiptId":"910001?x=1"}),
        json!({"accountId":A,"receiptId":"../910001"}),
        json!({"accountId":A,"receiptId":"https://accounts.booth.pm/orders/910001"}),
        json!({"accountId":A,"receiptId":"1234567890123456789012345"}),
        json!({"accountId":A,"receiptId":"910001","route":"/orders/910002"}),
    ] {
        assert_eq!(
            product_line_diagnostic_args(&args).unwrap_err().to_string(),
            "INVALID_DIAGNOSTIC_ARGUMENTS"
        );
    }
}

#[test]
fn diagnostic_source_is_a_closed_enum_and_always_targets_page_one() {
    assert_eq!(diagnostic_source(&json!({})).unwrap(), "library");
    for (source, path) in [
        ("library", "/library"),
        ("purchase", "/orders"),
        ("gift", "/library/gifts"),
        ("free", "/library/free_downloads"),
    ] {
        let args = json!({"source":source,"route":"https://evil.test/private","page":99});
        let selected = diagnostic_source(&args).unwrap();
        let route = format!("{}?page=1", safety::source_path(selected).unwrap());
        assert_eq!(
            safety::route(&route, selected).unwrap().as_str(),
            format!("https://accounts.booth.pm{path}?page=1")
        );
    }
    for value in [
        json!(null),
        json!(false),
        json!(1),
        json!({}),
        json!([]),
        json!(""),
        json!("all"),
        json!("orders"),
        json!("gifts"),
        json!("FREE"),
        json!("/library/free_downloads"),
        json!("https://evil.test/"),
    ] {
        assert_eq!(
            diagnostic_source(&json!({"source":value}))
                .unwrap_err()
                .to_string(),
            "INVALID_SOURCE"
        );
    }
}

struct Fixture {
    service: BoothService,
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("nna-booth-synthetic-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        Self {
            service: BoothService::new(catalogue).unwrap(),
            path,
        }
    }
}
fn row() -> parser::Row {
    parser::Row {
        source_record_id: "library:independently-authored-product".into(),
        product_id: Some("900001".into()),
        name: Some("Invented avatar".into()),
        shop: Some("Invented maker".into()),
        files: vec![parser::File {
            id: "700001".into(),
            variant_id: Some("800001".into()),
            name: Some("invented.psd".into()),
            size_bytes: Some(8),
            source_url: "https://booth.pm/downloadables/700001?variation_id=800001".into(),
            revision: "unknown".into(),
        }],
        manifest_complete: true,
        ..Default::default()
    }
}
fn account(catalogue: &Catalogue, id: &str, disconnected: bool) {
    catalogue
        .put(
            "account",
            id,
            json!({"id":id,"name":"Invented","store":"BOOTH","disconnected":disconnected}),
        )
        .unwrap();
}
const A: &str = "10000000-0000-4000-8000-000000000001";
const B: &str = "10000000-0000-4000-8000-000000000002";
#[test]
fn selected_update_scopes_bind_accounts_and_keep_public_reads_independent() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    account(catalogue, A, false);
    account(catalogue, B, false);
    for (key, owner, product) in [("one", A, "900001"), ("two", B, "900002")] {
        catalogue.put("entitlement", key, json!({"id":key,"accountId":owner,"productId":product,"source":"library"})).unwrap();
    }
    catalogue.put("acquisition", "free", json!({"id":"free","accountId":B,"productId":"900001","source":"free"})).unwrap();
    let scopes = fixture.service.update_scopes(&BTreeSet::from(["900001".into(), "900002".into(), "900003".into()])).unwrap();
    assert_eq!(scopes, vec![
        json!({"accountId":A,"productIds":["900001"]}),
        json!({"accountId":B,"productIds":["900001","900002"]}),
        json!({"productIds":["900003"],"publicOnly":true}),
    ]);
    for scope in scopes {
        let job = fixture.service.make_job("boothUpdate", scope).unwrap();
        if let Some(owner) = job["accountId"].as_str() {
            assert_eq!(catalogue.require_account_idle(owner).unwrap_err().to_string(), "ACCOUNT_BUSY");
            assert_eq!(catalogue.remove_account(owner, true).unwrap_err().to_string(), "ACCOUNT_BUSY");
            catalogue.delete("job", job["id"].as_str().unwrap()).unwrap();
        } else {
            catalogue.put("entitlement", "later", json!({"id":"later","accountId":B,"productId":"900003","source":"library"})).unwrap();
            catalogue.require_account_idle(A).unwrap();
            catalogue.require_account_idle(B).unwrap();
        }
    }
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn legacy_selected_update_keeps_its_owning_account_busy() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    account(catalogue, A, false);
    account(catalogue, B, false);
    catalogue.put("entitlement", "owned", json!({"id":"owned","accountId":A,"productId":"900001"})).unwrap();
    catalogue.put("job", "legacy-update", json!({"id":"legacy-update","type":"boothUpdate","state":"running","args":{"productIds":["900001"]}})).unwrap();
    assert_eq!(catalogue.remove_account(A, true).unwrap_err().to_string(), "ACCOUNT_BUSY");
    catalogue.require_account_idle(B).unwrap();
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn store_journal_pruning_retains_current_sources_and_running_gumroad_runs() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    let checkpoints = [
        json!({"id":format!("booth:{A}:library"),"accountId":A,"source":"library","runId":"current-library","journalIds":["booth-page:current-library:1"]}),
        json!({"id":format!("booth:{A}:purchase"),"accountId":A,"source":"purchase","runId":"current-purchase"}),
        json!({"id":"booth-page:current-library:1","accountId":A,"source":"library","runId":"current-library"}),
        json!({"id":"booth-product:current-purchase:1","accountId":A,"source":"purchase","runId":"current-purchase"}),
        json!({"id":"booth-page:obsolete:1","accountId":A,"source":"library","runId":"obsolete"}),
        json!({"id":"booth-product:obsolete:1","accountId":A,"source":"purchase","runId":"obsolete"}),
        json!({"id":"booth-page:other:1","accountId":B,"source":"library","runId":"other"}),
        json!({"id":format!("gumroad:{A}:library"),"store":"Gumroad","accountId":A,"jobId":"latest"}),
        json!({"id":format!("gumroad:{A}:latest:0:1"),"store":"Gumroad","accountId":A,"runId":"latest"}),
        json!({"id":format!("gumroad:{A}:active:0:1"),"store":"Gumroad","accountId":A,"runId":"active"}),
        json!({"id":format!("gumroad:{A}:obsolete:0:1"),"store":"Gumroad","accountId":A,"runId":"obsolete"}),
        json!({"id":format!("gumroad:{A}:selected:obsolete"),"store":"Gumroad","accountId":A,"jobId":"obsolete"}),
        json!({"id":"local-import:keep","accountId":A,"runId":"obsolete"}),
    ];
    for checkpoint in checkpoints {
        catalogue.put("checkpoint", checkpoint["id"].as_str().unwrap(), checkpoint.clone()).unwrap();
    }
    catalogue.put("job", "active", json!({"id":"active","type":"gumroadSync","store":"Gumroad","accountId":A,"state":"running"})).unwrap();
    assert_eq!(catalogue.prune_store_checkpoints("BOOTH", A).unwrap(), 2);
    assert_eq!(catalogue.prune_store_checkpoints("Gumroad", A).unwrap(), 2);
    assert_eq!(catalogue.prune_store_checkpoints("BOOTH", A).unwrap(), 0);
    assert_eq!(catalogue.prune_store_checkpoints("Gumroad", A).unwrap(), 0);
    assert!(catalogue.get("checkpoint", "booth-page:current-library:1").unwrap().is_some());
    assert!(catalogue.get("checkpoint", "booth-product:current-purchase:1").unwrap().is_some());
    assert!(catalogue.get("checkpoint", "booth-page:other:1").unwrap().is_some());
    assert!(catalogue.get("checkpoint", &format!("gumroad:{A}:latest:0:1")).unwrap().is_some());
    assert!(catalogue.get("checkpoint", &format!("gumroad:{A}:active:0:1")).unwrap().is_some());
    assert!(catalogue.get("checkpoint", "local-import:keep").unwrap().is_some());
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn library_and_purchase_publish_and_queue_ten_files_in_either_arrival_order() {
    let mut controls = String::new();
    for offset in 0..10 {
        let file = 700001 + offset;
        let dropdown = serde_json::to_string(&json!([
            {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id=800001&client=booth-library-manager")},
            {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id=800001&client=vroid&type=zip")}
        ])).unwrap();
        controls.push_str(&format!("<div class='legacy-list-item'><div class='min-w-0'><b>authored-{offset}.zip</b></div><a data-revision='v1' href='https://booth.pm/downloadables/{file}'>Download</a><button data-test='other-downloads-button' data-dropdown-items='{dropdown}'></button></div>"));
    }
    let product = "<a href='https://invented.booth.pm/items/900001'>Authored product</a>";
    let library = parser::parse(&format!("<main><h1>Library</h1><article data-test='library-item'>{product}<div data-test='downloadable-list'>{controls}</div></article></main>"), "library", "/library?page=1").unwrap();
    let purchase = parser::parse(&format!("<main><div class='sheet'>{product}{controls}</div></main>"), "purchase", "/orders/910001").unwrap();
    for page in [&library, &purchase] {
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].files.len(), 10);
        assert!(page.rows[0].manifest_complete);
        assert!(page.rows[0].files.iter().all(|file| file.variant_id.as_deref() == Some("800001")));
    }
    for pages in [[&library, &purchase], [&purchase, &library]] {
        let fixture = Fixture::new();
        let catalogue = &fixture.service.inner.catalogue;
        account(catalogue, A, false);
        let root = fixture.path.join("library");
        std::fs::create_dir_all(&root).unwrap();
        catalogue.put("root", "authored-root", json!({"id":"authored-root","path":root,"status":"online"})).unwrap();
        let expected: BTreeSet<_> = (0..10).map(|offset| format!("{A}:{}:800001", 700001 + offset)).collect();
        for (index, page) in pages.into_iter().enumerate() {
            fixture.service.record_row(A, &page.source, &page.rows[0], &page.route, false).unwrap();
            let remote = catalogue.related("remote_file", "900001").unwrap();
            assert_eq!(remote.len(), 10);
            assert_eq!(remote.iter().map(|file| file["id"].as_str().unwrap().to_owned()).collect::<BTreeSet<_>>(), expected);
            let details = catalogue.command("product_details", json!({"id":"900001"})).unwrap();
            assert_eq!(details["files"].as_array().unwrap().len(), 10);
            assert_eq!(details["total"], 10);
            let required: BTreeSet<_> = catalogue.effective_entitlements("900001").unwrap().into_iter()
                .flat_map(|entitlement| entitlement["fileIds"].as_array().unwrap().iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>())
                .collect();
            assert_eq!(required, expected);
            if index == 0 {
                for file in &remote {
                    let id = file["id"].as_str().unwrap();
                    let relative = format!("authored/{}.zip", file["sourceFileId"].as_str().unwrap());
                    let path = root.join(&relative);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, b"independently authored bytes").unwrap();
                    catalogue.put("artifact", id, json!({"id":id,"fileId":id,"productId":"900001","rootId":"authored-root","relativePath":relative,"revision":file["revision"],"status":"ready","originalAvailable":true})).unwrap();
                }
            }
            let details = catalogue.command("product_details", json!({"id":"900001"})).unwrap();
            assert_eq!(details["status"], "ready");
            assert_eq!(details["downloaded"], 10);
        }
        let stores = Arc::new(crate::stores::StoreService::new(catalogue.clone(), Arc::new(fixture.service.clone())).unwrap());
        let storage = crate::storage::StorageService::new(catalogue.clone(), stores.clone()).unwrap();
        let result = storage.enqueue_for_test(&json!({"productIds":["900001"]})).await.unwrap();
        let job_id = result["jobIds"][0].as_str().unwrap();
        let job = catalogue.get("job", job_id).unwrap().unwrap();
        assert_eq!(job["state"], "queued");
        assert_eq!(job["fileIds"].as_array().unwrap().len(), 10);
        assert_eq!(job["fileIds"].as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect::<BTreeSet<_>>(), expected);
        drop(storage);
        drop(stores);
        let path = fixture.path.clone();
        drop(fixture);
        let reopened = Catalogue::open(&path).unwrap();
        assert_eq!(reopened.command("product_details", json!({"id":"900001"})).unwrap()["files"].as_array().unwrap().len(), 10);
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }
}
#[test]
fn native_paid_triple_and_sparse_refresh_survive_restart_without_duplicate_acquisitions() {
    let fixture = Fixture::new();
    let mut purchase = row();
    purchase.source_record_id = "purchase:authored-paid".into();
    purchase.order_id = Some("910001".into());
    purchase.acquired_date_text = Some("2026/10/10 09:30".into());
    purchase.paid_amount_text = Some("authored historical text".into());
    purchase.product_line_total_jpy = Some(430);
    fixture
        .service
        .record_row(A, "purchase", &purchase, "/orders/910001", false)
        .unwrap();
    let id = format!("{A}:{}", purchase.source_record_id);
    let before = fixture
        .service
        .inner
        .catalogue
        .get("acquisition", &id)
        .unwrap()
        .unwrap();
    assert_eq!(before["paidPrice"], 430);
    assert_eq!(before["paidCurrency"], "JPY");
    assert_eq!(before["paidPriceBasis"], "productLineTotal");
    let mut sparse = purchase.clone();
    sparse.acquired_date_text = None;
    sparse.paid_amount_text = None;
    sparse.product_line_total_jpy = None;
    sparse.files.clear();
    sparse.manifest_complete = false;
    sparse.error_code = Some("DIGITAL_DETAIL_ROLE_UNKNOWN".into());
    for _ in 0..2 {
        fixture
            .service
            .record_row(A, "purchase", &sparse, "/orders/910001", false)
            .unwrap();
    }
    let path = fixture.path.clone();
    drop(fixture);
    let catalogue = Catalogue::open(&path).unwrap();
    let service = BoothService::new(catalogue).unwrap();
    service
        .record_row(A, "purchase", &sparse, "/orders/910001", false)
        .unwrap();
    let retained = service
        .inner
        .catalogue
        .get("acquisition", &id)
        .unwrap()
        .unwrap();
    for key in [
        "productId",
        "observedProductId",
        "orderId",
        "acquiredDateText",
        "paidAmountText",
        "paidPrice",
        "paidCurrency",
        "paidPriceBasis",
    ] {
        assert_eq!(retained[key], before[key], "{key}");
    }
    assert_eq!(
        service.inner.catalogue.list("acquisition").unwrap().len(),
        1
    );
    purchase.product_line_total_jpy = Some(0);
    service
        .record_row(A, "purchase", &purchase, "/orders/910001", false)
        .unwrap();
    assert_eq!(
        service
            .inner
            .catalogue
            .get("acquisition", &id)
            .unwrap()
            .unwrap()["paidPrice"],
        0
    );
    drop(service);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn native_paid_producer_rejects_sparse_unbound_and_unsafe_rows() {
    let fixture = Fixture::new();
    for case in 1..13 {
        let mut purchase = row();
        purchase.source_record_id = format!("purchase:gate-{case}");
        purchase.order_id = Some("910001".into());
        purchase.product_line_total_jpy = Some(0);
        purchase.paid_amount_text = Some("¥ 999".into());
        let mut source = "purchase";
        let mut route = "/orders/910001";
        match case {
            1 => source = "library",
            2 => purchase.order_id = None,
            3 => purchase.product_id = None,
            4 => purchase.files.clear(),
            5 => purchase.files[0].variant_id = Some("999999".into()),
            6 => purchase.unavailable = true,
            7 => purchase.error_code = Some("AUTH_REQUIRED".into()),
            8 => purchase.product_line_total_jpy = Some(9_007_199_254_740_992),
            9 => purchase.files[0].source_url = "https://evil.test/file".into(),
            10 => purchase.files[0].id = "999999".into(),
            11 => route = "/orders/910002",
            12 => purchase.product_line_total_jpy = None,
            _ => {}
        }
        fixture
            .service
            .record_row(A, source, &purchase, route, false)
            .unwrap();
        let value = fixture
            .service
            .inner
            .catalogue
            .get("acquisition", &format!("{A}:{}", purchase.source_record_id))
            .unwrap()
            .unwrap();
        for key in ["paidPrice", "paidCurrency", "paidPriceBasis"] {
            assert!(value.get(key).is_none(), "{case} {key}");
        }
    }
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn native_paid_fields_do_not_require_complete_filenames() {
    let fixture = Fixture::new();
    let mut purchase = row();
    purchase.source_record_id = "purchase:needed-fields".into();
    purchase.order_id = Some("910001".into());
    purchase.acquired_date_text = Some("2026/10/10 09:30".into());
    purchase.product_line_total_jpy = Some(0);
    purchase.manifest_complete = false;
    purchase.files[0].name = None;
    fixture
        .service
        .record_row(A, "purchase", &purchase, "/orders/910001", false)
        .unwrap();
    let acquired = fixture
        .service
        .inner
        .catalogue
        .get("acquisition", &format!("{A}:{}", purchase.source_record_id))
        .unwrap()
        .unwrap();
    assert_eq!(acquired["paidPrice"], 0);
    assert_eq!(acquired["paidPriceBasis"], "productLineTotal");
    assert_eq!(acquired["acquiredDateText"], "2026/10/10 09:30");
    assert_eq!(
        fixture
            .service
            .inner
            .catalogue
            .get(
                "entitlement",
                &format!("{A}:purchase:{}", purchase.product_id.unwrap())
            )
            .unwrap()
            .unwrap()["manifestComplete"],
        false
    );
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn purchase_date_identity_reuses_only_unique_exact_saved_fallback() {
    let fixture = Fixture::new();
    let mut purchase = row();
    purchase.order_id = Some("910001".into());
    purchase.source_record_id = format!("purchase:{}", safety::hash("910001:900001:::"));
    purchase.acquired_date_text = Some("2026/10/10 09:30".into());
    purchase.product_line_total_jpy = Some(100);
    purchase.manifest_complete = false;
    purchase.files[0].name = None;
    fixture
        .service
        .record_row(A, "purchase", &purchase, "/orders/910001", false)
        .unwrap();
    let old_id = format!("{A}:{}", purchase.source_record_id);
    purchase.source_record_id = format!(
        "purchase:{}",
        safety::hash("910001:900001::2026/10/10 09:30:")
    );
    purchase.manifest_complete = true;
    purchase.files[0].name = Some("invented.psd".into());
    purchase.product_line_total_jpy = Some(130);
    fixture
        .service
        .record_row(A, "purchase", &purchase, "/orders/910001", false)
        .unwrap();
    let acquisitions = fixture
        .service
        .inner
        .catalogue
        .related("acquisition", "900001")
        .unwrap();
    assert_eq!(acquisitions.len(), 1);
    assert_eq!(acquisitions[0]["id"], old_id);
    assert_eq!(acquisitions[0]["paidPrice"], 130);
    assert_eq!(acquisitions[0]["acquiredDateText"], "2026/10/10 09:30");
    purchase.order_id = Some("910002".into());
    purchase.source_record_id = format!(
        "purchase:{}",
        safety::hash("910002:900001::2026/10/10 09:30:")
    );
    fixture
        .service
        .record_row(A, "purchase", &purchase, "/orders/910002", false)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .inner
            .catalogue
            .related("acquisition", "900001")
            .unwrap()
            .len(),
        2
    );
    let unknown = parser::Row {
        source_record_id: "purchase:/orders/910003:2".into(),
        order_id: Some("910003".into()),
        error_code: Some("DIGITAL_CONTROLS_UNRESOLVED".into()),
        ..Default::default()
    };
    fixture
        .service
        .record_row(A, "purchase", &unknown, "/orders/910003", false)
        .unwrap();
    let saved = fixture
        .service
        .inner
        .catalogue
        .get("acquisition", &format!("{A}:{}", unknown.source_record_id))
        .unwrap()
        .unwrap();
    assert_eq!(saved["orderId"], "910003");
    assert_eq!(saved["errorCode"], "DIGITAL_CONTROLS_UNRESOLVED");
    assert!(saved["productId"].is_null() && saved["paidAmountText"].is_null());
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn sparse_acquisition_does_not_inherit_facts_across_conflicting_bindings() {
    let fixture = Fixture::new();
    for case in 0..7 {
        let mut purchase = row();
        purchase.source_record_id = format!("purchase:conflict-{case}");
        purchase.order_id = Some("910001".into());
        let id = format!("{A}:{}", purchase.source_record_id);
        let mut old = json!({"id":id,"accountId":A,"source":"purchase","sourceRecordId":purchase.source_record_id,"orderId":"910001","productId":"900001","observedProductId":"900001","acquiredDateText":"2026/10/10 09:30","paidAmountText":"old","paidPrice":100,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal"});
        match case {
            0 => old["accountId"] = json!(B),
            1 => old["source"] = json!("gift"),
            2 => old["sourceRecordId"] = json!("purchase:other"),
            3 => purchase.order_id = Some("910002".into()),
            4 => purchase.product_id = Some("900002".into()),
            5 => old["observedProductId"] = json!("900002"),
            6 => old["productId"] = json!("900002"),
            _ => unreachable!(),
        }
        fixture
            .service
            .inner
            .catalogue
            .put("acquisition", &id, old)
            .unwrap();
        purchase.files.clear();
        fixture
            .service
            .record_row(A, "purchase", &purchase, "/orders/910001", false)
            .unwrap();
        let value = fixture
            .service
            .inner
            .catalogue
            .get("acquisition", &id)
            .unwrap()
            .unwrap();
        assert!(value["acquiredDateText"].is_null());
        assert!(value["paidAmountText"].is_null());
        assert!(value["productId"].is_null());
        for key in ["paidPrice", "paidCurrency", "paidPriceBasis"] {
            assert!(value.get(key).is_none(), "{case} {key}");
        }
    }
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn free_file_observation_keeps_original_acquisition_and_retains_incomplete_files() {
    let fixture = Fixture::new();
    let catalogue = &fixture.service.inner.catalogue;
    let mut history = row();
    history.source_record_id = "free:authored-history".into();
    history.acquired_date_text = Some("authored-original-date".into());
    history.files.clear();
    history.manifest_complete = false;
    fixture
        .service
        .record_row(A, "free", &history, "/library/free_downloads?page=1", false)
        .unwrap();
    let acquisition_id = format!("{A}:{}", history.source_record_id);
    let original = catalogue
        .get("acquisition", &acquisition_id)
        .unwrap()
        .unwrap();
    let mut observed = history.clone();
    observed.files = row().files;
    observed.manifest_complete = true;
    fixture
        .service
        .record_row(
            A,
            "free",
            &observed,
            "https://booth.pm/ja/items/900001",
            false,
        )
        .unwrap();
    let after = catalogue
        .get("acquisition", &acquisition_id)
        .unwrap()
        .unwrap();
    assert_eq!(catalogue.list("acquisition").unwrap().len(), 1);
    for (key, value) in original.as_object().unwrap() {
        if key != "observedAt" && key != "variantIds" {
            assert_eq!(&after[key], value);
        }
    }
    assert_eq!(original["variantIds"], json!([]));
    assert_eq!(after["variantIds"], json!(["800001"]));
    assert_eq!(after["id"], original["id"]);
    let entitlement_id = format!("{A}:free:900001");
    let entitlement = catalogue
        .get("entitlement", &entitlement_id)
        .unwrap()
        .unwrap();
    assert_eq!(entitlement["fileIds"].as_array().unwrap().len(), 1);
    assert_eq!(entitlement["manifestComplete"], false);
    fixture
        .service
        .record_row(
            A,
            "free",
            &history,
            "https://booth.pm/ja/items/900001",
            false,
        )
        .unwrap();
    let incomplete = catalogue
        .get("entitlement", &entitlement_id)
        .unwrap()
        .unwrap();
    assert_eq!(incomplete["fileIds"], entitlement["fileIds"]);
    assert_eq!(incomplete["manifestComplete"], false);
    assert_eq!(catalogue.list("remote_file").unwrap().len(), 1);
    assert_eq!(catalogue.list("acquisition").unwrap().len(), 1);
    let sparse = catalogue
        .get("acquisition", &acquisition_id)
        .unwrap()
        .unwrap();
    assert_eq!(sparse["variantIds"], after["variantIds"]);
    assert_eq!(sparse["acquiredDateText"], original["acquiredDateText"]);
    assert_eq!(sparse["sourceRecordId"], original["sourceRecordId"]);
    let path = fixture.path.clone();
    drop(fixture);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn account_reconnect_does_not_enable_sync_before_native_verification() {
    let f = Fixture::new();
    account(&f.service.inner.catalogue, A, true);
    assert!(f.service.known_account(A).is_ok());
    assert!(f.service.account(A).is_err());
    let mut a = f.service.known_account(A).unwrap();
    a["disconnected"] = json!(false);
    f.service.inner.catalogue.put("account", A, a).unwrap();
    assert!(f.service.account(A).is_ok());
    let path = f.path.clone();
    drop(f);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn multiaccount_rows_replay_atomically_without_overwriting_human_source_layers() {
    let f = Fixture::new();
    let c = &f.service.inner.catalogue;
    account(c, A, false);
    account(c, B, false);
    let r = row();
    f.service
        .record_row(A, "library", &r, "/library?page=1", false)
        .unwrap();
    c.put(
        "overlay",
        "900001",
        json!({"id":"900001","title":"Human name","note":"Keep this","rejectedTags":["Rejected"]}),
    )
    .unwrap();
    f.service
        .record_row(A, "library", &r, "/library?page=2", false)
        .unwrap();
    f.service
        .record_row(B, "gift", &r, "/library/gifts?page=1", false)
        .unwrap();
    assert_eq!(c.list("acquisition").unwrap().len(), 2);
    assert_eq!(c.list("remote_file").unwrap().len(), 2);
    assert_eq!(c.list("product").unwrap().len(), 1);
    assert_eq!(
        c.get("overlay", "900001").unwrap().unwrap()["title"],
        "Human name"
    );
    let file = c
        .get("remote_file", &format!("{A}:700001:800001"))
        .unwrap()
        .unwrap();
    assert_eq!(file["sourceUrl"], "https://booth.pm/downloadables/700001");
    assert_eq!(file["variantId"], "800001");
    assert_eq!(file["revision"], "unknown");
    assert!(file["requiresContentVerification"] == true);
    let p = c.command("bootstrap", json!({})).unwrap();
    assert_ne!(p["products"][0]["status"], "ready");
    let path = f.path.clone();
    drop(f);
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn one_process_owns_a_dataset() {
    let f = Fixture::new();
    assert!(BoothService::new(f.service.inner.catalogue.clone()).is_err());
    let path = f.path.clone();
    drop(f);
    std::fs::remove_dir_all(path).unwrap();
}
