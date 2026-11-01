use super::*;

#[test]
fn unsupported_schema_is_rejected_before_persistent_journal_changes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("future.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=99; CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES('preserved');").unwrap();
    drop(conn);
    assert!(Catalogue::from_connection(Connection::open(&path).unwrap(), &fixture.0, &fixture.0, Some(fixture.0.clone()), None).is_err());
    let conn = Connection::open(path).unwrap();
    assert_eq!(conn.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)).unwrap(), "wal");
    assert_eq!(conn.query_row("SELECT value FROM future_data", [], |r| r.get::<_, String>(0)).unwrap(), "preserved");
}

#[test]
fn shared_catalogue_checks_delete_journal_and_setup_accepts_memory_mode() {
    let fixture = Fixture::new();
    let path = fixture.0.join("shared.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
    let c = Catalogue::from_connection(conn, &fixture.0, &fixture.0, Some(fixture.0.clone()), None).unwrap();
    assert_eq!(c.inner.lock().unwrap().query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)).unwrap(), "delete");
    drop(c);
    let setup = Catalogue::open_setup(&fixture.0).unwrap();
    assert_eq!(setup.inner.lock().unwrap().query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)).unwrap(), "memory");
}

#[test]
fn all_summary_queries_include_results_after_10000_without_unbounding_file_payloads() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.inner.lock().unwrap().execute_batch("WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10001) INSERT INTO products(id,body,category,status,sensitive,downloaded,title,search,in_all,summary) SELECT CAST(value AS TEXT),json_object('id',CAST(value AS TEXT),'files',json_array('AUTHORED_DETAIL')),'Tool','notDownloaded',0,0,printf('%05d',value),'authored',1,json_object('id',CAST(value AS TEXT),'filesLoaded',0) FROM n").unwrap();
    let query = |args| c.command("query", args).unwrap();
    let all = query(json!({"allSummaries":true,"includeFiles":false,"presentation":"list","sort":"title"}));
    assert_eq!(all["matched"], 10001);
    assert_eq!(all["products"].as_array().unwrap().len(), 10001);
    assert_eq!(all["products"][10000]["id"], "10001");
    let details = query(json!({"allSummaries":true,"includeFiles":true,"presentation":"list","limit":20000}));
    assert_eq!(details["products"].as_array().unwrap().len(), 10000);
}

#[test]
fn status_filter_survives_every_sort_order() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "wanted");
    product(&c, "other");
    c.put("job", "authored-failure", json!({"id":"authored-failure","productId":"wanted","type":"download","state":"failed","errorCode":"network_failed"})).unwrap();
    for sort in ["title", "acquired", "price", "status"] {
        assert_eq!(c.command("query", json!({"status":"attention","sort":sort})).unwrap()["matchedIds"], json!(["wanted"]));
    }
}

#[test]
fn undo_history_is_bounded_and_noop_edits_do_not_write_records_or_revisions() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "authored-undo");
    c.inner.lock().unwrap().execute_batch("WITH RECURSIVE n(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<100) INSERT INTO undo(token,body) SELECT CAST(value AS TEXT),'[]' FROM n").unwrap();
    let changed = c.command("overlay", json!({"ids":["authored-undo"],"patch":{"note":"Saved note"}})).unwrap();
    assert_eq!(changed["changed"], 1);
    let revision = c.revision().unwrap();
    let unchanged = c.command("overlay", json!({"ids":["authored-undo"],"patch":{"note":"Saved note"}})).unwrap();
    assert_eq!(unchanged["changed"], 0);
    assert!(unchanged.get("undoToken").is_none());
    assert_eq!(c.revision().unwrap(), revision);
    assert_eq!(c.inner.lock().unwrap().query_row("SELECT COUNT(*) FROM undo", [], |row| row.get::<_, u64>(0)).unwrap(), 100);
    assert!(c.inner.lock().unwrap().query_row("SELECT COUNT(*) FROM undo WHERE token='1'", [], |row| row.get::<_, u64>(0)).unwrap() == 0);
    c.command("undo", json!({"token":changed["undoToken"]})).unwrap();
}

#[test]
fn batch_import_status_snapshot_preserves_completed_members_and_updates_after_job_changes() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let mut records = (0..10000).map(|n| {
        let id = format!("authored-history-{n}");
        ("job".into(), id.clone(), json!({"id":id,"type":"download","state":"completed"}))
    }).collect::<Vec<_>>();
    for id in ["one", "two"] {
        records.push(("product".into(), id.into(), json!({"id":id,"title":"Authored import","sourceCategory":{"name":"Tool"}})));
    }
    let mut job = json!({"id":"authored-import","type":"storage:import_local","state":"running","productIds":["one","two"],"completedProductIds":["one"]});
    records.push(("job".into(), "authored-import".into(), job.clone()));
    c.put_many(records).unwrap();
    let status = |id| c.command("product_details", json!({"id":id})).unwrap()["status"].clone();
    assert_eq!(status("one"), "notDownloaded");
    assert_eq!(status("two"), "processing");
    job["completedProductIds"] = json!(["one", "two"]);
    c.put("job", "authored-import", job).unwrap();
    assert_eq!(status("one"), "notDownloaded");
    assert_eq!(status("two"), "notDownloaded");
}

#[test]
fn tag_edits_reject_only_visible_tags_and_preserve_new_hidden_source_compatibility() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let mut source = json!({"id":"authored-tag-edit","title":"Authored clothes","sourceCategory":{"name":"Clothes"},"sourceTags":["Cute"]});
    c.put("product", "authored-tag-edit", source.clone()).unwrap();
    c.command("overlay", json!({"ids":["authored-tag-edit"],"patch":{"tags":["Cute","Mine"]}})).unwrap();
    source["sourceTags"] = json!(["Cute", "Mafuyu"]);
    c.put("product", "authored-tag-edit", source).unwrap();
    let details = || c.command("product_details", json!({"id":"authored-tag-edit"})).unwrap();
    assert_eq!(details()["tags"], json!(["Cute", "Mine"]));
    assert!(details()["ownedAvatarEvidence"].as_array().unwrap().iter().any(|entry| entry["avatarId"] == "booth:5007531"));
    c.command("overlay", json!({"ids":["authored-tag-edit"],"patch":{"tags":["Cute","Mine","Fav"]}})).unwrap();
    assert!(!strings(&c.get("overlay", "authored-tag-edit").unwrap().unwrap()["rejectedTags"]).contains(&"Mafuyu".into()));
    assert_eq!(c.command("query", json!({"avatar":"booth:5007531"})).unwrap()["matchedIds"], json!(["authored-tag-edit"]));
    c.command("overlay", json!({"ids":["authored-tag-edit"],"patch":{},"clearFields":["tags"]})).unwrap();
    assert!(strings(&details()["tags"]).contains(&"Mafuyu".into()));
    // A tag actually removed from the displayed list still remains rejected.
    c.command("overlay", json!({"ids":["authored-tag-edit"],"patch":{"tags":["Cute"]}})).unwrap();
    assert!(strings(&c.get("overlay", "authored-tag-edit").unwrap().unwrap()["rejectedTags"]).contains(&"Mafuyu".into()));
}

#[test]
fn job_diagnostics_is_read_only_private_and_does_not_mix_later_account_snapshots() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let job = json!({"id":"PRIVATE_JOB","accountId":"PRIVATE_ACCOUNT","accountName":"PRIVATE_NAME",
        "type":"boothSync","state":"failed","errorCode":"SOURCE_HTTP_FAILED",
        "args":{"note":"PRIVATE_INPUT","url":"https://example.invalid/PRIVATE_ROUTE"},
        "progress":{"stage":"failed","pages":2},"supportDiagnostics":{"version":1,"lastStage":"collecting",
            "sources":{"purchase":{"state":"failed","errorCode":"SOURCE_HTTP_FAILED"}},
            "http":{"receipt":{"requests":1,"lastStatus":502}},"financial":{"receiptPages":1,"rowsWithPaidProof":0}}});
    c.put("job", "PRIVATE_JOB", job.clone()).unwrap();
    c.put("source_coverage", "PRIVATE_ACCOUNT:purchase", json!({"id":"PRIVATE_ACCOUNT:purchase","accountId":"PRIVATE_ACCOUNT","source":"purchase","pages":999,"status":"complete"})).unwrap();
    c.put("checkpoint", "PRIVATE_CHECKPOINT", json!({"id":"PRIVATE_CHECKPOINT","accountId":"PRIVATE_ACCOUNT","source":"purchase","pages":998,"routes":["PRIVATE_ROUTE"]})).unwrap();
    let before = c.change_domains().unwrap();
    let report = c.command("job_diagnostics", json!({"jobId":"PRIVATE_JOB"})).unwrap();
    let text = report.as_str().unwrap();
    let value:Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["captureAvailable"], true);
    assert_eq!(value["supportDiagnostics"]["http"]["receipt"]["lastStatus"],502);
    assert!(value.get("sourceCoverage").is_none());
    assert!(value.get("checkpoints").is_none());
    for secret in ["PRIVATE", "999", "998", "https://", "accountId", "cookie"] { assert!(!text.contains(secret), "{secret}"); }
    assert_eq!(c.change_domains().unwrap(),before);
    assert_eq!(c.get("job", "PRIVATE_JOB").unwrap(),Some(job));
    assert_eq!(c.command("job_diagnostics",json!({"jobId":"MISSING_PRIVATE_ID"})).unwrap_err().to_string(),"JOB_UNAVAILABLE");
}

#[test]
fn restore_waits_for_live_and_queued_catalogue_work() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let live = c.begin_catalogue_work().unwrap();
    c.put("job", "pending", json!({"id":"pending","type":"download","state":"cancelled"}))
        .unwrap();
    assert_eq!(
        c.begin_restore_work().err().unwrap().to_string(),
        "storage_busy"
    );
    drop(live);

    c.put("job", "pending", json!({"id":"pending","type":"download","state":"queued"}))
        .unwrap();
    assert_eq!(
        c.begin_restore_work().err().unwrap().to_string(),
        "storage_busy"
    );
    c.put("job", "pending", json!({"id":"pending","type":"download","state":"cancelled"}))
        .unwrap();

    let restore = c.begin_restore_work().unwrap();
    assert_eq!(
        c.begin_catalogue_work().err().unwrap().to_string(),
        "storage_busy"
    );
    drop(restore);
    assert!(c.begin_catalogue_work().is_ok());
}

#[test]
fn account_removal_busy_guard_preserves_records_until_related_jobs_are_terminal() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put("account", "a", json!({"id":"a","identityVerified":true}))
        .unwrap();
    for (kind, product_id) in [
        ("acquisition", "purchase-product"),
        ("entitlement", "entitled-product"),
        ("remote_file", "file-product"),
    ] {
        product(&c, product_id);
        c.put(
            kind,
            kind,
            json!({"id":kind,"productId":product_id,"accountId":"a"}),
        )
        .unwrap();
    }
    for binding in [
        json!({"type":"boothLogin","accountId":"a","state":"queued"}),
        json!({"type":"gumroadSync","accountId":"a","state":"running"}),
        json!({"type":"download","productId":"purchase-product","state":"queued"}),
        json!({"type":"download","productIds":["unrelated","entitled-product"],"state":"running"}),
        json!({"type":"storage:import_local","productIds":["file-product"],"state":"running"}),
    ] {
        let mut job = binding;
        job["id"] = json!("busy");
        c.put("job", "busy", job.clone()).unwrap();
        let revision = c.change_domains().unwrap();
        assert_eq!(
            c.require_account_idle("a").unwrap_err().to_string(),
            "ACCOUNT_BUSY"
        );
        for remove_products in [false, true] {
            assert_eq!(
                c.remove_account("a", remove_products)
                    .unwrap_err()
                    .to_string(),
                "ACCOUNT_BUSY"
            );
            assert!(c.get("account", "a").unwrap().is_some());
            for (kind, product_id) in [
                ("acquisition", "purchase-product"),
                ("entitlement", "entitled-product"),
                ("remote_file", "file-product"),
            ] {
                assert!(c.get("product", product_id).unwrap().is_some());
                assert!(c.get(kind, kind).unwrap().is_some());
            }
            assert_eq!(c.change_domains().unwrap(), revision);
        }
        job["state"] = json!("cancelled");
        c.put("job", "busy", job).unwrap();
        c.require_account_idle("a").unwrap();
    }
    // Other accounts' active work and unrelated products do not block removal.
    c.put(
        "job",
        "other",
        json!({"id":"other","type":"boothSync","accountId":"b","state":"running"}),
    )
    .unwrap();
    c.put("job", "unrelated", json!({"id":"unrelated","type":"download","productId":"unrelated","productIds":["also-unrelated"],"state":"queued"})).unwrap();
    c.require_account_idle("a").unwrap();
    assert_eq!(
        c.remove_account("a", true).unwrap(),
        json!({"accountRemoved":1,"productRemoved":3,"retainedShared":0})
    );
    assert_eq!(c.get("job", "busy").unwrap().unwrap()["state"], "cancelled");
    assert_eq!(c.get("job", "other").unwrap().unwrap()["state"], "running");
}

#[test]
fn account_removal_without_products_preserves_history_overlays_and_downloaded_files() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "p");
    c.put(
        "account",
        "a",
        json!({"id":"a","name":"Authored account","authenticatedAt":"2026-10-11T00:00:00Z"}),
    )
    .unwrap();
    let acquisition = json!({"id":"purchase","productId":"p","accountId":"a","source":"purchase","sourceRecordId":"authored-receipt","acquiredDateText":"2026/10/01","paidPrice":1200,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal"});
    let file = json!({"id":"f","productId":"p","accountId":"a","name":"Authored.unitypackage","revision":"r1"});
    let entitlement =
        json!({"id":"e","productId":"p","accountId":"a","fileIds":["f"],"manifestComplete":true});
    let relative = "Authored--p/source/Authored.unitypackage";
    let path = fixture.0.join(relative);
    let bytes = b"independently authored downloaded content";
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    use sha2::{Digest, Sha256};
    let artifact = json!({"id":"artifact","productId":"p","fileId":"f","revision":"r1","rootId":"r","status":"ready","originalAvailable":true,"relativePath":relative,"productDirectory":"Authored--p","sizeBytes":bytes.len(),"hash":format!("{:x}",Sha256::digest(bytes))});
    c.put(
        "root",
        "r",
        json!({"id":"r","path":fixture.0,"status":"online"}),
    )
    .unwrap();
    c.put_many(vec![
        ("acquisition".into(), "purchase".into(), acquisition.clone()),
        ("entitlement".into(), "e".into(), entitlement.clone()),
        ("remote_file".into(), "f".into(), file.clone()),
        ("artifact".into(), "artifact".into(), artifact.clone()),
        ("overlay".into(), "p".into(), json!({"id":"p","note":"Human note","rejectedTags":["blue"]})),
        ("job".into(), "login".into(), json!({"id":"login","accountId":"a","type":"boothLogin","state":"failed","errorCode":"AUTH_REQUIRED"})),
    ]).unwrap();
    let before = dynamic_details(&c, "p");
    assert_eq!(before["status"], "ready");
    assert_eq!(
        c.remove_account("a", false).unwrap(),
        json!({"accountRemoved":1,"productRemoved":0,"retainedShared":0})
    );
    assert!(c.get("account", "a").unwrap().is_none());
    for (kind, id, expected) in [
        ("acquisition", "purchase", acquisition),
        ("entitlement", "e", entitlement),
        ("remote_file", "f", file),
        ("artifact", "artifact", artifact),
    ] {
        assert_eq!(c.get(kind, id).unwrap(), Some(expected));
    }
    let after = dynamic_details(&c, "p");
    for field in [
        "status",
        "downloaded",
        "acquired",
        "paidPrice",
        "paidCurrency",
        "note",
        "rejectedTags",
    ] {
        assert_eq!(after[field], before[field], "{field}");
    }
    assert_eq!(after["provenance"][0]["paidPrice"].as_f64(), Some(1200.0));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(c.get("job", "login").unwrap().unwrap()["state"], "failed");
    assert_eq!(
        c.remove_account("a", true).unwrap(),
        json!({"accountRemoved":0,"productRemoved":0,"retainedShared":0})
    );
    let other = Fixture::new();
    let peer = other.open();
    peer.restore_manifest(c.manifest().unwrap()).unwrap();
    assert_eq!(
        peer.get("acquisition", "purchase").unwrap().unwrap()["paidPrice"].as_f64(),
        Some(1200.0)
    );
    assert_eq!(
        peer.get("overlay", "p").unwrap().unwrap()["note"],
        "Human note"
    );
    assert_eq!(
        peer.get("artifact", "artifact").unwrap().unwrap()["relativePath"],
        relative
    );
}

#[test]
fn account_removal_deletes_only_exclusive_products_and_own_source_facts() {
    let fixture = Fixture::new();
    let c = fixture.open();
    for account in ["a", "b"] {
        c.put(
            "account",
            account,
            json!({"id":account,"identityVerified":true}),
        )
        .unwrap();
    }
    for id in ["exclusive", "shared", "local:p"] {
        product(&c, id);
    }
    c.put("product", "local:p", json!({"id":"local:p","provider":"local","store":"Local Files","custom":true,"title":"Authored local"})).unwrap();
    for id in ["exclusive", "shared", "local:p"] {
        let acq_id = format!("a-{id}");
        c.put("acquisition", &acq_id, json!({"id":acq_id,"productId":id,"accountId":"a","paidPrice":1200,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal"})).unwrap();
        c.put(
            "overlay",
            id,
            json!({"id":id,"note":"Preserved human note"}),
        )
        .unwrap();
    }
    // Sharing must be detected even when the other account has no acquisition.
    c.put("remote_file", "b-shared", json!({"id":"b-shared","productId":"shared","accountId":"b","name":"Other.unitypackage","revision":"r1"})).unwrap();
    c.put("entitlement", "b-shared", json!({"id":"b-shared","productId":"shared","accountId":"b","fileIds":["b-shared"],"manifestComplete":true})).unwrap();
    c.put("remote_file", "a-shared", json!({"id":"a-shared","productId":"shared","accountId":"a","name":"Removed.unitypackage","revision":"r1"})).unwrap();
    c.put("entitlement", "a-shared", json!({"id":"a-shared","productId":"shared","accountId":"a","fileIds":["a-shared"],"manifestComplete":true})).unwrap();
    c.put(
        "acquisition",
        "a-unresolved",
        json!({"id":"a-unresolved","accountId":"a","sourceRecordId":"unresolved"}),
    )
    .unwrap();
    c.put("remote_file", "a-exclusive", json!({"id":"a-exclusive","productId":"exclusive","accountId":"a","name":"Authored.unitypackage","revision":"r1"})).unwrap();
    c.put("entitlement", "a-exclusive", json!({"id":"a-exclusive","productId":"exclusive","accountId":"a","fileIds":["a-exclusive"],"manifestComplete":true})).unwrap();
    c.put(
        "root",
        "r",
        json!({"id":"r","path":fixture.0,"status":"online"}),
    )
    .unwrap();
    for id in ["exclusive", "shared"] {
        let relative = format!("{id}/Authored.unitypackage");
        let path = fixture.0.join(&relative);
        let bytes = b"keep downloaded bytes";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        use sha2::{Digest, Sha256};
        c.put("artifact", id, json!({"id":id,"productId":id,"fileId":format!("a-{id}"),"rootId":"r","revision":"r1","status":"downloaded","originalAvailable":true,"sizeBytes":bytes.len(),"hash":format!("{:x}",Sha256::digest(bytes)),"relativePath":relative,"productDirectory":id})).unwrap();
    }
    let other_path = fixture.0.join("shared/Other.unitypackage");
    std::fs::write(&other_path, b"other account downloaded bytes").unwrap();
    use sha2::{Digest, Sha256};
    let other_artifact = json!({"id":"b-shared","productId":"shared","fileId":"b-shared","rootId":"r","revision":"r1","status":"ready","originalAvailable":true,"sizeBytes":30,"hash":format!("{:x}",Sha256::digest(b"other account downloaded bytes")),"relativePath":"shared/Other.unitypackage","productDirectory":"shared"});
    c.put("artifact", "b-shared", other_artifact.clone())
        .unwrap();
    c.put(
        "download_intent",
        "exclusive",
        json!({"id":"exclusive","wantDownload":true}),
    )
    .unwrap();
    c.put("job", "history", json!({"id":"history","accountId":"a","productId":"exclusive","type":"download","state":"cancelled"})).unwrap();
    c.put("collection", "manual", json!({"id":"manual","name":"Manual collection","productIds":["exclusive","shared","local:p"]})).unwrap();
    let other_file = c.get("remote_file", "b-shared").unwrap();
    let other_entitlement = c.get("entitlement", "b-shared").unwrap();
    assert_eq!(
        c.remove_account("a", true).unwrap(),
        json!({"accountRemoved":1,"productRemoved":1,"retainedShared":2})
    );
    assert!(c.get("product", "exclusive").unwrap().is_none());
    assert!(c.get("artifact", "exclusive").unwrap().is_none());
    assert!(c.get("download_intent", "exclusive").unwrap().is_none());
    assert!(c.get("account", "b").unwrap().is_some());
    assert!(c.get("product", "shared").unwrap().is_some());
    assert!(c.get("product", "local:p").unwrap().is_some());
    assert_eq!(c.get("remote_file", "b-shared").unwrap(), other_file);
    assert_eq!(c.get("entitlement", "b-shared").unwrap(), other_entitlement);
    assert!(c.get("artifact", "shared").unwrap().is_none());
    assert_eq!(c.get("artifact", "b-shared").unwrap(), Some(other_artifact));
    for kind in ["acquisition", "entitlement", "remote_file"] {
        assert!(!c
            .list(kind)
            .unwrap()
            .iter()
            .any(|row| row["accountId"] == "a"));
    }
    assert!(c.get("overlay", "exclusive").unwrap().is_none());
    for id in ["shared", "local:p"] {
        assert_eq!(
            c.get("overlay", id).unwrap().unwrap()["note"],
            "Preserved human note"
        );
    }
    let details = dynamic_details(&c, "shared");
    assert_eq!(details["accountIds"], json!(["b"]));
    assert_eq!(details["files"].as_array().unwrap().len(), 1);
    assert_eq!(details["files"][0]["id"], "b-shared");
    assert_eq!(details["files"][0]["status"], "ready");
    assert_eq!(
        std::fs::read(other_path).unwrap(),
        b"other account downloaded bytes"
    );
    assert!(details["provenance"].as_array().unwrap().is_empty());
    for id in ["exclusive", "shared"] {
        assert_eq!(
            std::fs::read(fixture.0.join(format!("{id}/Authored.unitypackage"))).unwrap(),
            b"keep downloaded bytes"
        );
    }
    assert_eq!(
        c.get("job", "history").unwrap().unwrap()["state"],
        "cancelled"
    );
    let manifest = c.manifest().unwrap();
    let other = Fixture::new();
    let peer = other.open();
    peer.restore_manifest(manifest).unwrap();
    assert!(peer.get("product", "exclusive").unwrap().is_none());
    assert!(peer.get("overlay", "exclusive").unwrap().is_none());
    assert!(peer.get("product", "shared").unwrap().is_some());
    assert_eq!(
        peer.get("collection", "manual").unwrap().unwrap()["productIds"],
        json!(["shared", "local:p"])
    );
}

#[test]
fn failed_login_discard_guards_success_history_and_active_jobs() {
    let fixture = Fixture::new();
    let c = fixture.open();
    for (id, marker) in [
        ("verified", json!({"identityVerified":true})),
        ("legacy", json!({"authenticatedAt":"2026-10-11T00:00:00Z"})),
        (
            "bound",
            json!({"identityHash":"authored-stable-binding","identityVerified":false}),
        ),
    ] {
        let mut account = marker;
        account["id"] = json!(id);
        c.put("account", id, account).unwrap();
        assert!(!c.discard_unverified_account(id).unwrap());
    }
    for kind in ["acquisition", "entitlement", "remote_file"] {
        c.put("account", kind, json!({"id":kind,"identityVerified":false}))
            .unwrap();
        c.put(kind, kind, json!({"id":kind,"accountId":kind}))
            .unwrap();
        assert!(!c.discard_unverified_account(kind).unwrap());
    }
    for state in ["queued", "running"] {
        c.put(
            "account",
            state,
            json!({"id":state,"identityVerified":false}),
        )
        .unwrap();
        c.put(
            "job",
            state,
            json!({"id":state,"accountId":state,"type":"boothLogin","state":state}),
        )
        .unwrap();
        assert!(!c.discard_unverified_account(state).unwrap());
        c.put(
            "job",
            state,
            json!({"id":state,"accountId":state,"type":"boothLogin","state":"cancelled"}),
        )
        .unwrap();
        assert!(c.discard_unverified_account(state).unwrap());
        assert_eq!(c.get("job", state).unwrap().unwrap()["state"], "cancelled");
    }
    c.put(
        "account",
        "draft",
        json!({"id":"draft","identityVerified":false,"authenticatedAt":null}),
    )
    .unwrap();
    c.put("job", "failed", json!({"id":"failed","accountId":"draft","type":"boothLogin","state":"failed","errorCode":"AUTH_REQUIRED"})).unwrap();
    assert!(c.discard_unverified_account("draft").unwrap());
    assert_eq!(
        c.get("job", "failed").unwrap().unwrap()["errorCode"],
        "AUTH_REQUIRED"
    );
    assert!(!c.discard_unverified_account("draft").unwrap());
}

#[test]
fn current_summary_schema_reopens_without_reprojection_or_history_changes() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "1");
    let expected = dynamic_details(&c, "1");
    let history = Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap();
    let revisions = c.change_domains().unwrap();
    drop(c);
    let reopened = fixture.open();
    let result = reopened
        .command("query", json!({"includeFiles":false}))
        .unwrap();
    assert_eq!(result["products"][0]["id"], "1");
    assert_eq!(result["products"][0]["title"], expected["title"]);
    assert_eq!(result["products"][0]["filesLoaded"], false);
    assert_eq!(dynamic_details(&reopened, "1"), expected);
    assert_eq!(reopened.change_domains().unwrap(), revisions);
    assert_eq!(
        Catalogue::document_snapshot(&reopened.inner.lock().unwrap()).unwrap(),
        history
    );
}

#[test]
fn incompatible_schema_is_rejected_without_altering_records() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "a");
    c.inner.lock().unwrap().execute_batch("DROP TRIGGER products_changed_update; DROP INDEX products_avatar_evidence; ALTER TABLE products DROP COLUMN summary;").unwrap();
    drop(c);
    assert!(Catalogue::open(&fixture.0).is_err());
    let conn = Connection::open(fixture.0.join("catalogue.sqlite3")).unwrap();
    assert!(conn.prepare("SELECT summary FROM products").is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM products", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn sqlite_commands_and_bootstrap_do_not_load_sync_history() {
    let f = Fixture::new();
    let c = f.open();
    c.inner
        .lock()
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES('automerge',?1)",
            [b"synthetic invalid history".as_slice()],
        )
        .unwrap();
    let _preparation = c.document_preparation.lock().unwrap();
    product(&c, "local");
    c.command(
        "collection",
        json!({"id":"group","name":"Local collection"}),
    )
    .unwrap();
    c.command("preferences", json!({"patch":{"locale":"ja"}}))
        .unwrap();
    c.put("job","progress",json!({"id":"progress","productId":"local","type":"download","state":"running","progress":25})).unwrap();
    assert!(c
        .command("bootstrap", json!({"includeProducts":false}))
        .is_ok());
    assert!(c
        .command("conflicts", json!({}))
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert!(c.document_cache.lock().unwrap().is_none());
    let conn = c.inner.lock().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM am_changes", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn outbox_restart_replays_order_and_ack_is_atomic_with_delta() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.command("overlay", json!({"ids":["1"],"patch":{"note":"first"}}))
        .unwrap();
    c.command("overlay", json!({"ids":["1"],"patch":{"note":"second"}}))
        .unwrap();
    drop(c);
    let c = f.open();
    c.inner.lock().unwrap().execute_batch("CREATE TRIGGER synthetic_fail_delta BEFORE INSERT ON am_changes BEGIN SELECT RAISE(FAIL,'synthetic transaction failure'); END;").unwrap();
    assert!(c.flush_sync_outbox().is_err());
    {
        let conn = c.inner.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM am_changes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        conn.execute_batch("DROP TRIGGER synthetic_fail_delta;")
            .unwrap();
    }
    c.flush_sync_outbox().unwrap();
    assert_eq!(
        sync::entities(c.document_cache.lock().unwrap().as_ref().unwrap())
            .unwrap()
            .into_iter()
            .find(|(kind, id, _)| kind == "overlay" && id == "1")
            .unwrap()
            .2["note"],
        "second"
    );
    assert_eq!(
        c.inner
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let count = c
        .inner
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM am_changes", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    c.flush_sync_outbox().unwrap();
    assert_eq!(
        c.inner
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM am_changes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
    c.command("overlay", json!({"ids":["1"],"patch":{"note":"third"}}))
        .unwrap();
    c.flush_sync_outbox().unwrap();
    assert_eq!(
        c.prepare_document(
            || panic!("warm history must not wait"),
            |_, _| panic!("warm history must not decode")
        )
        .unwrap(),
        (0, 0)
    );
}

#[test]
fn long_history_is_compacted_once_without_changing_the_document() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    for n in 0..=COMPACT_HISTORY_AFTER {
        c.command("overlay", json!({"ids":["1"],"patch":{"note":format!("note {n}")}}))
            .unwrap();
        c.flush_sync_outbox().unwrap();
    }
    let deltas = |c: &Catalogue| {
        c.inner
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM am_changes", [], |row| row.get::<_, usize>(0))
            .unwrap()
    };
    let document = |c: &Catalogue| sync::entities(c.document_cache.lock().unwrap().as_ref().unwrap()).unwrap();
    assert!(deltas(&c) > COMPACT_HISTORY_AFTER);
    let expected = document(&c);
    drop(c);
    let c = f.open();
    c.flush_sync_outbox().unwrap();
    assert_eq!(deltas(&c), 0);
    assert_eq!(document(&c), expected);
    c.command("overlay", json!({"ids":["1"],"patch":{"note":"after compaction"}}))
        .unwrap();
    c.flush_sync_outbox().unwrap();
    let expected = document(&c);
    drop(c);
    let c = f.open();
    c.flush_sync_outbox().unwrap();
    assert_eq!(deltas(&c), 1);
    assert_eq!(document(&c), expected);
}

#[test]
fn remote_merge_drains_concurrent_local_intent_before_projecting() {
    let folder = Fixture::new();
    let fa = Fixture::new();
    let fb = Fixture::new();
    let a = fa.open();
    let b = fb.open();
    product(&a, "1");
    a.command("overlay", json!({"ids":["1"],"patch":{"note":"base"}}))
        .unwrap();
    a.command("exchange", json!({"folder":folder.0})).unwrap();
    b.command("exchange", json!({"folder":folder.0})).unwrap();
    b.command("overlay", json!({"ids":["1"],"patch":{"note":"remote"}}))
        .unwrap();
    b.command("exchange", json!({"folder":folder.0})).unwrap();
    let path = folder.0.clone();
    a.on_sync_worker(move |catalogue| {
        catalogue.exchange_with_hook(&path, |local| {
            local.command(
                "overlay",
                json!({"ids":["1"],"patch":{"note":"concurrent local","title":"Local title"}}),
            )?;
            Ok(())
        })
    })
    .unwrap();
    assert_eq!(
        a.get("overlay", "1").unwrap().unwrap()["title"],
        "Local title"
    );
    let conflicts = a.command("conflicts", json!({})).unwrap();
    let note = conflicts
        .as_array()
        .unwrap()
        .iter()
        .find(|conflict| conflict["id"] == "1" && conflict["field"] == "note")
        .unwrap();
    let alternatives = note.to_string();
    assert!(alternatives.contains("concurrent local") && alternatives.contains("remote"));
}

#[test]
fn account_counts_include_hidden_products_and_deduplicate_observations() {
    let f = Fixture::new();
    let c = f.open();
    for id in ["a", "b", "zero"] {
        c.put("account", id, json!({"id":id,"store":"BOOTH"}))
            .unwrap();
    }
    for (id, category) in [
        ("hair", "Hair"),
        ("other", "NonVRChat"),
        ("unassigned", "NonVRChat"),
    ] {
        c.put(
            "product",
            id,
            json!({"id":id,"title":id,"category":category}),
        )
        .unwrap();
    }
    c.put(
        "product",
        "custom",
        json!({"id":"custom","title":"Custom","category":"NonVRChat","custom":true}),
    )
    .unwrap();
    for (id, product, account) in [
        ("a-one", "hair", "a"),
        ("a-two", "hair", "a"),
        ("a-other", "other", "a"),
        ("b-one", "hair", "b"),
    ] {
        c.put(
            "acquisition",
            id,
            json!({"id":id,"productId":product,"accountId":account,"source":"purchase"}),
        )
        .unwrap();
    }
    for id in ["file-one", "file-two"] {
        c.put(
            "remote_file",
            id,
            json!({"id":id,"productId":"hair","accountId":"a","name":id,"revision":"r1"}),
        )
        .unwrap();
    }
    let before = c.change_domains().unwrap();
    let expected = json!({"accounts":{"a":2,"b":1,"zero":0},"unassigned":1});
    assert_eq!(c.command("account_counts", json!({})).unwrap(), expected);
    let visible = c.command("query", json!({"includeFiles":false})).unwrap();
    assert!(!strings(&visible["matchedIds"]).contains(&"other".to_owned()));
    c.command(
        "query",
        json!({"category":"NonVRChat","search":"other","includeFiles":false}),
    )
    .unwrap();
    assert_eq!(c.command("account_counts", json!({})).unwrap(), expected);
    assert_eq!(c.change_domains().unwrap(), before);
}

#[test]
fn summary_status_progress_updates_leave_search_indexes_intact() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product","p",json!({"id":"p","title":"Synthetic casual Hair","shop":"Synthetic shop","sourceShop":"Synthetic source shop","store":"BOOTH","sourceKind":1,"sourceUrl":"https://booth.pm/ja/items/900001","sourceCategory":{"name":"Hair"},"sourceTags":["Mafuyu"],"sourceDescription":"large detail only","price":800,"publicVariants":[{"id":"v","name":"Owned","price":800,"currency":"JPY"}]})).unwrap();
    c.put("remote_file","file",json!({"id":"file","productId":"p","accountId":"a","name":"Needle_Manuka.unitypackage","revision":"r","variantId":"v"})).unwrap();
    c.put("entitlement","ent",json!({"id":"ent","productId":"p","accountId":"a","fileIds":["file"],"manifestComplete":true})).unwrap();
    c.put("acquisition","acq",json!({"id":"acq","productId":"p","accountId":"a","source":"purchase","acquiredDateText":"2026/10/01","paidPrice":500,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal"})).unwrap();
    c.command("overlay", json!({"ids":["p"],"patch":{"title":"Edited Synthetic Hair","tags":["Synthetic List Tag","Mafuyu"],"rejectedTags":["Synthetic Rejected Tag"],"note":"Keep this editor baseline"}})).unwrap();
    // A write to either index now fails deterministically, proving local status
    // updates do not rebuild filename/tag indexes.
    c.inner.lock().unwrap().execute_batch("CREATE TRIGGER synthetic_preserve_grams BEFORE DELETE ON search_grams BEGIN SELECT RAISE(FAIL,'index changed'); END;").unwrap();
    c.put(
        "job",
        "j",
        json!({"id":"j","productId":"p","type":"download","state":"running","progress":1}),
    )
    .unwrap();
    let before = c.change_domains().unwrap();
    c.put("job","j",json!({"id":"j","productId":"p","type":"download","state":"running","progress":99,"updatedAt":"later"})).unwrap();
    let after = c.change_domains().unwrap();
    assert!(after.0 > before.0);
    assert_eq!(after.1, before.1);
    c.put("artifact","artifact",json!({"id":"artifact","productId":"p","fileId":"file","revision":"r","status":"ready","originalAvailable":true,"extractedInventory":[{"path":"Detail only"}]})).unwrap();
    c.put(
        "job",
        "j",
        json!({"id":"j","productId":"p","type":"download","state":"completed"}),
    )
    .unwrap();
    let query = c
        .command(
            "query",
            json!({"search":"Needle","includeFiles":false,"accountIds":["a"]}),
        )
        .unwrap();
    let summary = &query["products"][0];
    assert_eq!(summary["status"], "ready");
    assert_eq!(summary["paidPrice"], 500.0);
    assert_eq!(summary["paidCurrency"], "JPY");
    assert_eq!(summary["price"], 800.0);
    assert_eq!(summary["acquired"], "2026/10/01");
    assert_eq!(summary["title"], "Edited Synthetic Hair");
    assert_eq!(summary["sourceTitle"], "Synthetic casual Hair");
    assert_eq!(summary["titleOverride"], true);
    assert_eq!(summary["sourceTags"], json!(["Mafuyu"]));
    assert_eq!(summary["rejectedTags"], json!(["Synthetic Rejected Tag"]));
    assert_eq!(summary["accountIds"], json!(["a"]));
    assert_eq!(summary["manifestComplete"], true);
    assert_eq!(summary["fullyDownloaded"], true);
    assert_eq!(summary["downloaded"], 1);
    assert_eq!(summary["total"], 1);
    assert!(!summary["tagSuggestions"].as_array().unwrap().is_empty());
    assert!(summary["tagSuggestions"][0]["basis"].is_string());
    let evidence = summary["ownedAvatarEvidence"].as_array().unwrap();
    assert!(evidence
        .iter()
        .any(|row| row["accountId"] == "a" && row["avatarId"].is_string()));
    assert!(evidence
        .iter()
        .any(|row| row["source"] == "sourceTag" && row["entitlementEstablished"] == false));
    assert!(summary["tags"]
        .as_array()
        .unwrap()
        .contains(&json!("Synthetic List Tag")));
    assert_eq!(summary["note"], "Keep this editor baseline");
    assert_eq!(summary["filesLoaded"], false);
    assert!(summary["files"].as_array().unwrap().is_empty());
    assert!(summary.get("sourceDescription").is_none());
    assert!(!summary["provenance"].as_array().unwrap().is_empty());
    let list = c
        .command(
            "query",
            json!({"search":"Needle","includeFiles":false,"presentation":"list","accountIds":["a"]}),
        )
        .unwrap();
    let mut expected_list = query.clone();
    for field in [
        "provenance",
        "categoryInference",
        "sources",
        "sourceShop",
        "sourceOriginUrl",
        "publicPageAvailability",
        "store",
        "sourceKind",
    ] {
        expected_list["products"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    expected_list["products"][0]["tagSuggestions"] = json!(summary["tagSuggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| json!({"label":row["label"]}))
        .collect::<Vec<_>>());
    expected_list["products"][0]["ownedAvatarEvidence"] = json!(evidence
        .iter()
        .map(|row| {
            let mut kept = json!({"avatar":row["avatar"],"source":row["source"]});
            if let Some(account) = row.get("accountId") {
                kept["accountId"] = account.clone();
            }
            kept
        })
        .collect::<Vec<_>>());
    // Preserve scoped paid/date values, matching IDs/counts, effective tags and
    // compatibility facts, and every other existing editor baseline.
    assert_eq!(list, expected_list);
    let detail = c
        .command("product_details", json!({"id":"p","accountIds":["a"]}))
        .unwrap();
    for field in [
        "tagSuggestions",
        "sourceShop",
        "sourceOriginUrl",
        "publicPageAvailability",
        "store",
        "sourceKind",
        "price",
        "paidPrice",
        "paidCurrency",
        "acquired",
        "tags",
        "ownedAvatars",
        "listingAvatars",
    ] {
        assert_eq!(detail[field], summary[field], "{field}");
    }
    assert!(detail["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["avatarId"].is_string()));
    let full = c
        .command(
            "query",
            json!({"search":"Needle","presentation":"list","accountIds":["a"]}),
        )
        .unwrap();
    assert_eq!(full["products"][0], detail);
    assert_eq!(
        c.command(
            "query",
            json!({"search":"Needle","includeFiles":false,"accountIds":["a"]})
        )
        .unwrap(),
        query
    );
    let avatar = evidence.iter().find(|row| row["accountId"] == "a").unwrap()["avatarId"].clone();
    let avatar_list = c
        .command(
            "query",
            json!({"avatar":avatar,"includeFiles":false,"presentation":"list","accountIds":["a"]}),
        )
        .unwrap();
    assert_eq!(avatar_list["matchedIds"], json!(["p"]));
    assert_eq!(avatar_list["products"], list["products"]);
    assert_eq!(
        detail["files"][0]["extractedInventory"][0]["path"],
        "Detail only"
    );
    assert_eq!(
        c.command("bootstrap", json!({"includeProducts":false}))
            .unwrap()["total"],
        1
    );
}

fn dynamic_details(c: &Catalogue, id: &str) -> Value {
    c.command("product_details", json!({"id":id})).unwrap()
}
fn dynamic_tag_ids(c: &Catalogue, id: &str) -> BTreeSet<String> {
    dynamic_details(c, id)["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| matches!(text(e, "source"), "sourceTag" | "userTag"))
        .map(|e| text(e, "avatarId").to_owned())
        .collect()
}
fn synthetic_named_base(c: &Catalogue, id: &str, name: &str) {
    c.put(
        "product",
        id,
        json!({"id":id,"sourceTitle":name,"sourceCategory":{"name":"3Dアバター"}}),
    )
    .unwrap();
}

#[test]
fn source_alias_history_survives_rename_without_reviving_removed_human_aliases() {
    let f = Fixture::new();
    let c = f.open();
    synthetic_named_base(&c, "z-base", "ソラネコ / Oldcat");
    for (id, tag) in [("old-tag", "Oldcat"), ("human-tag", "Humanalias")] {
        c.put("product", id, json!({"id":id,"title":"Synthetic hair","sourceCategory":{"name":"Hair"},"sourceTags":[tag]})).unwrap();
    }
    c.command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    synthetic_named_base(&c, "z-base", "ホシネコ / Newcat");
    assert_eq!(
        dynamic_tag_ids(&c, "old-tag"),
        BTreeSet::from(["product:z-base".into()])
    );
    c.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":["Humanalias"]}}}}),
    )
    .unwrap();
    assert_eq!(dynamic_tag_ids(&c, "human-tag").len(), 1);
    c.command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    c.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":[]}}}}),
    )
    .unwrap();
    assert!(dynamic_tag_ids(&c, "human-tag").is_empty());
    let history =
        c.get("preferences", "app").unwrap().unwrap()["avatarSourceAliases"]["z-base"].clone();
    assert!(strings(&history).contains(&"Oldcat".into()));
    assert!(strings(&history).contains(&"Newcat".into()));
    assert!(!strings(&history).contains(&"Humanalias".into()));
    c.command(
        "overlay",
        json!({"ids":["z-base"],"patch":{"title":"Human nickname"}}),
    )
    .unwrap();
    assert!(!strings(
        &c.get("preferences", "app").unwrap().unwrap()["avatarSourceAliases"]["z-base"]
    )
    .contains(&"Human nickname".into()));

    let restored = Fixture::new();
    let cr = restored.open();
    let manifest = c.manifest().unwrap();
    let saved_preferences = manifest["records"]["preferences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["id"] == "app")
        .unwrap();
    assert_eq!(saved_preferences["avatarSourceAliases"]["z-base"], history);
    cr.restore_manifest(manifest).unwrap();
    assert_eq!(
        cr.get("preferences", "app").unwrap().unwrap()["avatarSourceAliases"]["z-base"],
        history
    );
    assert_eq!(dynamic_tag_ids(&cr, "old-tag").len(), 1);
    assert!(dynamic_tag_ids(&cr, "human-tag").is_empty());
    drop(c);
    let c = f.open();
    assert_eq!(dynamic_tag_ids(&c, "old-tag").len(), 1);
    assert!(dynamic_tag_ids(&c, "human-tag").is_empty());

    synthetic_named_base(&c, "z-collision", "Oldcat");
    assert!(dynamic_tag_ids(&c, "old-tag").is_empty());
    c.delete("product", "z-collision").unwrap();
    assert_eq!(dynamic_tag_ids(&c, "old-tag").len(), 1);
    c.command(
        "overlay",
        json!({"ids":["z-base"],"patch":{"category":"Tool"}}),
    )
    .unwrap();
    assert!(dynamic_tag_ids(&c, "old-tag").is_empty());
    c.command(
        "overlay",
        json!({"ids":["z-base"],"patch":{},"clearFields":["category"]}),
    )
    .unwrap();
    assert_eq!(dynamic_tag_ids(&c, "old-tag").len(), 1);
    c.delete("product", "z-base").unwrap();
    assert!(dynamic_tag_ids(&c, "old-tag").is_empty());
    synthetic_named_base(&c, "different-base", "ホシネコ / Newcat");
    assert!(dynamic_tag_ids(&c, "old-tag").is_empty());
    c.delete("product", "different-base").unwrap();
    synthetic_named_base(&c, "z-base", "ホシネコ / Newcat");
    assert_eq!(dynamic_tag_ids(&c, "old-tag").len(), 1);
    assert!(dynamic_tag_ids(&c, "human-tag").is_empty());
}

#[test]
fn alias_collision_cannot_query_another_canonical_avatars_file_or_manual_evidence() {
    let f = Fixture::new();
    let c = f.open();
    synthetic_named_base(&c, "z-base", "Synthneko");
    for id in [
        "file-compatible",
        "manual-compatible",
        "lowercase-compatible",
    ] {
        product(&c, id);
    }
    c.put("remote_file", "f", json!({"id":"f","productId":"file-compatible","accountId":"a","name":"Mafuyu.unitypackage","revision":"r1"})).unwrap();
    c.put("entitlement", "e", json!({"id":"e","productId":"file-compatible","accountId":"a","fileIds":["f"],"manifestComplete":true})).unwrap();
    c.command(
        "overlay",
        json!({"ids":["manual-compatible"],"patch":{"ownedAvatars":["真冬"]}}),
    )
    .unwrap();
    c.command(
        "overlay",
        json!({"ids":["lowercase-compatible"],"patch":{"ownedAvatars":["mafuyu"]}}),
    )
    .unwrap();
    assert_eq!(
        dynamic_details(&c, "lowercase-compatible")["ownedAvatarEvidence"][0]["avatarId"],
        "booth:5007531"
    );
    c.command("preferences", json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":["Mafuyu","booth:5007531"]}}}})).unwrap();
    let wrong = c
        .command("query", json!({"avatar":"product:z-base"}))
        .unwrap();
    assert_eq!(strings(&wrong["matchedIds"]), vec!["z-base"]);
    let correct = c
        .command("query", json!({"avatar":"booth:5007531"}))
        .unwrap();
    assert_eq!(
        strings(&correct["matchedIds"])
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "file-compatible".into(),
            "manual-compatible".into(),
            "lowercase-compatible".into()
        ])
    );
}

#[test]
fn dynamic_base_tags_reconcile_creation_category_removal_collision_and_manual_precedence() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product", "a-tagged", json!({"id":"a-tagged","title":"Synthetic clothes","sourceCategory":{"name":"Clothes"},"sourceTags":["Synthneko対応"]})).unwrap();
    assert!(dynamic_tag_ids(&c, "a-tagged").is_empty());
    synthetic_named_base(&c, "z-base", "Synthneko");
    assert_eq!(
        dynamic_tag_ids(&c, "a-tagged"),
        BTreeSet::from(["product:z-base".into()])
    );
    c.command(
        "overlay",
        json!({"ids":["z-base"],"patch":{"category":"Tool"}}),
    )
    .unwrap();
    assert!(dynamic_tag_ids(&c, "a-tagged").is_empty());
    c.command(
        "overlay",
        json!({"ids":["z-base"],"patch":{},"clearFields":["category"]}),
    )
    .unwrap();
    assert_eq!(dynamic_tag_ids(&c, "a-tagged").len(), 1);
    synthetic_named_base(&c, "z-collision", "Synthneko");
    assert!(dynamic_tag_ids(&c, "a-tagged").is_empty());
    c.delete("product", "z-collision").unwrap();
    assert_eq!(dynamic_tag_ids(&c, "a-tagged").len(), 1);
    c.command(
        "overlay",
        json!({"ids":["a-tagged"],"patch":{"ownedAvatars":[]}}),
    )
    .unwrap();
    assert!(dynamic_details(&c, "a-tagged")["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .is_empty());
    c.command(
        "overlay",
        json!({"ids":["a-tagged"],"patch":{},"clearFields":["ownedAvatars"]}),
    )
    .unwrap();
    c.command("preferences", json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":["Astercat"],"englishName":"Astercat"}}}})).unwrap();
    c.command(
        "overlay",
        json!({"ids":["a-tagged"],"patch":{"rejectedTags":["Astercat"]}}),
    )
    .unwrap();
    assert!(dynamic_tag_ids(&c, "a-tagged").is_empty());
    c.command(
        "overlay",
        json!({"ids":["a-tagged"],"patch":{"tags":["Astercat"],"rejectedTags":[]}}),
    )
    .unwrap();
    assert_eq!(dynamic_tag_ids(&c, "a-tagged").len(), 1);
    assert_eq!(
        c.get("overlay", "a-tagged").unwrap().unwrap()["rejectedTags"],
        json!([])
    );
    c.command("overlay", json!({"ids":["a-tagged"],"patch":{"ownedAvatars":["Manualother"],"rejectedTags":["Astercat"]}})).unwrap();
    let p = dynamic_details(&c, "a-tagged");
    assert_eq!(p["ownedAvatars"], json!(["Manualother"]));
    assert!(p["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["source"] == "user"));
    c.command(
        "overlay",
        json!({"ids":["a-tagged"],"patch":{"rejectedTags":[]},"clearFields":["ownedAvatars"]}),
    )
    .unwrap();
    c.delete("product", "z-base").unwrap();
    assert!(dynamic_tag_ids(&c, "a-tagged").is_empty());
    assert!(c
        .get("product", "a-tagged")
        .unwrap()
        .unwrap()
        .get("_tagInputs")
        .is_none());
}

#[test]
fn dynamic_alias_preferences_restart_restore_exchange_and_account_scopes_stay_consistent() {
    let a = Fixture::new();
    let b = Fixture::new();
    let restored = Fixture::new();
    let folder = Fixture::new();
    let ca = a.open();
    ca.put("product", "a-tagged", json!({"id":"a-tagged","title":"Synthetic hair","sourceCategory":{"name":"Hair"},"sourceTags":["Humanalias"]})).unwrap();
    ca.put(
        "acquisition",
        "a-acquisition",
        json!({"id":"a-acquisition","productId":"a-tagged","accountId":"a"}),
    )
    .unwrap();
    synthetic_named_base(&ca, "z-base", "Synthneko");
    assert!(dynamic_tag_ids(&ca, "a-tagged").is_empty());
    ca.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":["Humanalias"]}}}}),
    )
    .unwrap();
    assert_eq!(dynamic_tag_ids(&ca, "a-tagged").len(), 1);
    ca.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":[]}}}}),
    )
    .unwrap();
    assert!(dynamic_tag_ids(&ca, "a-tagged").is_empty());
    ca.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"product:z-base":{"aliases":["Humanalias"]}}}}),
    )
    .unwrap();
    assert_eq!(dynamic_tag_ids(&ca, "a-tagged").len(), 1);
    for (accounts, expected) in [
        (Value::Null, true),
        (json!(["a"]), true),
        (json!(["b"]), false),
        (json!([]), false),
    ] {
        let query = ca
            .command(
                "query",
                json!({"avatar":"product:z-base","accountIds":accounts}),
            )
            .unwrap();
        assert_eq!(
            strings(&query["matchedIds"]).contains(&"a-tagged".to_owned()),
            expected
        );
    }
    // A source-backed English name works without relying on device-local prefs,
    // so portable restore and peer exchange must independently derive it.
    ca.put("product", "a-tagged", json!({"id":"a-tagged","title":"Synthetic hair","sourceCategory":{"name":"Hair"},"sourceTags":["Synthneko"]})).unwrap();
    let cr = restored.open();
    cr.restore_manifest(ca.manifest().unwrap()).unwrap();
    assert_eq!(dynamic_tag_ids(&cr, "a-tagged").len(), 1);
    let cb = b.open();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(dynamic_tag_ids(&cb, "a-tagged").len(), 1);
    drop(ca);
    let ca = a.open();
    assert_eq!(dynamic_tag_ids(&ca, "a-tagged").len(), 1);
    ca.command(
        "overlay",
        json!({"ids":["z-base"],"patch":{"category":"Tool"}}),
    )
    .unwrap();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    assert!(dynamic_tag_ids(&cb, "a-tagged").is_empty());
}

#[test]
fn materialized_details_track_root_account_and_job_changes_without_losing_dynamic_tags() {
    let f = Fixture::new();
    let c = f.open();
    synthetic_named_base(&c, "base", "Synthneko");
    c.put("product", "p", json!({"id":"p","title":"Synthetic hair","sourceCategory":{"name":"Hair"},"sourceTags":["Synthneko"]})).unwrap();
    c.put(
        "account",
        "a",
        json!({"id":"a","name":"Before","store":"BOOTH"}),
    )
    .unwrap();
    c.put(
        "root",
        "r",
        json!({"id":"r","path":"X:/synthetic","status":"online"}),
    )
    .unwrap();
    c.put("remote_file", "f", json!({"id":"f","productId":"p","accountId":"a","name":"Synthetic.unitypackage","revision":"r1"})).unwrap();
    c.put(
        "entitlement",
        "e",
        json!({"id":"e","productId":"p","accountId":"a","fileIds":["f"],"manifestComplete":true}),
    )
    .unwrap();
    c.put("artifact", "f", json!({"id":"f","productId":"p","fileId":"f","revision":"r1","rootId":"r","relativePath":"synthetic/file.unitypackage","status":"ready"})).unwrap();
    c.put(
        "account",
        "a",
        json!({"id":"a","name":"After","store":"BOOTH"}),
    )
    .unwrap();
    c.put(
        "root",
        "r",
        json!({"id":"r","path":"X:/synthetic","status":"offline"}),
    )
    .unwrap();
    let p = dynamic_details(&c, "p");
    assert_eq!(p["sources"][0]["accountName"], "After");
    assert_eq!(p["files"][0]["rootStatus"], "offline");
    assert_eq!(dynamic_tag_ids(&c, "p").len(), 1);
    c.put("job", "j", json!({"id":"j","type":"download","productId":"p","state":"failed","errorCode":"synthetic_failure","updatedAt":"2026-10-09T01:00:00Z"})).unwrap();
    // A stale job failure cannot override all-current verified files.
    assert_eq!(dynamic_details(&c, "p")["status"], "ready");
    let mut artifact = c.get("artifact", "f").unwrap().unwrap();
    artifact["status"] = json!("missing");
    c.put("artifact", "f", artifact.clone()).unwrap();
    assert_eq!(dynamic_details(&c, "p")["status"], "attention");
    assert_eq!(dynamic_tag_ids(&c, "p").len(), 1);
    artifact["status"] = json!("ready");
    c.put("artifact", "f", artifact).unwrap();
    c.delete("job", "j").unwrap();
    assert_eq!(dynamic_details(&c, "p")["status"], "ready");
    assert_eq!(dynamic_tag_ids(&c, "p").len(), 1);
    c.delete("remote_file", "f").unwrap();
    assert_ne!(dynamic_details(&c, "p")["status"], "ready");
    assert_eq!(dynamic_tag_ids(&c, "p").len(), 1);
}

fn seed_current_product_url(c: &Catalogue, id: &str) -> Value {
    let value = json!({"id":id,"store":"BOOTH","title":"Synthetic current source","sourceUrl":format!("https://booth.pm/ja/items/{id}"),"observedAt":"2026-10-09T00:00:00Z","sourceDescription":format!("Preserve literal https://booth.pm/items/{id} in prose")});
    c.mutate(|tx, projections| {
        let before = raw(tx, "product", id)?;
        Catalogue::enqueue_sync(tx, "product", id, before.as_ref(), Some(&value))?;
        set_raw(tx, "product", id, &value)?;
        projections.merge(projection::refresh(tx, id)?);
        Ok(())
    })
    .unwrap();
    value
}

#[test]
fn current_booth_url_restore_and_peer_exchange_preserve_observations() {
    let a = Fixture::new();
    let b = Fixture::new();
    let restored = Fixture::new();
    let folder = Fixture::new();
    let ca = a.open();
    let cb = b.open();
    let source = seed_current_product_url(&ca, "900002");
    let acquisition = json!({"id":"synthetic-acquisition","productId":"900002","accountId":"synthetic","sourceRecordId":"stable-row"});
    ca.put("acquisition", "synthetic-acquisition", acquisition.clone())
        .unwrap();
    let old_manifest = ca.manifest().unwrap();
    let cr = restored.open();
    cr.restore_manifest(old_manifest).unwrap();
    assert_eq!(
        cr.get("product", "900002").unwrap().unwrap()["sourceUrl"],
        "https://booth.pm/ja/items/900002"
    );
    assert_eq!(
        cr.get("acquisition", "synthetic-acquisition").unwrap(),
        Some(acquisition.clone())
    );

    // Current-format restore and exchange preserve the exact source observation.
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(
        cb.get("product", "900002").unwrap().unwrap()["sourceUrl"],
        "https://booth.pm/ja/items/900002"
    );
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(
        ca.get("product", "900002").unwrap(),
        cb.get("product", "900002").unwrap()
    );
    let mut expected = source;
    expected["sourceUrl"] = json!("https://booth.pm/ja/items/900002");
    assert_eq!(ca.get("product", "900002").unwrap(), Some(expected));
    assert_eq!(
        cb.get("acquisition", "synthetic-acquisition").unwrap(),
        Some(acquisition)
    );
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    let revision = cb.revision().unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(cb.revision().unwrap(), revision);
}

#[test]
fn exact_avatar_tags_remain_global_while_product_and_file_accounts_stay_scoped() {
    let f = Fixture::new();
    let c = f.open();
    for id in ["source-tag", "user-tag", "file-evidence"] {
        c.put("product", id, json!({"id":id,"title":"Synthetic clothes","sourceCategory":{"name":"Clothes"},"sourceTags":if id == "source-tag" { vec!["ミーシェ"] } else { vec![] }})).unwrap();
        let acquisition = format!("a:{id}");
        c.put(
            "acquisition",
            &acquisition,
            json!({"id":acquisition,"productId":id,"accountId":"a"}),
        )
        .unwrap();
    }
    c.command(
        "overlay",
        json!({"ids":["user-tag"],"patch":{"tags":["Mishe"]}}),
    )
    .unwrap();
    c.put("remote_file", "file-b", json!({"id":"file-b","productId":"file-evidence","accountId":"b","name":"Mishe Synthetic.unitypackage","revision":"synthetic-r1"})).unwrap();
    c.put("entitlement", "entitlement-b", json!({"id":"entitlement-b","productId":"file-evidence","accountId":"b","fileIds":["file-b"],"manifestComplete":true})).unwrap();
    for (accounts, expected) in [
        (Value::Null, vec!["file-evidence", "source-tag", "user-tag"]),
        (json!(["a"]), vec!["source-tag", "user-tag"]),
        (json!(["b"]), vec!["file-evidence"]),
        (json!([]), vec![]),
    ] {
        let result = c
            .command(
                "query",
                json!({"avatar":"booth:1256087","accountIds":accounts}),
            )
            .unwrap();
        let actual: BTreeSet<_> = strings(&result["matchedIds"]).into_iter().collect();
        assert_eq!(
            actual,
            expected.into_iter().map(str::to_owned).collect(),
            "accounts={accounts}"
        );
    }
}

#[test]
fn newly_curated_mishe_keeps_exact_product_settings_and_bootstrap_is_idempotent() {
    let f = Fixture::new();
    let c = f.open();
    c.put(
        "product",
        "1256087",
        json!({"id":"1256087","title":"ミーシェ","sourceCategory":{"name":"3Dアバター"}}),
    )
    .unwrap();
    c.put("preferences", "app", json!({"id":"app","avatarSettings":{"booth:1256087":{"enabled":false,"englishName":"Mishe"}}})).unwrap();
    let first = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let entry = first["avatarOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "booth:1256087")
        .unwrap();
    assert_eq!(entry["enabled"], false);
    assert_eq!(entry["baseProductIds"], json!(["1256087"]));
    assert!(first["preferences"]["avatarSettings"]
        .get("product:1256087")
        .is_none());
    assert_eq!(
        first["preferences"]["avatarSettings"]["booth:1256087"]["enabled"],
        false
    );
    let revision = c.revision().unwrap();
    let second = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    assert_eq!(first["avatarOptions"], second["avatarOptions"]);
    assert_eq!(first["preferences"], second["preferences"]);
    assert_eq!(c.revision().unwrap(), revision);
}

#[test]
fn fetched_thumbnail_baseline_survives_custom_reset_sync_and_restart() {
    let f = Fixture::new();
    let c = f.open();
    let original = json!({"id":"synthetic-thumb","title":"Source title","previewUrl":"https://booth.pximg.net/synthetic-original.png","sourceUrl":"https://example.org/original"});
    c.put("product", "synthetic-thumb", original.clone())
        .unwrap();
    let fetched = "https://booth.pximg.net/synthetic-fetched.png";
    c.command("overlay", json!({"ids":["synthetic-thumb"],"patch":{"sourceUrl":"https://example.org/edited","sourceThumbnailUrl":fetched,"title":"Fetched title"}})).unwrap();
    c.command(
        "overlay",
        json!({"ids":["synthetic-thumb"],"patch":{"thumbnailPath":"media/synthetic-custom.png"}}),
    )
    .unwrap();
    let p = projection(&c, "synthetic-thumb");
    assert_eq!(p["sourcePreviewUrl"], fetched);
    assert_ne!(p["previewUrl"], fetched);
    c.command("overlay", json!({"ids":["synthetic-thumb"],"clearFields":["thumbnailPath","thumbnailHidden"],"patch":{}})).unwrap();
    assert_eq!(projection(&c, "synthetic-thumb")["previewUrl"], fetched);
    // Source refresh preserves the current fetched baseline and raw provenance.
    c.put("product", "synthetic-thumb", original.clone())
        .unwrap();
    assert_eq!(c.get("product", "synthetic-thumb").unwrap(), Some(original));
    assert_eq!(projection(&c, "synthetic-thumb")["previewUrl"], fetched);
    let saved = c.manifest().unwrap();
    let other = Fixture::new();
    let peer = other.open();
    peer.restore_manifest(saved).unwrap();
    assert_eq!(
        projection(&peer, "synthetic-thumb")["sourcePreviewUrl"],
        fetched
    );
    drop(c);
    let c = f.open();
    assert_eq!(projection(&c, "synthetic-thumb")["previewUrl"], fetched);
    c.command(
        "overlay",
        json!({"ids":["synthetic-thumb"],"patch":{"sourceThumbnailUrl":""}}),
    )
    .unwrap();
    assert_eq!(projection(&c, "synthetic-thumb")["sourcePreviewUrl"], "");
    assert!(projection(&c, "synthetic-thumb")["previewUrl"]
        .as_str()
        .unwrap_or("")
        .is_empty());
    for invalid in [
        "http://booth.pximg.net/a",
        "https://example.org/a",
        "https://booth.pximg.net/a?token=secret",
        "https://booth.pximg.net/a#fragment",
        "https://u:p@booth.pximg.net/a",
        "https://booth.pximg.net:443/a",
    ] {
        assert!(c
            .command(
                "overlay",
                json!({"ids":["synthetic-thumb"],"patch":{"sourceThumbnailUrl":invalid}})
            )
            .is_err());
        assert_eq!(projection(&c, "synthetic-thumb")["sourcePreviewUrl"], "");
    }
}

#[test]
fn collection_results_are_atomic_and_revisions_preserve_broad_changes() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "synthetic-member");
    let before = c.change_revisions().unwrap();
    assert_eq!(before.0, before.1);
    // Exercise upgrade fallback, including the first collection-only mutation.
    c.inner
        .lock()
        .unwrap()
        .execute("DELETE FROM meta WHERE key='noncollection_revision'", [])
        .unwrap();
    assert_eq!(c.change_revisions().unwrap(), before);
    let added = c.command("collection", json!({"op":"create","id":"synthetic-list","name":"Synthetic list","productIds":["synthetic-member"]})).unwrap();
    assert_eq!(
        added["collections"][0]["productIds"],
        json!(["synthetic-member"])
    );
    assert!(c
        .get("collection", "synthetic-list")
        .unwrap()
        .unwrap()
        .get("collections")
        .is_none());
    let after = c.change_revisions().unwrap();
    assert!(after.0 > before.0);
    assert_eq!(after.1, before.0);
    let dedup = c
        .command(
            "collection",
            json!({"op":"add","collectionIds":["synthetic-list"],"ids":["synthetic-member"]}),
        )
        .unwrap();
    assert_eq!(dedup["collections"], added["collections"]);
    assert_eq!(c.change_revisions().unwrap(), after);
    product(&c, "synthetic-second");
    let source = c.change_revisions().unwrap();
    assert!(source.0 > after.0);
    assert_eq!(source.0, source.1);
    let removed = c
        .command("collection", json!({"op":"delete","id":"synthetic-list"}))
        .unwrap();
    assert_eq!(removed["collections"], json!([]));
    let deleted = c.change_revisions().unwrap();
    assert!(deleted.0 > source.0);
    assert_eq!(deleted.1, source.1);
    drop(c);
    assert_eq!(f.open().change_revisions().unwrap(), deleted);
}

#[test]
fn lightweight_collections_reads_memberships_without_normalizing_avatar_preferences() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "synthetic-member");
    let before = c.get("preferences", "app").unwrap();
    c.command("collection", json!({"operation":"add","names":["Synthetic list","Synthetic list"],"ids":["synthetic-member"]})).unwrap();
    let revision = c.revision().unwrap();
    let result = c.command("collections", json!({})).unwrap();
    let rows = result["collections"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["productIds"], json!(["synthetic-member"]));
    assert_eq!(c.revision().unwrap(), revision);
    assert_eq!(c.get("preferences", "app").unwrap(), before);
}

#[test]
#[ignore = "Lead-owned release-only synthetic avatar responsiveness probe"]
fn synthetic_260_base_bootstrap_and_collection_probe() {
    use std::time::Instant;
    let f = Fixture::new();
    let c = f.open();
    let mut records = Vec::new();
    for n in 0..260 {
        let name = format!("Synthcat{n:03}");
        let base = format!("synthetic-base-{n:03}");
        records.push(("product".into(), base.clone(), json!({"id":base,"title":name,"sourceCategory":{"name":"3Dアバター"},"shop":"Synthetic creator"})));
        for item in 0..3 {
            let id = format!("synthetic-accessory-{n:03}-{item}");
            records.push((
                "product".into(),
                id.clone(),
                json!({"id":id,"title":"Synthetic hair","sourceCategory":{"name":"Hair"}}),
            ));
            records.push(("overlay".into(), id.clone(), json!({"id":id,"tags":[name]})));
            for file in 0..2 {
                let fid = format!("{id}:file:{file}");
                records.push(("remote_file".into(), fid.clone(), json!({"id":fid,"productId":id,"name":format!("Synthetic_{file}.unitypackage"),"revision":"synthetic-v1"})));
            }
        }
    }
    c.put_many(records).unwrap();
    let started = Instant::now();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let first = started.elapsed().as_micros();
    let options = boot["avatarOptions"].as_array().unwrap();
    assert_eq!(
        options
            .iter()
            .filter(|entry| text(entry, "id").starts_with("product:"))
            .count(),
        260
    );
    assert!(options
        .iter()
        .filter(|entry| text(entry, "id").starts_with("product:"))
        .all(|entry| entry["matchingProductCount"] == 4));
    let stable = c.get("preferences", "app").unwrap();
    let mut bootstrap_us = Vec::new();
    let mut query_us = Vec::new();
    let mut bytes = 0;
    for _ in 0..5 {
        let started = Instant::now();
        let result = c
            .command("bootstrap", json!({"includeProducts":false}))
            .unwrap();
        bytes = serde_json::to_vec(&result).unwrap().len();
        bootstrap_us.push(started.elapsed().as_micros());
        let started = Instant::now();
        let result = c
            .command(
                "query",
                json!({"quick":"catalogue","includeFiles":false,"limit":10000}),
            )
            .unwrap();
        assert_eq!(result["products"].as_array().unwrap().len(), 1040);
        let _ = serde_json::to_vec(&result).unwrap();
        query_us.push(started.elapsed().as_micros());
    }
    assert_eq!(c.get("preferences", "app").unwrap(), stable);
    let started = Instant::now();
    c.command(
        "collection",
        json!({"operation":"add","names":["Synthetic list"],"ids":["synthetic-base-000"]}),
    )
    .unwrap();
    let collection_us = started.elapsed().as_micros();
    let started = Instant::now();
    assert_eq!(
        c.command("collections", json!({})).unwrap()["collections"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let collection_read_us = started.elapsed().as_micros();
    let started = Instant::now();
    c.put("job", "synthetic-probe-job", json!({"id":"synthetic-probe-job","productId":"synthetic-accessory-000-0","type":"download","state":"cancelled"})).unwrap();
    let job_refresh_us = started.elapsed().as_micros();
    assert_eq!(dynamic_tag_ids(&c, "synthetic-accessory-000-0").len(), 1);
    let started = Instant::now();
    synthetic_named_base(&c, "synthetic-base-000", "Renamedsynthcat");
    let base_identity_change_us = started.elapsed().as_micros();
    assert_eq!(dynamic_tag_ids(&c, "synthetic-accessory-000-0").len(), 1);
    c.command(
        "overlay",
        json!({"ids":["synthetic-base-000"],"patch":{"category":"Tool"}}),
    )
    .unwrap();
    assert!(dynamic_tag_ids(&c, "synthetic-accessory-000-0").is_empty());
    println!("synthetic_base_count=260 synthetic_product_count=1040 synthetic_file_count=1560 bootstrap_first_us={first} bootstrap_warm_us={bootstrap_us:?} query_10000_us={query_us:?} bootstrap_serialized_bytes={bytes} collection_write_us={collection_us} collection_read_us={collection_read_us} job_refresh_us={job_refresh_us} base_identity_change_us={base_identity_change_us}");
}

#[test]
fn saved_human_base_name_overrides_ambiguous_source_before_registry_rejection() {
    let f = Fixture::new();
    let c = f.open();
    let source = json!({"id":"synthetic-base","sourceTitle":"星猫 / 海猫","sourceCategory":{"name":"3Dアバター"},"shop":"Synthetic creator"});
    c.put("product", "synthetic-base", source.clone()).unwrap();
    c.command(
        "overlay",
        json!({"ids":["synthetic-base"],"patch":{"note":"Retained human note"}}),
    )
    .unwrap();
    let overlay = c.get("overlay", "synthetic-base").unwrap();
    c.command("preferences",json!({"patch":{"avatarSettings":{"product:synthetic-base":{"originalName":"星空猫","englishName":"Hoshizoraneko","enabled":true}}}})).unwrap();
    for pass in 0..2 {
        let boot = c
            .command("bootstrap", json!({"includeProducts":false}))
            .unwrap();
        let options: Vec<_> = boot["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["id"] == "product:synthetic-base")
            .collect();
        assert_eq!(options.len(), 1, "{pass}");
        assert_eq!(options[0]["displayName"], "星空猫 / Hoshizoraneko");
        assert_eq!(options[0]["enabled"], true);
        assert_eq!(options[0]["baseProductIds"], json!(["synthetic-base"]));
    }
    drop(c);
    let c = f.open();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let a = boot["avatarOptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "product:synthetic-base")
        .unwrap();
    assert_eq!(a["displayName"], "星空猫 / Hoshizoraneko");
    assert_eq!(a["enabled"], true);
    assert_eq!(
        boot["preferences"]["avatarSettings"]["product:synthetic-base"]["enabled"],
        true
    );
    assert_eq!(c.get("product", "synthetic-base").unwrap(), Some(source));
    assert_eq!(c.get("overlay", "synthetic-base").unwrap(), overlay);
    assert_eq!(
        c.command("product_details", json!({"id":"synthetic-base"}))
            .unwrap()["category"],
        "Base"
    );
}

#[test]
fn ambiguous_slash_bases_remain_selectable_without_fabricated_individual_identities() {
    let f = Fixture::new();
    let c = f.open();
    let sources = [
        (
            "named",
            "VRChat対応 / オリジナル3Dモデル「星空猫」 / VRChat",
        ),
        ("multiple", "星猫 / 海猫"),
        ("platform", "VRChat / 3D Avatar"),
    ];
    for (id, title) in sources {
        c.put(
            "product",
            id,
            json!({"id":id,"sourceTitle":title,"sourceCategory":{"name":"3Dアバター"}}),
        )
        .unwrap();
    }
    product(&c, "manual");
    c.command(
        "overlay",
        json!({"ids":["manual"],"patch":{"ownedAvatars":["星猫 / 海猫"]}}),
    )
    .unwrap();
    let boot = c.command("bootstrap", json!({})).unwrap();
    let options = boot["avatarOptions"].as_array().unwrap();
    let named = options.iter().find(|a| a["id"] == "product:named").unwrap();
    assert_eq!(named["officialName"], "星空猫");
    #[cfg(target_os = "windows")]
    assert_eq!(named["displayName"], "星空猫 / Hoshizoraneko");
    for id in ["product:multiple", "product:platform"] {
        let fallback = options.iter().find(|a| a["id"] == id).unwrap();
        assert_eq!(fallback["nameKind"], "fallback");
        assert_eq!(fallback["englishName"], "");
    }
    assert!(!options.iter().any(|a| text(a, "id").starts_with("avatar:")));
    assert_eq!(boot["products"].as_array().unwrap().len(), 4);
    for (id, title) in sources {
        assert_eq!(c.get("product", id).unwrap().unwrap()["sourceTitle"], title);
    }
    assert_eq!(
        c.get("overlay", "manual").unwrap().unwrap()["ownedAvatars"],
        json!(["星猫 / 海猫"])
    );
}

#[test]
fn avatar_snapshot_is_pure_and_manual_base_visibility_does_not_change_defaults() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product", "manual-base", json!({"id":"manual-base","sourceTitle":"VRChat / 3D Avatar","sourceCategory":{"name":"Clothes"}})).unwrap();
    c.put("job", "running-store", json!({"id":"running-store","type":"boothSync","state":"running"})).unwrap();
    c.command("overlay", json!({"ids":["manual-base"],"patch":{"category":"Base"}})).unwrap();
    let preferences = c.get("preferences", "app").unwrap();
    let revision = c.revision().unwrap();
    let snapshot = c.command("avatar_options", json!({})).unwrap();
    let option = snapshot["avatarOptions"].as_array().unwrap().iter()
        .find(|option| option["id"] == "product:manual-base").unwrap();
    assert_eq!(option["nameKind"], "fallback");
    assert_eq!(option["enabled"], false);
    assert_eq!(option["baseProductIds"], json!(["manual-base"]));
    assert_eq!(c.get("preferences", "app").unwrap(), preferences);
    assert_eq!(c.revision().unwrap(), revision);
    let bootstrap = c.command("bootstrap", json!({"includeProducts":false})).unwrap();
    assert_eq!(snapshot["avatarOptions"], bootstrap["avatarOptions"]);
    product(&c, "selected-fallback");
    c.command("overlay", json!({"ids":["selected-fallback"],"patch":{"ownedAvatars":["product:manual-base"]}})).unwrap();
    let selected = dynamic_details(&c, "selected-fallback");
    assert_eq!(selected["ownedAvatarEvidence"], json!([{"avatarId":"product:manual-base","avatar":option["displayName"],"source":"user"}]));
    c.put("product", "generic-tag", json!({"id":"generic-tag","sourceTitle":"Authored clothing","sourceCategory":{"name":"Clothes"},"sourceTags":["VRChat / 3D Avatar"]})).unwrap();
    assert!(dynamic_tag_ids(&c, "generic-tag").is_empty());
    assert_eq!(c.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"], preferences.unwrap()["avatarAutomaticEnabled"]);
}

#[test]
fn avatar_presentation_overrides_survive_source_refresh_without_becoming_tag_evidence() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product", "named-base", json!({"id":"named-base","sourceTitle":"シロネコ","shop":"Authored source shop","sourceCategory":{"name":"3Dアバター"}})).unwrap();
    for (id, tag) in [("source-tag", "Shironeko"), ("presentation-tag", "Custom display")] {
        c.put("product", id, json!({"id":id,"sourceTitle":"Authored clothing","sourceCategory":{"name":"Clothes"},"sourceTags":[tag]})).unwrap();
    }
    c.command("preferences", json!({"patch":{"avatarSettings":{"product:named-base":{"displayName":"Custom display","shopName":"Custom group","enabled":false}},"avatarAliases":{"custom display":"product:named-base"}}})).unwrap();
    product(&c, "legacy-display");
    c.command("overlay", json!({"ids":["legacy-display"],"patch":{"ownedAvatars":["Custom display"]}})).unwrap();
    let option = |catalogue: &Catalogue| catalogue.command("avatar_options", json!({})).unwrap()["avatarOptions"]
        .as_array().unwrap().iter().find(|entry| entry["id"] == "product:named-base").unwrap().clone();
    let first = option(&c);
    assert_eq!(first["displayName"], "Custom display");
    assert_eq!(first["shopName"], "Custom group");
    assert_eq!(first["sourceDisplayName"], "シロネコ / Shironeko");
    assert_eq!(first["sourceShopName"], "Authored source shop");
    assert_eq!(first["matchingProductCount"], 2);
    assert!(!strings(&first["aliases"]).contains(&"Custom display".into()));
    assert_eq!(dynamic_tag_ids(&c, "source-tag"), BTreeSet::from(["product:named-base".into()]));
    assert!(dynamic_tag_ids(&c, "presentation-tag").is_empty());
    c.put("product", "named-base", json!({"id":"named-base","sourceTitle":"クロネコ","shop":"Refreshed source shop","sourceCategory":{"name":"3Dアバター"}})).unwrap();
    assert_eq!(option(&c)["displayName"], "Custom display");
    assert_eq!(option(&c)["sourceDisplayName"], "クロネコ / Kuroneko");
    assert_eq!(option(&c)["sourceShopName"], "Refreshed source shop");
    drop(c);
    let c = f.open();
    assert_eq!(option(&c)["displayName"], "Custom display");
    assert_eq!(option(&c)["shopName"], "Custom group");
    c.command("preferences", json!({"patch":{"avatarSettings":{"product:named-base":{"displayName":"  ","shopName":"","enabled":false}}}})).unwrap();
    assert_eq!(option(&c)["displayName"], "クロネコ / Kuroneko");
    assert_eq!(option(&c)["shopName"], "Refreshed source shop");
    for invalid in [json!({"displayName":true}), json!({"shopName":[]}), json!({"enabled":"false"})] {
        let before = c.get("preferences", "app").unwrap();
        assert!(c.command("preferences", json!({"patch":{"avatarSettings":{"product:named-base":invalid}}})).is_err());
        assert_eq!(c.get("preferences", "app").unwrap(), before);
    }
}

#[test]
fn avatar_delta_uses_current_exact_evidence_preserves_legacy_and_undoes_atomically() {
    let f = Fixture::new();
    let c = f.open();
    for id in ["base-a", "base-b"] { synthetic_named_base(&c, id, "Sharedcat"); }
    product(&c, "edited");
    // Represents a newer edit committed after the popup captured its draft.
    c.command("overlay", json!({"ids":["edited"],"patch":{"ownedAvatars":["product:base-a","product:base-b","  Legacy label  "]}})).unwrap();
    let before = c.get("overlay", "edited").unwrap();
    let result = c.command("overlay", json!({"changes":[{"id":"edited","patch":{"note":"Changed together"},"ownedAvatarsDelta":{"add":["Mafuyu","booth:5007531"],"remove":["product:base-a"]}}]})).unwrap();
    let overlay = c.get("overlay", "edited").unwrap().unwrap();
    assert_eq!(strings(&overlay["ownedAvatars"]).into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from(["product:base-b".into(), "  Legacy label  ".into(), "Mafuyu".into()]));
    let evidence: BTreeSet<_> = dynamic_details(&c, "edited")["ownedAvatarEvidence"].as_array().unwrap().iter()
        .map(|entry| text(entry, "avatarId").to_owned()).collect();
    assert!(evidence.contains("product:base-b"));
    assert!(!evidence.contains("product:base-a"));
    assert_eq!(overlay["note"], "Changed together");
    c.command("undo", json!({"token":result["undoToken"]})).unwrap();
    assert_eq!(c.get("overlay", "edited").unwrap(), before);
    c.command("overlay", json!({"ids":["edited"],"patch":{},"ownedAvatarsDelta":{"add":[],"remove":["product:base-a","product:base-b","  Legacy label  "]}})).unwrap();
    assert_eq!(c.get("overlay", "edited").unwrap().unwrap()["ownedAvatars"], json!([]));
    c.put("product", "edited", json!({"id":"edited","sourceTitle":"Refreshed clothing","sourceCategory":{"name":"Clothes"},"sourceTags":["Mafuyu"]})).unwrap();
    assert_eq!(dynamic_details(&c, "edited")["ownedAvatarEvidence"], json!([]));
}

#[test]
fn avatar_delta_rejects_conflicts_and_rolls_back_all_changes_for_missing_identity() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "one");
    product(&c, "two");
    for change in [
        json!({"id":"one","patch":{"ownedAvatars":[]},"ownedAvatarsDelta":{"add":[],"remove":[]}}),
        json!({"id":"one","patch":{},"ownedAvatarsDelta":{"add":["Mafuyu"],"remove":["Mafuyu"]}}),
        json!({"id":"one","patch":{},"ownedAvatarsDelta":{"add":[true],"remove":[]}}),
    ] {
        assert!(c.command("overlay", json!({"changes":[change]})).is_err());
    }
    assert!(c.command("overlay", json!({"changes":[
        {"id":"one","patch":{"note":"Must roll back"},"ownedAvatarsDelta":{"add":["booth:5007531"],"remove":[]}},
        {"id":"two","patch":{},"ownedAvatarsDelta":{"add":["product:missing"],"remove":[]}}
    ]})).is_err());
    assert!(c.get("overlay", "one").unwrap().is_none());
    assert!(c.get("overlay", "two").unwrap().is_none());
    assert!(dynamic_tag_ids(&c, "one").is_empty());
}

#[test]
fn avatar_delta_can_add_existing_evidence_only_choice_without_storing_its_derived_id() {
    let f = Fixture::new();
    let c = f.open();
    for id in ["origin", "target"] { product(&c, id); }
    c.command("overlay", json!({"ids":["origin"],"patch":{"ownedAvatars":["ソラネコ / Skycat"]}})).unwrap();
    let snapshot = c.command("avatar_options", json!({})).unwrap();
    let option = snapshot["avatarOptions"].as_array().unwrap().iter()
        .find(|option| text(option, "id").starts_with("avatar:")).unwrap();
    c.command("overlay", json!({"ids":["target"],"ownedAvatarsDelta":{"add":[option["id"]],"remove":[]}})).unwrap();
    assert_eq!(c.get("overlay", "target").unwrap().unwrap()["ownedAvatars"], json!(["ソラネコ / Skycat"]));
    let after = c.command("avatar_options", json!({})).unwrap();
    assert_eq!(after["avatarOptions"].as_array().unwrap().iter()
        .find(|entry| entry["id"] == option["id"]).unwrap()["matchingProductCount"], 2);
}

#[test]
fn avatar_delta_preserves_wrapped_derived_identity_before_origin_and_after_origin_deletion() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let c = f.open();
    for id in ["a-target", "z-origin"] { product(&c, id); }
    let label = "【無料】Ren♡Ai";
    let (identity_name, _) = crate::taxonomy::avatar_label_parts(label);
    let id = format!("avatar:{:x}", Sha256::digest(normalized(&identity_name).as_bytes()));
    c.command("overlay", json!({"ids":["z-origin"],"patch":{"ownedAvatars":[label]}})).unwrap();
    c.command("preferences", json!({"patch":{"avatarSettings":{(id.clone()):{"displayName":"Saved Ren name","shopName":"Saved Ren group","enabled":true}}}})).unwrap();
    let read_snapshot = |catalogue: &Catalogue| {
        let preferences = catalogue.get("preferences", "app").unwrap();
        let revision = catalogue.revision().unwrap();
        let snapshot = catalogue.command("avatar_options", json!({})).unwrap();
        assert_eq!(catalogue.get("preferences", "app").unwrap(), preferences);
        assert_eq!(catalogue.revision().unwrap(), revision);
        let derived: Vec<_> = snapshot["avatarOptions"].as_array().unwrap().iter()
            .filter(|entry| text(entry, "id").starts_with("avatar:")).cloned().collect();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0]["id"], id);
        assert_eq!(derived[0]["officialName"], "Ren♡Ai");
        assert_eq!(derived[0]["sourceIdentityValue"], label);
        assert_eq!(derived[0]["displayName"], "Saved Ren name");
        assert_eq!(derived[0]["shopName"], "Saved Ren group");
        assert_eq!(derived[0]["enabled"], true);
        derived[0].clone()
    };
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 1);
    c.command("overlay", json!({"ids":["a-target"],"ownedAvatarsDelta":{"add":[id.clone()],"remove":[]}})).unwrap();
    assert_eq!(c.get("overlay", "a-target").unwrap().unwrap()["ownedAvatars"], json!([label]));
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 2);
    c.delete("product", "z-origin").unwrap();
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 1);
    assert!(c.get("preferences", "app").unwrap().unwrap()["avatarSettings"].get(&id).is_some());
    drop(c);
    let c = f.open();
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 1);
}

#[test]
fn avatar_single_overlay_keeps_raw_derived_identity_after_origin_deletion_and_reopen() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let c = f.open();
    for id in ["a-target", "z-origin"] { product(&c, id); }
    let label = "【無料】Ren♡Ai";
    let (identity_name, _) = crate::taxonomy::avatar_label_parts(label);
    let id = format!("avatar:{:x}", Sha256::digest(normalized(&identity_name).as_bytes()));
    c.command("overlay", json!({"ids":["z-origin"],"patch":{"ownedAvatars":[label]}})).unwrap();
    c.command("preferences", json!({"patch":{"avatarSettings":{(id.clone()):{
        "displayName":"Saved single Ren name","shopName":"Saved single Ren group","enabled":true
    }}}})).unwrap();
    let read_snapshot = |catalogue: &Catalogue| {
        let preferences = catalogue.get("preferences", "app").unwrap();
        let revision = catalogue.revision().unwrap();
        let snapshot = catalogue.command("avatar_options", json!({})).unwrap();
        assert_eq!(catalogue.get("preferences", "app").unwrap(), preferences);
        assert_eq!(catalogue.revision().unwrap(), revision);
        let derived: Vec<_> = snapshot["avatarOptions"].as_array().unwrap().iter()
            .filter(|entry| text(entry, "id").starts_with("avatar:")).cloned().collect();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0]["id"], id);
        assert_eq!(derived[0]["officialName"], "Ren♡Ai");
        assert_eq!(derived[0]["sourceDisplayName"], "Ren♡Ai");
        assert_eq!(derived[0]["sourceIdentityValue"], label);
        assert_eq!(derived[0]["displayName"], "Saved single Ren name");
        assert_eq!(derived[0]["shopName"], "Saved single Ren group");
        assert_eq!(derived[0]["enabled"], true);
        derived[0].clone()
    };
    let origin = read_snapshot(&c);
    assert_eq!(origin["matchingProductCount"], 1);
    // Single Edit sends the raw representative from the selected option through
    // the existing exact overlay patch, rather than the runtime hash ID.
    c.command("overlay", json!({"ids":["a-target"],"patch":{
        "ownedAvatars":[origin["sourceIdentityValue"]]
    }})).unwrap();
    assert_eq!(c.get("overlay", "a-target").unwrap().unwrap()["ownedAvatars"], json!([label]));
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 2);
    c.delete("product", "z-origin").unwrap();
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 1);
    assert!(c.get("preferences", "app").unwrap().unwrap()["avatarSettings"].get(&id).is_some());
    drop(c);
    let c = f.open();
    assert_eq!(read_snapshot(&c)["matchingProductCount"], 1);
}

#[test]
fn avatar_label_cleanup_keeps_existing_derived_identity_settings() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let c = f.open();
    product(&c, "label-owner");
    let label = "【無料】Syntheticcat";
    let (identity_name, _) = crate::taxonomy::avatar_label_parts(label);
    let id = format!("avatar:{:x}", Sha256::digest(normalized(&identity_name).as_bytes()));
    c.command("overlay", json!({"ids":["label-owner"],"patch":{"ownedAvatars":[label]}})).unwrap();
    c.command("preferences", json!({"patch":{"avatarSettings":{(id.clone()):{"displayName":"Saved presentation","enabled":false}}}})).unwrap();
    let snapshot = c.command("avatar_options", json!({})).unwrap();
    let option = snapshot["avatarOptions"].as_array().unwrap().iter()
        .find(|option| option["id"] == id).unwrap();
    assert_eq!(option["officialName"], "Syntheticcat");
    assert_eq!(option["sourceDisplayName"], "Syntheticcat");
    assert_eq!(option["displayName"], "Saved presentation");
    assert_eq!(option["enabled"], false);
    assert!(!strings(&option["aliases"]).contains(&"Saved presentation".into()));
}

#[test]
fn avatar_uncertain_base_ignores_historical_text_aliases_without_erasing_them() {
    let f = Fixture::new();
    let c = f.open();
    synthetic_named_base(&c, "uncertain-base", "星猫 / 海猫");
    c.command("preferences", json!({"patch":{"avatarAliases":{"legacy guessed":"product:uncertain-base","product:uncertain-base":"product:uncertain-base"}}})).unwrap();
    for n in 0..6 {
        let id = format!("legacy-member-{n}");
        product(&c, &id);
        c.command("overlay", json!({"ids":[id],"patch":{"ownedAvatars":["Legacy guessed"]}})).unwrap();
    }
    let before = c.get("preferences", "app").unwrap().unwrap();
    let snapshot = c.command("avatar_options", json!({})).unwrap();
    let uncertain = snapshot["avatarOptions"].as_array().unwrap().iter()
        .find(|entry| entry["id"] == "product:uncertain-base").unwrap();
    assert_eq!(uncertain["matchingProductCount"], 1);
    assert_eq!(uncertain["enabled"], false);
    assert_eq!(c.get("preferences", "app").unwrap().unwrap(), before);
    c.refresh_avatar_defaults().unwrap();
    let preferences = c.get("preferences", "app").unwrap().unwrap();
    assert_eq!(preferences["avatarAliases"]["legacy guessed"], "product:uncertain-base");
    assert_eq!(preferences["avatarAliases"]["product:uncertain-base"], "product:uncertain-base");
    assert!(preferences["avatarAutomaticEnabled"].get("product:uncertain-base").is_none());
    c.command("overlay", json!({"ids":["legacy-member-0"],"patch":{"ownedAvatars":["product:uncertain-base"]}})).unwrap();
    let snapshot = c.command("avatar_options", json!({})).unwrap();
    assert_eq!(snapshot["avatarOptions"].as_array().unwrap().iter()
        .find(|entry| entry["id"] == "product:uncertain-base").unwrap()["matchingProductCount"], 2);
}

#[test]
fn avatar_legacy_human_name_romanizes_corrected_stem_instead_of_uncertain_source() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let c = f.open();
    product(&c, "legacy-owner");
    let source_label = "【 R18 Edition】苺佳(ichica) and GLC and 蒼葉(aoba)";
    let (identity_name, _) = crate::taxonomy::avatar_label_parts(source_label);
    let id = format!("avatar:{:x}", Sha256::digest(normalized(&identity_name).as_bytes()));
    let source_key = crate::taxonomy::canonical_tag(source_label);
    c.command("overlay", json!({"ids":["legacy-owner"],"patch":{"ownedAvatars":[source_label]}})).unwrap();
    c.command("preferences", json!({"patch":{
        "avatarAliases":{(source_key):id.clone()},
        "avatarSettings":{(id.clone()):{"originalName":"シロネコ","enabled":false}}
    }})).unwrap();
    let snapshot = c.command("avatar_options", json!({})).unwrap();
    let option = snapshot["avatarOptions"].as_array().unwrap().iter()
        .find(|option| option["id"] == id).unwrap();
    assert_eq!(option["officialName"], "シロネコ");
    assert_eq!(option["englishName"], "Shironeko");
    assert_eq!(option["nameKind"], "single");
    assert_eq!(c.get("overlay", "legacy-owner").unwrap().unwrap()["ownedAvatars"], json!([source_label]));
}

#[test]
fn dynamic_bilingual_aliases_deduplicate_independent_of_order_and_restart() {
    let mut ids = vec![];
    for labels in [
        json!(["ソラネコ / Skycat", "ソラネコ"]),
        json!(["ソラネコ", "ソラネコ / Skycat"]),
    ] {
        let f = Fixture::new();
        let c = f.open();
        product(&c, "synthetic");
        c.command(
            "overlay",
            json!({"ids":["synthetic"],"patch":{"ownedAvatars":labels}}),
        )
        .unwrap();
        let boot = c
            .command("bootstrap", json!({"includeProducts":false}))
            .unwrap();
        let options: Vec<_> = boot["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| text(a, "id").starts_with("avatar:"))
            .collect();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0]["matchingProductCount"], 1);
        assert_eq!(options[0]["displayName"], "ソラネコ / Skycat");
        let id = options[0]["id"].clone();
        ids.push(id.clone());
        let revision = c.revision().unwrap();
        c.command("bootstrap", json!({"includeProducts":false}))
            .unwrap();
        assert_eq!(c.revision().unwrap(), revision);
        drop(c);
        let c = f.open();
        let again = c
            .command("bootstrap", json!({"includeProducts":false}))
            .unwrap();
        let options: Vec<_> = again["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| text(a, "id").starts_with("avatar:"))
            .collect();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0]["id"], id);
        assert_eq!(
            c.command("query", json!({"avatar":"Skycat"})).unwrap()["matched"],
            1
        );
    }
    assert_eq!(ids[0], ids[1]);
}

#[test]
fn derived_avatar_options_filter_the_products_they_count() {
    let f = Fixture::new();
    let c = f.open();
    let clothing = |id: &str| json!({"id":id,"store":"BOOTH","sourceTitle":"Authored VRChat clothing","sourceCategory":{"name":"3D衣装"},"sourceTags":["VRChat"]});
    c.put("account", "a", json!({"id":"a","store":"BOOTH"})).unwrap();
    c.put("product", "early", clothing("early")).unwrap();
    // Unlisted Base names arrive raw from bound variants and manual choices.
    for (id, variant) in [
        ("900101", "SyntheticNeko専用"),
        ("900102", "ＳｙｎｔｈｅｔｉｃＮｅｋｏ専用"),
        ("900103", "【無料】Ren♡Ai専用"),
        ("900104", "シロネコ専用"),
    ] {
        let mut source = clothing(id);
        source["publicVariants"] = json!([{"id":format!("{id}-v"),"type":"digital","name":variant}]);
        c.put_many(vec![
            ("product".into(), id.into(), source),
            ("remote_file".into(), format!("{id}-f"), json!({"id":format!("{id}-f"),"productId":id,"accountId":"a","variantId":format!("{id}-v"),"name":"opaque.zip","revision":"r1"})),
            ("entitlement".into(), format!("{id}-e"), json!({"id":format!("{id}-e"),"productId":id,"accountId":"a","fileIds":[format!("{id}-f")],"manifestComplete":true})),
        ]).unwrap();
    }
    c.put("product", "manual", clothing("manual")).unwrap();
    c.command("overlay", json!({"ids":["manual"],"patch":{"ownedAvatars":["SYNTHETICNEKO"]}})).unwrap();
    let check = |c: &Catalogue, ren: u64| {
        let options = c.command("avatar_options", json!({})).unwrap()["avatarOptions"].as_array().unwrap().clone();
        let mut derived: Vec<_> = options.iter().filter(|o| text(o, "id").starts_with("avatar:")).map(|option| {
            let matched = c.command("query", json!({"avatar":option["id"]})).unwrap()["matched"].clone();
            (text(option, "officialName").to_owned(), option["matchingProductCount"].clone(), matched)
        }).collect();
        derived.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(derived, [
            ("Ren♡Ai".to_owned(), json!(ren), json!(ren)),
            ("SyntheticNeko".to_owned(), json!(3), json!(3)),
            ("シロネコ".to_owned(), json!(1), json!(1)),
        ]);
        let synthetic = options.iter().find(|o| o["officialName"] == "SyntheticNeko").unwrap();
        let mut scoped = strings(&c.command("query", json!({"avatar":synthetic["id"],"accountIds":["a"],"includeOffline":false})).unwrap()["matchedIds"]);
        scoped.sort();
        assert_eq!(scoped, ["900101", "900102"]);
    };
    // New names match before bootstrap persists their aliases.
    check(&c, 1);
    c.command("bootstrap", json!({"includeProducts":false})).unwrap();
    // A later cleaned spelling joins the existing derived identity.
    c.command("overlay", json!({"ids":["manual"],"patch":{"ownedAvatars":["SYNTHETICNEKO", "Ren♡Ai"]}})).unwrap();
    check(&c, 2);
    // A saved alias links an ambiguous label on an earlier product; it counts
    // regardless of whether that product precedes the identity's first name.
    let label = "Ren♡Ai and SyntheticNeko and シロネコ";
    assert!(!crate::taxonomy::avatar_label_is_unambiguous(label)
        || crate::taxonomy::parsed_avatar_label(label).name_kind != "single");
    let options = c.command("avatar_options", json!({})).unwrap()["avatarOptions"].clone();
    let ren = options.as_array().unwrap().iter().find(|o| o["officialName"] == "Ren♡Ai").unwrap()["id"].clone();
    let mut aliases = c.get("preferences", "app").unwrap().unwrap()["avatarAliases"].clone();
    aliases[crate::taxonomy::canonical_tag(label)] = ren;
    c.command("preferences", json!({"patch":{"avatarAliases":aliases}})).unwrap();
    c.command("overlay", json!({"ids":["early"],"patch":{"ownedAvatars":[label]}})).unwrap();
    check(&c, 3);
    drop(c);
    check(&f.open(), 3);
}

#[test]
fn explicit_public_404_evidence_hides_source_link_without_changing_raw_or_custom_url() {
    let f = Fixture::new();
    let c = f.open();
    let paired = json!({"id":"current-404","store":"BOOTH","sourceTitle":"Synthetic retained source","sourceUrl":"https://booth.pm/ja/items/999999","publicPageAvailability":"unavailable"});
    c.put("product", "current-404", paired.clone()).unwrap();
    assert_eq!(projection(&c, "current-404")["sourceUrlAvailable"], false);
    assert_eq!(
        projection(&c, "current-404")["publicPageAvailability"],
        "unavailable"
    );
    assert_eq!(
        c.get("product", "current-404").unwrap(),
        Some(paired.clone())
    );
    for (id, patch, available) in [
        ("unknown", json!({"publicPageAvailability":"unknown"}), true),
        (
            "available",
            json!({"publicPageAvailability":"available"}),
            true,
        ),
        (
            "unavailable",
            json!({"publicPageAvailability":"unavailable"}),
            false,
        ),
        (
            "other-store",
            json!({"store":"Gumroad","publicPageAvailability":"unavailable"}),
            false,
        ),
    ] {
        let mut source = paired.clone();
        source["id"] = json!(id);
        for (k, v) in patch.as_object().unwrap() {
            source[k] = v.clone();
        }
        c.put("product", id, source).unwrap();
        assert_eq!(projection(&c, id)["sourceUrlAvailable"], available, "{id}");
    }
    c.command(
        "overlay",
        json!({"ids":["current-404"],"patch":{"sourceUrl":"https://example.org/replacement"}}),
    )
    .unwrap();
    assert_eq!(projection(&c, "current-404")["sourceUrlAvailable"], true);
    c.command(
        "overlay",
        json!({"ids":["current-404"],"patch":{"sourceUrl":""}}),
    )
    .unwrap();
    assert_eq!(projection(&c, "current-404")["sourceUrlAvailable"], false);
    c.command(
        "overlay",
        json!({"ids":["current-404"],"patch":{},"clearFields":["sourceUrl"]}),
    )
    .unwrap();
    let reset = projection(&c, "current-404");
    assert_eq!(reset["sourceUrl"], paired["sourceUrl"]);
    assert_eq!(reset["sourceUrlAvailable"], false);
    assert_eq!(reset["sourceUrlOverride"], false);
    assert_eq!(
        c.get("product", "current-404").unwrap(),
        Some(paired.clone())
    );
    drop(c);
    let c = f.open();
    assert_eq!(projection(&c, "current-404")["sourceUrlAvailable"], false);
    assert_eq!(c.get("product", "current-404").unwrap(), Some(paired));
}

#[test]
fn bilingual_and_decorated_labels_preserve_source_and_product_identity() {
    let f = Fixture::new();
    let c = f.open();
    let title = "オリジナル3Dモデル「ソラネコ / Soraneko」Ver.1.2";
    for (id, source, shop) in [
        ("pair-a", title, "Authored A"),
        ("pair-b", title, "Authored B"),
        ("decorated", "★シロネコ★", "Authored C"),
    ] {
        c.put("product",id,json!({"id":id,"sourceTitle":source,"sourceCategory":{"name":"3Dアバター"},"shop":shop})).unwrap();
    }
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let options = boot["avatarOptions"].as_array().unwrap();
    for id in ["pair-a", "pair-b"] {
        let a = options
            .iter()
            .find(|a| text(a, "id") == format!("product:{id}"))
            .unwrap();
        assert_eq!(a["displayName"], "ソラネコ / Soraneko");
        assert_eq!(a["englishNameStatus"], "sourceTitleSpelling");
        assert_eq!(a["baseProductIds"], json!([id]));
        assert_eq!(c.get("product", id).unwrap().unwrap()["sourceTitle"], title);
    }
    let decorated = options
        .iter()
        .find(|a| a["id"] == "product:decorated")
        .unwrap();
    assert_eq!(decorated["englishName"], "Shironeko");
    assert_eq!(text(decorated, "displayName").matches(" / ").count(), 1);
}

#[test]
fn indexed_all_scope_excludes_foreign_apps_and_respects_manual_categories() {
    let f = Fixture::new();
    let c = f.open();
    for (id, tags, description) in [
        (
            "windows",
            json!(["Windows", "ツール"]),
            "Export generated output for VRChat",
        ),
        (
            "unity",
            json!(["Windows", "Unity", "ツール"]),
            "Unity editor tool",
        ),
        (
            "vr",
            json!(["Windows", "VRChat", "ツール"]),
            "Authored tool",
        ),
        (
            "ae-shader",
            json!(["AfterEffects", "Shader"]),
            "Authored AE effect",
        ),
        ("unknown-vr", json!(["VRChat"]), ""),
    ] {
        c.put("product",id,json!({"id":id,"sourceTitle":"Synthetic product","sourceTags":tags,"sourceDescription":description})).unwrap();
    }
    let q = c.command("query", json!({})).unwrap();
    assert_eq!(q["matched"], 3);
    assert_eq!(q["counts"]["all"], 3);
    assert!(!strings(&q["matchedIds"]).contains(&"windows".to_owned()));
    assert!(!strings(&q["matchedIds"]).contains(&"ae-shader".to_owned()));
    assert_eq!(
        c.command("query", json!({"category":"Shader"})).unwrap()["matchedIds"],
        json!(["ae-shader"])
    );
    c.command(
        "overlay",
        json!({"ids":["ae-shader"],"patch":{"category":"Shader"}}),
    )
    .unwrap();
    c.command(
        "overlay",
        json!({"ids":["unknown-vr"],"patch":{"category":"NonVRChat"}}),
    )
    .unwrap();
    let q = c.command("query", json!({})).unwrap();
    assert_eq!(q["matched"], 3);
    assert_eq!(q["counts"]["all"], 3);
    assert!(strings(&q["matchedIds"]).contains(&"ae-shader".to_owned()));
    assert!(!strings(&q["matchedIds"]).contains(&"unknown-vr".to_owned()));
    drop(c);
    let c = f.open();
    assert_eq!(c.command("query", json!({})).unwrap()["counts"]["all"], 3);
}

#[test]
fn product_nicknames_never_rename_canonical_avatar_labels() {
    let f = Fixture::new();
    let c = f.open();
    for (id, title) in [
        ("5007531", "真冬"),
        ("synthetic-base", "シロネコ"),
        ("linked-base", "Second edition"),
    ] {
        c.put(
            "product",
            id,
            json!({"id":id,"sourceTitle":title,"sourceCategory":{"name":"3Dアバター"}}),
        )
        .unwrap();
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"title":format!("Nickname {id}")}}),
        )
        .unwrap();
    }
    c.command(
        "overlay",
        json!({"ids":["linked-base"],"patch":{"ownedAvatars":["Mafuyu"]}}),
    )
    .unwrap();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let entries = boot["avatarOptions"].as_array().unwrap();
    let known = entries.iter().find(|a| a["id"] == "booth:5007531").unwrap();
    assert_eq!(known["displayName"], "真冬 / Mafuyu");
    assert!(strings(&known["aliases"]).contains(&"Nickname linked-base".to_owned()));
    assert_eq!(
        entries
            .iter()
            .find(|a| a["id"] == "product:synthetic-base")
            .unwrap()["displayName"],
        "シロネコ / Shironeko"
    );
    assert_eq!(
        c.command("product_details", json!({"id":"5007531"}))
            .unwrap()["title"],
        "Nickname 5007531"
    );
    c.command("preferences",json!({"patch":{"avatarSettings":{"booth:5007531":{"originalName":"Human identity","englishName":"Human English"}}}})).unwrap();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    assert_eq!(
        boot["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "booth:5007531")
            .unwrap()["displayName"],
        "Human identity / Human English"
    );
}

#[test]
fn explicit_manual_base_identity_unions_pins_without_same_name_merges() {
    let f = Fixture::new();
    let c = f.open();
    for (id, name, shop) in [
        ("5007531", "真冬", "Official"),
        ("manual-base", "Authored edition", "Other"),
        ("ambiguous-base", "真冬", "Unrelated"),
    ] {
        c.put(
            "product",
            id,
            json!({"id":id,"sourceTitle":name,"shop":shop,"sourceCategory":{"name":"3Dアバター"}}),
        )
        .unwrap();
    }
    c.command(
        "overlay",
        json!({"ids":["manual-base"],"patch":{"ownedAvatars":["Mafuyu"]}}),
    )
    .unwrap();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let options = boot["avatarOptions"].as_array().unwrap();
    let known = options.iter().find(|a| a["id"] == "booth:5007531").unwrap();
    assert_eq!(known["baseProductIds"], json!(["5007531", "manual-base"]));
    assert_eq!(known["matchingProductCount"], 2);
    assert!(!options.iter().any(|a| a["id"] == "product:manual-base"));
    assert!(options.iter().any(|a| a["id"] == "product:ambiguous-base"));
    let query=c.command("query",json!({"avatar":"Mafuyu","category":"Hair","search":"no match","accountIds":[],"limit":1,"offset":1})).unwrap();
    assert_eq!(query["matchedIds"], json!(["5007531", "manual-base"]));
    assert_eq!(query["products"][0]["id"], "manual-base");
    let without_offline=c.command("query",json!({"avatar":"Mafuyu","category":"Hair","search":"no match","accountIds":[],"includeOffline":false})).unwrap();
    assert_eq!(without_offline["matchedIds"], query["matchedIds"]);
}

#[test]
fn canonical_avatar_registry_counts_aliases_and_preserves_manual_disable() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product","6538026",json!({"id":"6538026","sourceTitle":"オリジナル3Dモデル『ミルティナ』","shop":"DOLOS art","sourceCategory":{"name":"3Dアバター"},"sourceTags":["VRChat"]})).unwrap();
    for n in 0..6 {
        let id = format!("compat{n}");
        product(&c, &id);
        c.command("overlay",json!({"ids":[id],"patch":{"ownedAvatars":["ミルティナ","Milltina","オリジナル3Dモデル『ミルティナ』"]}})).unwrap();
    }
    c.put("preferences","app",json!({"id":"app","avatarSettings":{"booth:6538026":{"enabled":false,"originalName":"Human name","englishName":"Human English","aliases":["Human alias"]}}})).unwrap();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let entries = boot["avatarOptions"].as_array().unwrap();
    let a = entries.iter().find(|v| v["id"] == "booth:6538026").unwrap();
    assert_eq!(a["matchingProductCount"], 7);
    assert_eq!(a["enabled"], false);
    assert_eq!(a["displayName"], "Human name / Human English");
    assert_eq!(a["baseProductIds"], json!(["6538026"]));
    assert_eq!(
        entries
            .iter()
            .filter(|v| v["productId"] == "6538026")
            .count(),
        1
    );
    c.command(
        "overlay",
        json!({"ids":["compat0"],"patch":{"ownedAvatars":["Human name / Human English"]}}),
    )
    .unwrap();
    assert_eq!(
        c.command("query", json!({"avatar":"Human name / Human English"}))
            .unwrap()["matched"],
        7
    );
    let refreshed = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    assert_eq!(
        refreshed["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "booth:6538026")
            .unwrap()["matchingProductCount"],
        7
    );
    assert!(!refreshed["avatarOptions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| text(a, "id").starts_with("avatar:")));
    let revision = c.revision().unwrap();
    c.command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    assert_eq!(
        revision,
        c.revision().unwrap(),
        "bootstrap reads must not change revisions"
    );
    drop(c);
    let c = f.open();
    assert_eq!(
        c.command("query", json!({"avatar":"Human alias"})).unwrap()["matched"],
        7
    );
    let prefs = c.get("preferences", "app").unwrap().unwrap();
    assert_eq!(prefs["avatarSettings"]["booth:6538026"]["enabled"], false);
}

#[test]
fn avatar_base_pins_override_ordinary_filters_before_paging_without_duplicates() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product","5007531",json!({"id":"5007531","sourceTitle":"真冬","sourceCategory":{"name":"3Dアバター"},"sensitive":true})).unwrap();
    for id in ["one", "two"] {
        product(&c, id);
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"ownedAvatars":["Mafuyu"]}}),
        )
        .unwrap();
    }
    let hidden=c.command("query",json!({"avatar":"booth:5007531","category":"Clothes","collectionId":"missing","search":"no match","quick":"downloaded","sensitive":"show","accountIds":[],"sort":"price","limit":1})).unwrap();
    assert_eq!(hidden["matchedIds"], json!(["5007531"]));
    assert_eq!(hidden["matched"], 1);
    let first = c
        .command(
            "query",
            json!({"avatar":"Mafuyu","category":"Hair","sort":"title","limit":1}),
        )
        .unwrap();
    assert_eq!(first["matched"], 3);
    assert_eq!(first["products"][0]["id"], "5007531");
    let rest = c
        .command(
            "query",
            json!({"avatar":"Mafuyu","category":"Hair","sort":"title","offset":1,"limit":9}),
        )
        .unwrap();
    assert_eq!(rest["products"].as_array().unwrap().len(), 2);
    assert!(rest["products"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["id"] != "5007531"));
    c.command(
        "overlay",
        json!({"ids":["5007531"],"patch":{"ownedAvatars":["Mafuyu"]}}),
    )
    .unwrap();
    assert_eq!(
        c.command("query", json!({"avatar":"Mafuyu"})).unwrap()["matched"],
        3
    );
}

#[test]
fn avatar_base_pins_respect_r18_hide_only_and_hidden_boundaries() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product", "5007531", json!({"id":"5007531","provider":"booth","sourceTitle":"真冬","sourceCategory":{"name":"3Dアバター"},"sensitive":true})).unwrap();
    let query = |mode| c.command("query", json!({"avatar":"booth:5007531","sensitive":mode})).unwrap()["matchedIds"].clone();
    assert_eq!(query("hide"), json!([]));
    assert_eq!(query("only"), json!(["5007531"]));
    c.command("overlay", json!({"ids":["5007531"],"patch":{"sensitive":false}})).unwrap();
    assert_eq!(query("only"), json!([]));
    assert_eq!(query("hide"), json!(["5007531"]));
    c.command("overlay", json!({"ids":["5007531"],"patch":{"hidden":true}})).unwrap();
    assert_eq!(query("show"), json!([]));
    assert_eq!(c.command("query", json!({"avatar":"booth:5007531","sensitive":"hide","hiddenOnly":true})).unwrap()["matchedIds"], json!(["5007531"]));
}

#[test]
fn avatar_default_threshold_and_unknown_base_identity_are_independent() {
    let f = Fixture::new();
    let c = f.open();
    for n in 0..5 {
        let id = format!("member{n}");
        product(&c, &id);
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"ownedAvatars":["Maya"]}}),
        )
        .unwrap();
    }
    c.put("product","synthetic-base",json!({"id":"synthetic-base","sourceTitle":"真冬","shop":"Independent author","sourceCategory":{"name":"3Dアバター"}})).unwrap();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    let entries = boot["avatarOptions"].as_array().unwrap();
    assert_eq!(
        entries.iter().find(|v| v["id"] == "booth:3390957").unwrap()["enabled"],
        false
    );
    assert_eq!(
        entries
            .iter()
            .find(|v| v["id"] == "product:synthetic-base")
            .unwrap()["baseProductIds"],
        json!(["synthetic-base"])
    );
    assert_eq!(
        c.command("query", json!({"avatar":"booth:5007531"}))
            .unwrap()["matched"],
        0
    );
    product(&c, "member5");
    c.command(
        "overlay",
        json!({"ids":["member5"],"patch":{"ownedAvatars":["Maya"]}}),
    )
    .unwrap();
    let maya = |c: &Catalogue| {
        c.command("bootstrap", json!({"includeProducts":false}))
            .unwrap()["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "booth:3390957")
            .unwrap()
            .clone()
    };
    assert_eq!(maya(&c)["matchingProductCount"], 6);
    assert_eq!(
        maya(&c)["enabled"],
        false,
        "Compatible clothes do not establish Base ownership"
    );
    c.put(
        "product",
        "3390957",
        json!({"id":"3390957","sourceTitle":"舞夜","sourceCategory":{"name":"3Dアバター"}}),
    )
    .unwrap();
    assert_eq!(maya(&c)["enabled"], false, "Ingestion does not toggle the avatar mid-sync");
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(maya(&c)["enabled"], true);
    c.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"booth:3390957":{"enabled":false}}}}),
    )
    .unwrap();
    assert_eq!(
        maya(&c)["enabled"],
        false,
        "Manual off wins over count and owned Base"
    );
    c.delete("product", "3390957").unwrap();
    c.command(
        "preferences",
        json!({"patch":{"avatarSettings":{"booth:3390957":{"enabled":true}},"uiScale":999}}),
    )
    .unwrap();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    assert_eq!(boot["preferences"]["uiScale"], 300);
    assert_eq!(
        boot["avatarOptions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "booth:3390957")
            .unwrap()["enabled"],
        true
    );
}

#[test]
fn avatar_automatic_defaults_change_only_at_sync_closure_and_survive_reopen() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let enabled = |catalogue: &Catalogue| {
        catalogue.command("bootstrap", json!({"includeProducts":false})).unwrap()["avatarOptions"]
            .as_array().unwrap().iter().find(|entry| entry["id"] == "booth:3390957").unwrap()["enabled"].clone()
    };
    product(&c, "first-clothes");
    c.command("overlay", json!({"ids":["first-clothes"],"patch":{"ownedAvatars":["Maya"]}})).unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), false);
    c.put("job", "sync", json!({"id":"sync","type":"boothSync","mode":"incremental","state":"running"})).unwrap();
    for n in 0..6 {
        let id = format!("later-clothes-{n}");
        product(&c, &id);
        c.command("overlay", json!({"ids":[id],"patch":{"ownedAvatars":["Maya"]}})).unwrap();
        assert_eq!(enabled(&c), false, "A bootstrap during sync only reads the previous default");
    }
    c.put("product", "3390957", json!({"id":"3390957","sourceTitle":"舞夜","sourceCategory":{"name":"3Dアバター"}})).unwrap();
    assert_eq!(enabled(&c), false, "A late Base does not change the default during sync");
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), false, "An active sync prevents automatic analysis");
    c.put("job", "sync-other-store", json!({"id":"sync-other-store","type":"gumroadSync","state":"queued"})).unwrap();
    c.put("job", "sync", json!({"id":"sync","type":"boothSync","mode":"incremental","state":"completed"})).unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), false, "Another provider's queued sync retains defaults");
    c.put("job", "sync-other-store", json!({"id":"sync-other-store","type":"gumroadSync","state":"cancelled"})).unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), true);
    assert_eq!(c.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"]["booth:3390957"], true);
    drop(c);
    let c = fixture.open();
    assert_eq!(enabled(&c), true, "Saved automatic defaults survive restart");
    c.put("job", "sync-full", json!({"id":"sync-full","type":"boothSync","mode":"forced","state":"running"})).unwrap();
    c.delete("product", "3390957").unwrap();
    assert_eq!(enabled(&c), true, "A full refresh retains the prior default until closure");
    c.put("job", "sync-full", json!({"id":"sync-full","type":"boothSync","mode":"forced","state":"failed"})).unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), false, "Terminal partial failures recompute from currently available records");
    let revision = c.revision().unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(revision, c.revision().unwrap(), "Unchanged recomputation is idempotent");
}

#[test]
fn avatar_user_overrides_remain_separate_from_recomputed_defaults() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put("product", "3390957", json!({"id":"3390957","sourceTitle":"舞夜","sourceCategory":{"name":"3Dアバター"}})).unwrap();
    for n in 0..6 {
        let id = format!("compatible-{n}");
        product(&c, &id);
        c.command("overlay", json!({"ids":[id],"patch":{"ownedAvatars":["Maya"]}})).unwrap();
    }
    let enabled = |catalogue: &Catalogue| {
        catalogue.command("bootstrap", json!({"includeProducts":false})).unwrap()["avatarOptions"]
            .as_array().unwrap().iter().find(|entry| entry["id"] == "booth:3390957").unwrap()["enabled"].clone()
    };
    c.command("preferences", json!({"patch":{"avatarSettings":{"booth:3390957":{"enabled":false}}}})).unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), false, "Manual off wins over the automatic enabled snapshot");
    let prefs = c.get("preferences", "app").unwrap().unwrap();
    assert_eq!(prefs["avatarSettings"]["booth:3390957"]["enabled"], false);
    assert_eq!(prefs["avatarAutomaticEnabled"]["booth:3390957"], true);
    c.command("preferences", json!({"patch":{"avatarSettings":{"booth:3390957":{"enabled":true}}}})).unwrap();
    c.delete("product", "3390957").unwrap();
    c.refresh_avatar_defaults().unwrap();
    assert_eq!(enabled(&c), true, "Manual on survives an automatic off result");
    drop(c);
    let c = fixture.open();
    assert_eq!(enabled(&c), true, "Manual on survives restart");
    assert_eq!(c.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"]["booth:3390957"], Value::Null);
}

#[test]
fn source_links_provenance_and_unclassified_vr_scope_preserve_manual_choices() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product","synthetic",json!({"id":"synthetic","sourceTitle":"Uncategorized authored product","store":"BOOTH","sourceUrl":"https://booth.pm/items/999999","publicPageAvailability":"unavailable","sourceTags":["VRChat"]})).unwrap();
    for id in ["a", "b"] {
        c.put(
            "account",
            id,
            json!({"id":id,"store":"BOOTH","name":format!("Account {id}")}),
        )
        .unwrap();
        let aid = format!("acq-{id}");
        c.put(
            "acquisition",
            &aid,
            json!({"id":aid,"productId":"synthetic","accountId":id}),
        )
        .unwrap();
    }
    let p = projection(&c, "synthetic");
    assert_eq!(p["category"], "NonVRChat");
    assert_eq!(p["sourceUrlAvailable"], false);
    assert_eq!(p["sources"].as_array().unwrap().len(), 2);
    assert_eq!(p["sources"][0]["label"], "BOOTH – Account a");
    assert_eq!(c.command("query", json!({})).unwrap()["matched"], 1);
    c.command("overlay",json!({"ids":["synthetic"],"patch":{"sourceUrl":"https://example.org/item?view=full#about","category":"NonVRChat"}})).unwrap();
    assert_eq!(projection(&c, "synthetic")["sourceUrlAvailable"], true);
    assert_eq!(
        c.get("product", "synthetic").unwrap().unwrap()["sourceUrl"],
        "https://booth.pm/items/999999"
    );
    assert_eq!(c.command("query", json!({})).unwrap()["matched"], 0);
    assert_eq!(
        c.command("query", json!({"category":"NonVRChat"})).unwrap()["matched"],
        1
    );
    assert!(c
        .command(
            "overlay",
            json!({"ids":["synthetic"],"patch":{"sourceUrl":"https://user:secret@example.org/"}})
        )
        .is_err());
    c.command(
        "overlay",
        json!({"ids":["synthetic"],"patch":{"sourceUrl":""}}),
    )
    .unwrap();
    assert_eq!(projection(&c, "synthetic")["sourceUrlAvailable"], false);
}

#[test]
fn current_projection_restart_preserves_overlay() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "synthetic");
    c.command("overlay",json!({"ids":["synthetic"],"patch":{"title":"Human title","category":"Tool","note":"Keep this"}})).unwrap();
    drop(c);
    let c = f.open();
    let p = projection(&c, "synthetic");
    assert_eq!(p["category"], "Tool");
    assert_eq!(p["title"], "Human title");
    assert_eq!(p["note"], "Keep this");
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/catalogue-synthetic")
            .join(format!("nna-catalogue-synthetic-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn open(&self) -> Catalogue {
        Catalogue::open(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn product(c: &Catalogue, id: &str) {
    c.put("product",id,json!({"id":id,"title":"Synthetic ＡＢＣ 髪型","shop":"Synthetic Maker","tags":["hair","blue"],"sourceCategory":{"name":"Hair"}})).unwrap();
}
fn projection(c: &Catalogue, id: &str) -> Value {
    c.command("bootstrap", json!({})).unwrap()["products"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| text(p, "id") == id)
        .unwrap()
        .clone()
}
#[test]
fn reopen_corrects_stale_partial_badge_without_store_sync() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put("product", "p", json!({"id":"p","sourceTitle":"Authored product"})).unwrap();
    c.put("entitlement", "e", json!({"id":"e","productId":"p","fileIds":["f"],"manifestComplete":false})).unwrap();
    c.put("remote_file", "f", json!({"id":"f","productId":"p","name":"authored.unitypackage","revision":"r1"})).unwrap();
    c.put("artifact", "a", json!({"id":"a","productId":"p","fileId":"f","revision":"r1","status":"ready"})).unwrap();
    assert_eq!(projection(&c,"p")["status"], "ready");
    {
        let conn = c.inner.lock().unwrap();
        conn.execute("UPDATE products SET status='partial',downloaded=0,body=json_set(body,'$.status','partial','$.fullyDownloaded',json('false')),summary=json_set(summary,'$.status','partial','$.fullyDownloaded',json('false')) WHERE id='p'", []).unwrap();
    }
    drop(c);
    let reopened = fixture.open();
    let product = projection(&reopened,"p");
    assert_eq!(product["manifestComplete"], false);
    assert_eq!(product["status"], "ready");
    assert_eq!(product["fullyDownloaded"], true);
    assert_eq!(reopened.command("query", json!({"quick":"downloaded","limit":10})).unwrap()["allDownloaded"], true);
}
#[test]
fn source_overlay_restart_and_cjk_index() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "100");
    let result=c.command("overlay",json!({"ids":["100"],"patch":{"title":"Human name","note":"備考の短文","tags":["hair"]}})).unwrap();
    c.put("product","100",json!({"id":"100","title":"New source","tags":["blue","hair","green"],"sourceCategory":{"name":"Hair"}})).unwrap();
    let p = projection(&c, "100");
    assert_eq!(p["title"], "Human name");
    assert_eq!(p["sourceTitle"], "New source");
    assert_eq!(p["tags"], json!(["hair"]));
    assert!(strings(&p["rejectedTags"]).contains(&"blue".into()));
    assert_eq!(
        c.command("query", json!({"search":"備考"})).unwrap()["matchedIds"],
        json!(["100"])
    );
    drop(c);
    let c = f.open();
    assert_eq!(projection(&c, "100")["title"], "Human name");
    c.command("overlay", json!({"ids":["100"],"patch":{"title":"Later"}}))
        .unwrap();
    assert_eq!(
        c.command("undo", json!({"token":result["token"]})).unwrap()["conflicts"],
        json!(["100"])
    );
}

#[test]
fn gumroad_scope_rule_applies_to_new_source_sync_without_reclassifying_on_open() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let id = "gumroad:authored-existing-shader";
    let mut source = json!({"id":id,"provider":"gumroad","store":"Gumroad",
        "sourceTitle":"Authored shader for Blender","sourceTags":["VRChat","Shader"],
        "observedAt":"2026-10-01T00:00:00Z"});
    c.put("product", id, source.clone()).unwrap();
    // Preserve a category materialized before the stricter Gumroad rule.
    let old = json!({"category":"Shader","basis":"tags","inferred":true,
        "evidence":[],"scope":"vrchat","scopeEvidence":{"vrchat":true}});
    c.inner.lock().unwrap().execute(
        "UPDATE products SET category='Shader',in_all=1,body=json_set(body,'$.category','Shader','$.categoryInference',json(?2)),summary=json_set(summary,'$.category','Shader','$.categoryInference',json(?2)) WHERE id=?1",
        params![id,serde_json::to_string(&old).unwrap()],
    ).unwrap();
    drop(c);

    let reopened = fixture.open();
    assert_eq!(projection(&reopened, id)["category"], "Shader");
    assert_eq!(reopened.get("product", id).unwrap().unwrap(), source);
    reopened.put("product", id, source.clone()).unwrap();
    assert_eq!(projection(&reopened, id)["category"], "Shader");

    source["observedAt"] = json!("2026-10-14T00:00:00Z");
    reopened.put("product", id, source).unwrap();
    assert_eq!(projection(&reopened, id)["category"], "NonVRChat");
    assert_eq!(reopened.command("query", json!({})).unwrap()["matched"], 0);
    assert_eq!(reopened.command("query", json!({"category":"NonVRChat"})).unwrap()["matchedIds"], json!([id]));
}
#[test]
fn atomic_batch_and_collections() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    product(&c, "2");
    assert!(c
        .command(
            "overlay",
            json!({"ids":["1","missing"],"patch":{"note":"must rollback"}})
        )
        .is_err());
    assert_eq!(projection(&c, "1")["note"], "");
    c.command("collection", json!({"op":"create","id":"a","name":"A"}))
        .unwrap();
    c.command("collection", json!({"op":"create","id":"b","name":"B"}))
        .unwrap();
    c.command(
        "collection",
        json!({"op":"add","collectionIds":["a","b"],"ids":["1","2"]}),
    )
    .unwrap();
    assert_eq!(
        c.get("collection", "a").unwrap().unwrap()["productIds"],
        json!(["1", "2"])
    );
    assert!(c
        .command(
            "collection",
            json!({"op":"add","collectionIds":["a","missing"],"ids":["1"]})
        )
        .is_err());
}
#[test]
fn current_revision_and_incomplete_manifest_never_ready() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.put(
        "remote_file",
        "f",
        json!({"id":"f","productId":"1","accountId":"a","name":"test.zip","revision":"r1"}),
    )
    .unwrap();
    c.put(
        "entitlement",
        "e",
        json!({"id":"e","productId":"1","accountId":"a","fileIds":["f"],"manifestComplete":true}),
    )
    .unwrap();
    c.put("artifact","a",json!({"id":"a","productId":"1","fileId":"f","revision":"r1","status":"ready","rootId":"r","relativePath":"product/test.zip","originalAvailable":false})).unwrap();
    assert_eq!(projection(&c, "1")["status"], "ready");
    assert_eq!(projection(&c, "1")["files"][0]["originalAvailable"], false);
    c.put(
        "remote_file",
        "f",
        json!({"id":"f","productId":"1","accountId":"a","name":"test.zip","revision":"r2"}),
    )
    .unwrap();
    assert_eq!(projection(&c, "1")["status"], "notDownloaded");
    c.put(
        "entitlement",
        "e",
        json!({"id":"e","productId":"1","accountId":"a","fileIds":["f"],"manifestComplete":false}),
    )
    .unwrap();
    assert_eq!(projection(&c, "1")["status"], "notDownloaded");
}
#[test]
fn partial_twenty_nine_of_thirty_without_failure_is_partial() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    let mut files = vec![];
    for n in 0..30 {
        let id = format!("f{n}");
        files.push(id.clone());
        c.put("remote_file",&id,json!({"id":id,"productId":"1","name":format!("file{n}.unitypackage"),"revision":"one"})).unwrap();
        if n < 29 {
            c.put("artifact",&id,json!({"id":id,"productId":"1","fileId":id,"revision":"one","status":"ready","relativePath":format!("files/{n}.unitypackage")})).unwrap();
        }
    }
    c.put(
        "entitlement",
        "e",
        json!({"id":"e","productId":"1","fileIds":files,"manifestComplete":true}),
    )
    .unwrap();
    let p = projection(&c, "1");
    assert_eq!(p["downloaded"], 29);
    assert_eq!(p["total"], 30);
    assert_eq!(p["status"], "partial");
}
#[test]
fn restore_rejects_before_replacement_and_preserves_semantics() {
    let a = Fixture::new();
    let c = a.open();
    product(&c, "1");
    c.command(
        "overlay",
        json!({"ids":["1"],"patch":{"note":"Retained","rejectedTags":["blue"]}}),
    )
    .unwrap();
    c.put("acquisition","unresolved",json!({"id":"unresolved","sourceRecordId":"observation-unknown","productId":null,"source":"purchase"})).unwrap();
    c.put(
        "root",
        "r",
        json!({"id":"r","path":"C:/synthetic/local-only","status":"online"}),
    )
    .unwrap();
    let manifest = c.manifest().unwrap();
    assert!(manifest.to_string().find("C:/synthetic").is_none());
    let mut invalid = manifest.clone();
    invalid["version"] = json!(999);
    assert!(c.restore_manifest(invalid).is_err());
    assert_eq!(projection(&c, "1")["note"], "Retained");
    let b = Fixture::new();
    let other = b.open();
    other.restore_manifest(manifest).unwrap();
    assert_eq!(projection(&other, "1")["note"], "Retained");
    assert_eq!(
        other.get("root", "r").unwrap().unwrap()["status"],
        "needsMapping"
    );
    assert!(other.get("acquisition", "unresolved").unwrap().is_some());
}
#[test]
fn real_two_device_exchange_restart_conflicts() {
    let a = Fixture::new();
    let b = Fixture::new();
    let exchange = Fixture::new();
    let ca = a.open();
    let cb = b.open();
    product(&ca, "1");
    ca.command("exchange", json!({"folder":exchange.0}))
        .unwrap();
    cb.command("exchange", json!({"folder":exchange.0}))
        .unwrap();
    ca.command(
        "overlay",
        json!({"ids":["1"],"patch":{"note":"from A","title":"A title"}}),
    )
    .unwrap();
    cb.command(
        "overlay",
        json!({"ids":["1"],"patch":{"note":"from B","sensitive":true}}),
    )
    .unwrap();
    ca.command("exchange", json!({"folder":exchange.0}))
        .unwrap();
    cb.command("exchange", json!({"folder":exchange.0}))
        .unwrap();
    ca.command("exchange", json!({"folder":exchange.0}))
        .unwrap();
    assert_eq!(projection(&ca, "1"), projection(&cb, "1"));
    assert_eq!(projection(&ca, "1")["title"], "A title");
    assert_eq!(projection(&ca, "1")["sensitive"], true);
    let conflicts = ca.command("conflicts", json!({})).unwrap();
    assert!(conflicts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["field"] == "note"));
    drop(ca);
    let ca = a.open();
    assert_eq!(ca.command("conflicts", json!({})).unwrap(), conflicts);
    ca.command("exchange", json!({"folder":exchange.0}))
        .unwrap();
    assert_eq!(projection(&ca, "1"), projection(&cb, "1"));
}
#[test]
fn defaults_scope_r18_and_unknown_last() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.put(
        "product",
        "2",
        json!({"id":"2","title":"Synthetic illustration","tags":[],"price":1}),
    )
    .unwrap();
    assert_eq!(
        c.command("query", json!({})).unwrap()["matchedIds"],
        json!(["1"])
    );
    assert_eq!(
        c.command("query", json!({"category":"NonVRChat"})).unwrap()["matchedIds"],
        json!(["2"])
    );
    c.command("overlay", json!({"ids":["1"],"patch":{"sensitive":true}}))
        .unwrap();
    assert_eq!(
        c.command("query", json!({"sensitive":"hide"})).unwrap()["matchedIds"],
        json!([])
    );
    assert_eq!(
        c.command("query", json!({"sensitive":"only"})).unwrap()["matchedIds"],
        json!(["1"])
    );
    assert_eq!(
        c.get("preferences", "app").unwrap().unwrap()["autoDownloadAll"],
        false
    );
    let prefs = c
        .command(
            "preferences",
            json!({"patch":{"autoDownloadAll":false,"downloadConcurrency":99}}),
        )
        .unwrap();
    assert_eq!(prefs["autoDownloadAll"], false);
    assert_eq!(prefs["downloadConcurrency"], 6);
    c.command("preferences", json!({"patch":{"autoDownloadAll":true}}))
        .unwrap();
    for field in ["setupComplete"] {
        assert_eq!(
            c.command("preferences", json!({"patch":{field:true}}))
                .unwrap_err()
                .to_string(),
            "library_not_configured"
        );
    }
    c.put(
        "root",
        "setup-root",
        json!({"id":"setup-root","path":f.0,"status":"needsMapping","active":true}),
    )
    .unwrap();
    c.command(
        "preferences",
        json!({"patch":{"libraryRootId":"setup-root"}}),
    )
    .unwrap();
    assert!(c
        .command("preferences", json!({"patch":{"setupComplete":true}}))
        .is_err());
    c.put(
        "root",
        "setup-root",
        json!({"id":"setup-root","path":f.0,"status":"online","active":true}),
    )
    .unwrap();
    let complete = c
        .command("preferences", json!({"patch":{"setupComplete":true}}))
        .unwrap();
    assert_eq!(complete["autoDownloadAll"], true);
    c.put(
        "root",
        "setup-root",
        json!({"id":"setup-root","status":"offline"}),
    )
    .unwrap();
    assert!(c
        .command("preferences", json!({"patch":{"theme":"dark"}}))
        .is_ok());
    drop(c);
    assert_eq!(
        f.open().get("preferences", "app").unwrap().unwrap()["autoDownloadAll"],
        true
    );
}
#[test]
fn restore_then_mutation_restart_exchange_does_not_resurrect() {
    let a = Fixture::new();
    let b = Fixture::new();
    let folder = Fixture::new();
    let ca = a.open();
    let cb = b.open();
    product(&ca, "old");
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    product(&cb, "new");
    let manifest = cb.manifest().unwrap();
    ca.restore_manifest(manifest).unwrap();
    ca.command(
        "overlay",
        json!({"ids":["new"],"patch":{"note":"after restore"}}),
    )
    .unwrap();
    drop(ca);
    let ca = a.open();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    assert!(ca.get("product", "old").unwrap().is_none());
    assert_eq!(projection(&ca, "new")["note"], "after restore");
    drop(ca);
    let ca = a.open();
    assert!(ca.get("product", "old").unwrap().is_none());
}
#[test]
fn exact_tag_equivalence_and_manual_tags_survive() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product","1",json!({"id":"1","title":"Synthetic","tags":["Casual","Mafuyu coat","Mafuyu"],"sourceCategory":{"name":"Hair"}})).unwrap();
    c.command(
        "overlay",
        json!({"ids":["1"],"patch":{"tags":["カジュアル","Mafuyu"],"ownedAvatars":["真冬"]}}),
    )
    .unwrap();
    let p = projection(&c, "1");
    assert!(strings(&p["tags"]).contains(&"カジュアル".into()));
    assert!(strings(&p["tags"]).contains(&"Mafuyu".into()));
    assert!(!strings(&p["rejectedTags"]).contains(&"Casual".into()));
    assert_eq!(
        c.command("query", json!({"avatar":"booth:5007531"}))
            .unwrap()["matchedIds"],
        json!(["1"])
    );
    c.command(
        "overlay",
        json!({"ids":["1"],"patch":{"tags":["Mafuyu"],"ownedAvatars":[]}}),
    )
    .unwrap();
    assert!(strings(&projection(&c, "1")["tags"]).contains(&"Mafuyu".into()));
}
#[test]
fn heterogeneous_overlay_is_atomic_and_undo_field_aware() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    product(&c, "2");
    let result=c.command("overlay",json!({"changes":[{"id":"1","patch":{"title":"One"}},{"id":"2","patch":{"note":"Two"}}]})).unwrap();
    c.command("overlay", json!({"ids":["1"],"patch":{"note":"Unrelated"}}))
        .unwrap();
    let undone = c
        .command("undo", json!({"token":result["undoToken"]}))
        .unwrap();
    assert_eq!(undone["changed"], 2);
    assert_eq!(projection(&c, "1")["note"], "Unrelated");
}
#[test]
#[ignore = "Manual release-scale verification: independently authored 10k products / 300k file records"]
fn release_scale_10000_products_300000_files() {
    use std::time::Instant;
    // Explicit ignored runs retain synthetic fixtures even after a panic, so a
    // failed sync measurement never requires another twelve-minute population.
    let reuse = std::env::var("NNA_SCALE_REUSE_FIXTURE").ok();
    let f = std::mem::ManuallyDrop::new(if let Some(name) = &reuse {
        let suffix = name
            .strip_prefix("nna-catalogue-synthetic-")
            .expect("synthetic fixture name required");
        Uuid::parse_str(suffix).expect("synthetic fixture UUID required");
        assert!(!name.contains(['/', '\\', ':']));
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/catalogue-synthetic")
            .canonicalize()
            .unwrap();
        let path = base.join(name).canonicalize().unwrap();
        assert_eq!(path.parent(), Some(base.as_path()));
        // The lead explicitly identified this completed pre-marker fixture.
        assert!(
            path.join("scale-fixture.json").is_file()
                || suffix == "6263b886-7762-4c8d-bc19-4d87fce1d25d"
        );
        Fixture(path)
    } else {
        Fixture::new()
    });
    println!(
        "synthetic_fixture={}",
        f.0.file_name().unwrap().to_string_lossy()
    );
    let opened = Instant::now();
    let c = f.open();
    println!(
        "database_open_ms={} reused={}",
        opened.elapsed().as_millis(),
        reuse.is_some()
    );
    let start = Instant::now();
    for batch in 0..if reuse.is_some() { 0 } else { 100 } {
        let mut records = vec![];
        for n in batch * 100..(batch + 1) * 100 {
            let id = format!("{}", 900000000 + n);
            let mut file_ids = vec![];
            records.push(("product".into(),id.clone(),json!({"id":id,"title":format!("Synthetic ＡＢＣ 髪型 商品 {n:05}"),"shop":format!("Synthetic Maker {}",n%50),"sourceCategory":{"name":"Hair"},"tags":["hair","Casual"],"price":if n%9==0{None}else{Some(n%1000)},"sensitive":n%7==0})));
            for file in 0..30 {
                let fid = format!("{id}:file:{file}");
                file_ids.push(fid.clone());
                records.push(("remote_file".into(),fid.clone(),json!({"id":fid,"productId":id,"accountId":"synthetic","name":format!("Synthetic_説明_{n:05}_{file:02}.unitypackage"),"revision":"synthetic-v1","sizeBytes":12345})));
            }
            records.push(("entitlement".into(),id.clone(),json!({"id":id,"productId":id,"accountId":"synthetic","fileIds":file_ids,"manifestComplete":true})));
        }
        c.put_many(records).unwrap();
        if batch % 10 == 9 {
            println!(
                "seed_products={} seed_files={} elapsed_ms={}",
                (batch + 1) * 100,
                (batch + 1) * 3000,
                start.elapsed().as_millis()
            );
        }
    }
    println!("synthetic_population_ms={}", start.elapsed().as_millis());
    {
        let conn = c.inner.lock().unwrap();
        for (kind, expected) in [
            ("product", 10000),
            ("entitlement", 10000),
            ("remote_file", 300000),
        ] {
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM entities WHERE kind=?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count, expected);
        }
        let products: i64 = conn.query_row("SELECT COUNT(*) FROM entities WHERE kind='product' AND CAST(id AS INTEGER) BETWEEN 900000000 AND 900009999 AND json_extract(body,'$.title') GLOB 'Synthetic ＡＢＣ 髪型 商品 *'", [], |r| r.get(0)).unwrap();
        let files: i64 = conn.query_row("SELECT COUNT(*) FROM entities WHERE kind='remote_file' AND json_extract(body,'$.accountId')='synthetic' AND json_extract(body,'$.revision')='synthetic-v1' AND json_extract(body,'$.name') GLOB 'Synthetic_説明_*.unitypackage'", [], |r| r.get(0)).unwrap();
        let projections: i64 = conn
            .query_row("SELECT COUNT(*) FROM products", [], |r| r.get(0))
            .unwrap();
        assert_eq!((products, files, projections), (10000, 300000, 10000));
    }
    let marker = json!({"kind":"independently-authored-scale-fixture","version":1,"products":10000,"remoteFiles":300000});
    let marker_path = f.0.join("scale-fixture.json");
    if marker_path.exists() {
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&marker_path).unwrap()).unwrap(),
            marker
        );
    } else {
        std::fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();
    }
    let begin = Instant::now();
    let boot = c
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    assert!(boot["products"].as_array().unwrap().is_empty());
    println!("lean_bootstrap_ms={}", begin.elapsed().as_millis());
    let mut timings = vec![];
    for i in 0..15 {
        let search = ["髪", "ＡＢＣ", "説明_00001", "casual", "maker 3"][i % 5];
        let begin = Instant::now();
        let result=c.command("query",json!({"search":search,"includeFiles":false,"limit":100,"sort":(["title","acquired","price"][i%3]),"direction":if i%2==0{"asc"}else{"desc"}})).unwrap();
        let serialized = serde_json::to_vec(&result).unwrap();
        assert!(!serialized.is_empty());
        timings.push(begin.elapsed().as_micros());
    }
    timings.sort();
    let p95 = timings[14];
    println!(
        "query_samples=15 query_p95_us={p95} query_min_us={} query_median_us={}",
        timings[0], timings[7]
    );
    // Cold history validation is a background task after restart; catalogue
    // reads must remain available while its strict decoder is working.
    drop(c);
    let c = f.open();
    let begin = Instant::now();
    c.command("collection",json!({"operation":"create","name":"Synthetic cold collection","id":format!("scale-cold-{}",Uuid::new_v4()),"productIds":["900000001"]})).unwrap();
    let cold_collection_ms = begin.elapsed().as_millis();
    let pending: i64 = c
        .inner
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row.get(0))
        .unwrap();
    println!("cold_collection_commit_ms={cold_collection_ms} pending_sync_intents={pending}");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let complete = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = c.clone();
    let reader_barrier = barrier.clone();
    let reader_complete = complete.clone();
    let concurrent = std::thread::spawn(move || {
        let mut samples = vec![];
        reader_barrier.wait();
        while !reader_complete.load(std::sync::atomic::Ordering::Relaxed) || samples.len() < 15 {
            let started = Instant::now();
            let result = reader
                .command(
                    "query",
                    json!({"search":"髪","includeFiles":false,"limit":100}),
                )
                .unwrap();
            let _bytes = serde_json::to_vec(&result).unwrap();
            samples.push(started.elapsed().as_micros());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        samples
    });
    barrier.wait();
    let begin = Instant::now();
    let prepared = c.ensure_document();
    let cold_ms = begin.elapsed().as_millis();
    complete.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut cold_samples = concurrent.join().unwrap();
    let (copy_us, decode_us) = prepared.unwrap();
    cold_samples.sort();
    println!("cold_cache_prepare_ms={cold_ms} cold_copy_and_lock_wait_us={copy_us} cold_decode_outside_lock_us={decode_us} cold_query_samples={} cold_query_p95_us={} cold_query_max_us={}",cold_samples.len(),cold_samples[((cold_samples.len()*95).div_ceil(100)).saturating_sub(1)],cold_samples.last().unwrap());
    let folder = std::mem::ManuallyDrop::new(Fixture::new());
    println!(
        "synthetic_exchange_fixture={}",
        folder.0.file_name().unwrap().to_string_lossy()
    );
    let begin = Instant::now();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    println!("initial_exchange_ms={}", begin.elapsed().as_millis());
    let begin = Instant::now();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    println!("exchange_warmup_ms={}", begin.elapsed().as_millis());
    let revision = c.revision().unwrap();
    let begin = Instant::now();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    println!("unchanged_exchange_ms={}", begin.elapsed().as_millis());
    assert_eq!(c.revision().unwrap(), revision);
    let other = std::mem::ManuallyDrop::new(Fixture::new());
    println!(
        "synthetic_peer_fixture={}",
        other.0.file_name().unwrap().to_string_lossy()
    );
    let peer = other.open();
    let begin = Instant::now();
    peer.command("exchange", json!({"folder":folder.0}))
        .unwrap();
    println!("fresh_peer_import_ms={}", begin.elapsed().as_millis());
    let remote_note = format!("Synthetic remote edit {}", Uuid::new_v4());
    peer.command(
        "overlay",
        json!({"ids":["900000001"],"patch":{"note":remote_note}}),
    )
    .unwrap();
    peer.command("exchange", json!({"folder":folder.0}))
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let complete = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = c.clone();
    let reader_barrier = barrier.clone();
    let reader_complete = complete.clone();
    let concurrent = std::thread::spawn(move || {
        let mut samples = vec![];
        reader_barrier.wait();
        while !reader_complete.load(std::sync::atomic::Ordering::Relaxed) || samples.len() < 15 {
            let started = Instant::now();
            let value = reader
                .command(
                    "query",
                    json!({"search":"髪","includeFiles":false,"limit":100}),
                )
                .unwrap();
            let _bytes = serde_json::to_vec(&value).unwrap();
            samples.push(started.elapsed().as_micros());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        samples
    });
    barrier.wait();
    let begin = Instant::now();
    let exchanged = c.command("exchange", json!({"folder":folder.0}));
    let exchange_ms = begin.elapsed().as_millis();
    complete.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut samples = concurrent.join().unwrap();
    exchanged.unwrap();
    samples.sort();
    println!("one_remote_overlay_exchange_ms={} contention_query_samples={} contention_query_p95_us={} contention_query_max_us={}",exchange_ms,samples.len(),samples[((samples.len()*95).div_ceil(100)).saturating_sub(1)],samples.last().unwrap());
    assert_eq!(
        c.command("product_details", json!({"id":"900000001"}))
            .unwrap()["note"],
        remote_note
    );
    drop(c);
    let begin = Instant::now();
    let reopened = f.open();
    println!("local_open_ms={}", begin.elapsed().as_millis());
    let begin = Instant::now();
    reopened
        .command("bootstrap", json!({"includeProducts":false}))
        .unwrap();
    println!("restart_lean_bootstrap_ms={}", begin.elapsed().as_millis());
    assert_eq!(
        reopened
            .command(
                "query",
                json!({"search":"remote edit","includeFiles":false})
            )
            .unwrap()["matchedIds"],
        json!(["900000001"])
    );
    println!("target_query_p95_under_200ms={}", p95 <= 200_000);
}
#[test]
fn acquisition_dates_accept_complete_provider_timestamps_without_rewriting_raw_text() {
    for (raw, expected) in [
        ("2026/2/1 09:08:07", "2026-02-01T09:08:07.000000000Z"),
        ("2026/2/1 09:08", "2026-02-01T09:08:00.000000000Z"),
        ("2026/2/1", "2026-02-01T00:00:00.000000000Z"),
    ] {
        assert_eq!(acquisition::date_key(raw).as_deref(), Some(expected));
    }
    for invalid in [
        "2026/2/30 09:08:07",
        "2026/2/1 25:08:07",
        "2026/2/1 09:99",
        "receipt 2026/2/1 09:08:07",
        "2026/2/1 09:08:07 trailing",
    ] {
        assert!(acquisition::date_key(invalid).is_none(), "{invalid}");
    }
    let raw = json!({"id":"a","accountId":"a","acquiredDateText":"","acquiredAt":"2026-02-01T09:08:07Z","paidAmountText":"Original provider text"});
    let mut p = json!({"provenance":acquisition::normalize_provenance(&[raw.clone()])});
    acquisition::apply_default(&mut p, "");
    assert_eq!(p["acquired"], raw["acquiredAt"]);
    for field in [
        "id",
        "accountId",
        "acquiredDateText",
        "acquiredAt",
        "paidAmountText",
    ] {
        assert_eq!(p["provenance"][0][field], raw[field]);
    }
    let mut p = json!({"provenance":acquisition::normalize_provenance(&[
        json!({"id":"z","acquiredDateText":"Later provider text"}),
        json!({"id":"a","acquiredDateText":"Unparsed provider text"}),
    ])});
    acquisition::apply_default(&mut p, "");
    assert_eq!(p["acquired"], "Unparsed provider text");
    assert!(p["acquiredSortKey"].is_null());
    // Equal dates use the stable acquisition id, not input traversal order.
    p["provenance"] = json!(acquisition::normalize_provenance(&[
        json!({"id":"z","acquiredDateText":"2026/2/1"}),
        json!({"id":"a","acquiredDateText":"2026年2月1日"}),
    ]));
    acquisition::apply_default(&mut p, "");
    assert_eq!(p["acquired"], "2026年2月1日");
}

#[test]
fn paid_price_requires_unambiguous_scoped_item_evidence_and_preserves_zero() {
    let proof = |id: &str, account: &str, amount: Value, currency: &str| json!({"id":id,"accountId":account,"paidPrice":amount,"paidCurrency":currency,"paidPriceBasis":"productLineTotal"});
    let unproven = json!({"id":"raw","accountId":"a","paidAmountText":"¥ 900","paidPrice":900,"paidCurrency":"JPY","orderTotal":900});
    let mut p = json!({"price":999,"provenance":acquisition::normalize_provenance(&[
        unproven.clone(), proof("1","a",json!(0),"JPY"), proof("2","a",json!(0),"JPY"),
        proof("3","b",json!(500),"JPY"), proof("negative","a",json!(-1),"JPY"),
        proof("fraction","a",json!(1.5),"JPY"),
    ])});
    acquisition::apply_default(&mut p, "");
    assert!(p["paidPrice"].is_null());
    let accounts = vec![json!("a")];
    acquisition::apply_scope(&mut p, Some(&accounts));
    assert_eq!(p["paidPrice"], 0.0);
    assert_eq!(p["paidCurrency"], "JPY");
    assert_eq!(p["price"], 999);
    assert_eq!(
        p["provenance"][0]["paidAmountText"],
        unproven["paidAmountText"]
    );
    let accounts = vec![json!("b")];
    acquisition::apply_scope(&mut p, Some(&accounts));
    assert_eq!(p["paidPrice"], 500.0);
    acquisition::apply_scope(&mut p, Some(&vec![]));
    assert!(p["paidPrice"].is_null());
    assert!(p["paidCurrency"].is_null());
    for rows in [
        vec![unproven],
        vec![
            json!({"id":"free","source":"free"}),
            json!({"id":"gift","source":"gift"}),
        ],
        vec![
            proof("1", "a", json!(500), "JPY"),
            proof("2", "a", json!(500), "USD"),
        ],
        vec![
            proof("1", "a", json!(500), "JPY"),
            proof("2", "a", json!(12.50), "USD"),
        ],
        vec![proof("unsafe", "a", json!(9_007_199_254_740_992u64), "JPY")],
    ] {
        p["provenance"] = json!(acquisition::normalize_provenance(&rows));
        acquisition::apply_default(&mut p, "");
        assert!(p["paidPrice"].is_null());
    }
    for (amount, currency) in [(12.50, "USD"), (0.0, "EUR")] {
        p["provenance"] = json!(acquisition::normalize_provenance(&[
            proof("one", "a", json!(amount), currency),
            proof("duplicate", "a", json!(amount), currency),
        ]));
        acquisition::apply_scope(&mut p, Some(&vec![json!("a")]));
        assert_eq!(p["paidPrice"], amount);
        assert_eq!(p["paidCurrency"], currency);
        acquisition::apply_scope(&mut p, Some(&vec![]));
        assert!(p["paidPrice"].is_null());
        assert!(p["paidCurrency"].is_null());
    }
}

#[test]
fn provider_prices_preserve_currency_identity_scope_and_portable_facts() {
    let f = Fixture::new();
    let c = f.open();
    for (id, provider, store, amount, currency) in [
        ("900001", "booth", "BOOTH", 800.0, "JPY"),
        ("gumroad:900001", "gumroad", "Gumroad", 12.50, "USD"),
        ("jinxxy:900001", "jinxxy", "Jinxxy", 99.25, "EUR"),
        ("gumroad:cheap", "gumroad", "Gumroad", 2.0, "USD"),
        ("jinxxy:unknown", "jinxxy", "Jinxxy", 5.0, ""),
    ] {
        let mut source = json!({"id":id,"provider":provider,"store":store,"title":"Same synthetic Hair","sourceCategory":{"name":"Hair"},"price":amount,"priceCurrency":currency});
        if provider == "gumroad" {
            source["sourceDescription"] = json!("Authored hair for VRChat.");
        }
        c.put("product", id, source).unwrap();
    }
    let mut booth = c.get("product", "900001").unwrap().unwrap();
    booth["publicVariants"] = json!([{"id":"owned","name":"Owned","price":800,"currency":"JPY"}]);
    c.put("product", "900001", booth).unwrap();
    c.put("acquisition","booth-owned",json!({"id":"booth-owned","productId":"900001","accountId":"booth-account","variantIds":["owned"]})).unwrap();
    c.put("account", "gum-account", json!({"id":"gum-account","name":"Synthetic","provider":"gumroad","store":"Gumroad","status":"connected","identityHash":"authored-stable-buyer-hash","identityVerified":true,"nativeProfile":"authored-device-only"})).unwrap();
    c.put("acquisition", "gum-paid", json!({"id":"gum-paid","productId":"gumroad:900001","provider":"gumroad","accountId":"gum-account","source":"purchase","acquiredDateText":"2026-10-01","paidPrice":10.25,"paidCurrency":"USD","paidPriceBasis":"productLineTotal"})).unwrap();
    c.command("overlay", json!({"ids":["gumroad:900001"],"patch":{"title":"Edited Gumroad","note":"Keep correction"}})).unwrap();
    let list = c
        .command(
            "query",
            json!({"sort":"price","direction":"asc","includeFiles":false,"presentation":"list"}),
        )
        .unwrap();
    assert_eq!(
        list["matchedIds"],
        json!([
            "jinxxy:900001",
            "900001",
            "gumroad:cheap",
            "gumroad:900001",
            "jinxxy:unknown"
        ])
    );
    let desc = c
        .command("query", json!({"sort":"price","direction":"desc"}))
        .unwrap();
    assert_eq!(
        desc["matchedIds"],
        json!([
            "jinxxy:900001",
            "900001",
            "gumroad:900001",
            "gumroad:cheap",
            "jinxxy:unknown"
        ])
    );
    let scoped = c
        .command(
            "query",
            json!({"accountIds":["gum-account"],"includeFiles":false,"presentation":"list"}),
        )
        .unwrap();
    let detail = c
        .command(
            "product_details",
            json!({"id":"gumroad:900001","accountIds":["gum-account"]}),
        )
        .unwrap();
    for field in [
        "id",
        "title",
        "note",
        "provider",
        "price",
        "priceCurrency",
        "currentPriceText",
        "paidPrice",
        "paidCurrency",
        "acquired",
    ] {
        assert_eq!(scoped["products"][0][field], detail[field], "{field}");
    }
    assert_eq!(detail["price"], 12.50);
    assert_eq!(detail["priceCurrency"], "USD");
    assert_eq!(detail["currentPriceText"], "USD 12.5");
    assert_eq!(detail["paidPrice"], 10.25);
    assert_eq!(detail["paidCurrency"], "USD");
    assert_eq!(dynamic_details(&c, "900001")["currentPriceText"], "¥ 800");
    assert!(dynamic_details(&c, "jinxxy:unknown")["price"].is_null());
    c.put("product", "jinxxy:unknown", json!({"id":"jinxxy:unknown","provider":"jinxxy","store":"Jinxxy","title":"Synthetic formatted quote","priceFormatted":"¥ 1200","priceSourceCurrency":"JPY"})).unwrap();
    let formatted = dynamic_details(&c, "jinxxy:unknown");
    assert!(formatted["price"].is_null());
    assert_eq!(formatted["priceCurrency"], "JPY");
    assert_eq!(formatted["currentPriceText"], "¥ 1200");
    let empty_scope = c
        .command(
            "product_details",
            json!({"id":"gumroad:900001","accountIds":[]}),
        )
        .unwrap();
    assert!(empty_scope["paidPrice"].is_null());
    assert_eq!(empty_scope["priceCurrency"], "USD");
    let csv = c.command("export_csv", json!({})).unwrap();
    assert!(text(&csv, "text")
        .lines()
        .next()
        .unwrap()
        .ends_with("tags,priceCurrency,paidPrice,paidCurrency"));
    assert!(text(&csv, "text").contains("\"USD\",\"10.25\",\"USD\""));
    let manifest = c.manifest().unwrap();
    assert_eq!(manifest["records"]["account"][0]["provider"], "gumroad");
    assert_eq!(manifest["records"]["account"][0]["status"], "disconnected");
    assert_eq!(
        manifest["records"]["account"][0]["identityHash"],
        "authored-stable-buyer-hash"
    );
    assert!(manifest["records"]["account"][0]
        .get("identityVerified")
        .is_none());
    assert!(manifest["records"]["account"][0]
        .get("nativeProfile")
        .is_none());
    let fresh = Fixture::new();
    let other = fresh.open();
    other.restore_manifest(manifest).unwrap();
    let account = other.get("account", "gum-account").unwrap().unwrap();
    assert_eq!(account["identityHash"], "authored-stable-buyer-hash");
    assert_eq!(account["status"], "disconnected");
    assert_ne!(account["identityVerified"], true);
    assert_eq!(
        dynamic_details(&other, "gumroad:900001")["paidCurrency"],
        "USD"
    );
    assert_eq!(
        dynamic_details(&other, "gumroad:900001")["note"],
        "Keep correction"
    );
    assert_eq!(other.list("product").unwrap().len(), 5);
    let restored_formatted = dynamic_details(&other, "jinxxy:unknown");
    assert!(restored_formatted["price"].is_null());
    assert_eq!(restored_formatted["currentPriceText"], "¥ 1200");
}

#[test]
fn local_manifest_and_batch_jobs_preserve_required_files_without_accounts() {
    let f = Fixture::new();
    let c = f.open();
    c.put("root", "local-root", json!({"id":"local-root","label":"Synthetic local root","path":"X:/Synthetic local library","status":"online"})).unwrap();
    let local = |id: &str, files: Value| json!({"id":id,"provider":"local","store":"Local Files","custom":true,"title":"Synthetic local Hair","sourceCategory":{"name":"Hair"},"localManifest":{"complete":true,"fileIds":files,"directories":["package/empty/nested"]},"importedAt":"2026-10-10T00:00:00Z"});
    c.put(
        "product",
        "local:one",
        local("local:one", json!(["local:file-a", "local:file-b"])),
    )
    .unwrap();
    c.put(
        "product",
        "local:two",
        local("local:two", json!(["local:file-c"])),
    )
    .unwrap();
    c.put("acquisition", "local-import-fingerprint:synthetic", json!({"id":"local-import-fingerprint:synthetic","productId":"local:one","observationKind":"localImport","provider":"local","source":"local","importedAt":"2026-10-10T00:00:00Z"})).unwrap();
    assert_eq!(dynamic_details(&c, "local:one")["manifestComplete"], false);
    for (file, product) in [
        ("local:file-a", "local:one"),
        ("local:file-b", "local:one"),
        ("local:file-c", "local:two"),
    ] {
        c.put("remote_file",file,json!({"id":file,"productId":product,"provider":"local","name":"Synthetic.unitypackage","revision":"r1","sizeBytes":4})).unwrap();
    }
    let pending = dynamic_details(&c, "local:one");
    assert_eq!(pending["manifestComplete"], true);
    assert_eq!(pending["status"], "notDownloaded");
    assert_eq!(pending["accountIds"], json!([]));
    assert_eq!(pending["acquired"], "");
    assert!(pending["price"].is_null());
    assert!(pending["paidPrice"].is_null());
    assert_eq!(pending["sources"][0]["provider"], "Local Files");
    let mut job = json!({"id":"local-import:synthetic","type":"storage:import_local","productIds":["local:one","local:two"],"completedProductIds":[],"state":"running","progress":1});
    c.put("job", "local-import:synthetic", job.clone()).unwrap();
    assert_eq!(dynamic_details(&c, "local:one")["status"], "processing");
    assert_eq!(dynamic_details(&c, "local:two")["status"], "processing");
    let before = c.change_domains().unwrap();
    job["progress"] = json!(99);
    c.put("job", "local-import:synthetic", job.clone()).unwrap();
    let after = c.change_domains().unwrap();
    assert!(after.0 > before.0);
    assert_eq!(after.1, before.1);
    let artifact = |file: &str, product: &str, status: &str| json!({"id":format!("artifact:{file}"),"fileId":file,"productId":product,"rootId":"local-root","revision":"r1","status":status,"hash":"synthetic-hash","sizeBytes":4,"relativePath":format!("package/{}",file.replace(':',"-")),"originalAvailable":true});
    c.put(
        "artifact",
        "artifact:local:file-a",
        artifact("local:file-a", "local:one", "ready"),
    )
    .unwrap();
    job["state"] = json!("cancelled");
    c.put("job", "local-import:synthetic", job.clone()).unwrap();
    assert_eq!(dynamic_details(&c, "local:one")["status"], "partial");
    assert_eq!(dynamic_details(&c, "local:one")["fullyDownloaded"], false);
    assert_eq!(dynamic_details(&c, "local:one")["downloaded"], 1);
    c.put(
        "artifact",
        "artifact:local:file-b",
        artifact("local:file-b", "local:one", "extractionFailed"),
    )
    .unwrap();
    assert_eq!(dynamic_details(&c, "local:one")["status"], "attention");
    c.put(
        "artifact",
        "artifact:local:file-b",
        artifact("local:file-b", "local:one", "ready"),
    )
    .unwrap();
    job["state"] = json!("running");
    job["completedProductIds"] = json!(["local:one"]);
    c.put_many(vec![(
        "job".into(),
        "local-import:synthetic".into(),
        job.clone(),
    )])
    .unwrap();
    assert_eq!(dynamic_details(&c, "local:one")["status"], "ready");
    assert_eq!(dynamic_details(&c, "local:two")["status"], "processing");
    job["state"] = json!("failed");
    job["errorCode"] = json!("storage.file_missing");
    c.put("job", "local-import:synthetic", job).unwrap();
    assert_eq!(dynamic_details(&c, "local:one")["status"], "ready");
    assert_eq!(dynamic_details(&c, "local:two")["status"], "attention");
    assert_eq!(
        c.command("query", json!({"accountIds":["unrelated"]}))
            .unwrap()["matched"],
        0
    );
    assert!(c.list("account").unwrap().is_empty());
    let manifest = c.manifest().unwrap();
    drop(c);
    let c = f.open();
    assert_eq!(dynamic_details(&c, "local:one")["status"], "ready");
    let fresh = Fixture::new();
    let other = fresh.open();
    other.restore_manifest(manifest).unwrap();
    let restored = dynamic_details(&other, "local:one");
    assert_eq!(
        restored["localManifest"]["directories"],
        json!(["package/empty/nested"])
    );
    // Metadata restore requires root mapping and storage verification before Ready.
    assert_eq!(restored["status"], "notDownloaded");
    assert_eq!(
        other.get("root", "local-root").unwrap().unwrap()["status"],
        "needsMapping"
    );
    other.put("root", "local-root", json!({"id":"local-root","label":"Synthetic mapped root","path":"Y:/Synthetic restored library","status":"online"})).unwrap();
    assert_eq!(
        dynamic_details(&other, "local:one")["status"],
        "notDownloaded"
    );
    // Model the verified facts that Storage publishes after checking copied files.
    for file in ["local:file-a", "local:file-b"] {
        other
            .put(
                "artifact",
                &format!("artifact:{file}"),
                artifact(file, "local:one", "ready"),
            )
            .unwrap();
    }
    assert_eq!(dynamic_details(&other, "local:one")["status"], "ready");
    assert_eq!(restored["acquired"], "");
    assert!(restored["paidPrice"].is_null());
    assert!(other.list("account").unwrap().is_empty());
    assert!(other.list("job").unwrap().is_empty());
    let invalid = local("local:empty", json!([]));
    assert!(other.put("product", "local:empty", invalid).is_err());
}

#[test]
fn offline_product_scope_preserves_accounts_paging_and_local_metadata() {
    let f = Fixture::new();
    let c = f.open();
    for (id, title, custom) in [
        ("owned-a", "A owned store", false),
        ("owned-b", "B owned store", false),
        ("edited-store", "C edited store", true),
        ("unassigned-store", "D unassigned store", false),
    ] {
        c.put("product",id,json!({"id":id,"provider":"booth","title":title,"custom":custom,"sourceCategory":{"name":"Hair"}})).unwrap();
    }
    for (id, account) in [("owned-a", "a"), ("owned-b", "b"), ("edited-store", "a")] {
        c.put("acquisition",id,json!({"id":id,"productId":id,"accountId":account,"source":"purchase","acquiredDateText":if id=="owned-a"{"2026/10/1"}else{""}})).unwrap();
    }
    for (id, title) in [("local:blank", "E local blank"), ("local:dated", "F local dated")] {
        c.put("product",id,json!({"id":id,"provider":"local","store":"Local Files","custom":true,"title":title,"sourceTags":["Mafuyu"],"sourceCategory":{"name":"Hair"},"importedAt":"2026-10-10T00:00:00Z"})).unwrap();
    }
    // Authored observed acquisition metadata must remain independent of store scope.
    c.put("acquisition","local-observation",json!({"id":"local-observation","productId":"local:dated","provider":"local","source":"local","observationKind":"localImport","importedAt":"2026-10-10T00:00:00Z","acquiredDateText":"2026/1/15"})).unwrap();
    c.command("overlay",json!({"ids":["local:dated"],"patch":{"note":"Human local note","tags":["Human local tag"],"ownedAvatars":["Mafuyu"]}})).unwrap();

    let all = json!(["owned-a", "owned-b", "edited-store", "unassigned-store", "local:blank", "local:dated"]);
    for (scope, expected) in [
        (json!({}), all.clone()),
        (json!({"accountIds":null}), all.clone()),
        (json!({"accountIds":null,"includeOffline":true}), all.clone()),
        (json!({"includeOffline":false}), json!(["owned-a", "owned-b", "edited-store", "unassigned-store"])),
        (json!({"accountIds":null,"includeOffline":false}), json!(["owned-a", "owned-b", "edited-store", "unassigned-store"])),
        (json!({"accountIds":["a"]}), json!(["owned-a", "edited-store"])),
        (json!({"accountIds":["a"],"includeOffline":false}), json!(["owned-a", "edited-store"])),
        (json!({"accountIds":["a"],"includeOffline":true}), json!(["owned-a", "edited-store", "local:blank", "local:dated"])),
        (json!({"accountIds":["b"],"includeOffline":true}), json!(["owned-b", "local:blank", "local:dated"])),
        (json!({"accountIds":["a","b"],"includeOffline":true}), json!(["owned-a", "owned-b", "edited-store", "local:blank", "local:dated"])),
        (json!({"accountIds":[]}), json!([])),
        (json!({"accountIds":[],"includeOffline":false}), json!([])),
        (json!({"accountIds":[],"includeOffline":true}), json!(["local:blank", "local:dated"])),
    ] {
        let mut args = scope.clone();
        args["sort"] = json!("title");
        args["direction"] = json!("asc");
        let result = c.command("query", args).unwrap();
        assert_eq!(result["matchedIds"], expected, "{scope}");
        assert_eq!(result["matched"], expected.as_array().unwrap().len(), "{scope}");
        assert_eq!(result["total"], 6, "{scope}");
    }
    let paged=c.command("query",json!({"accountIds":["a"],"includeOffline":true,"sort":"title","direction":"asc","limit":1,"offset":1,"includeFiles":false,"presentation":"list"})).unwrap();
    assert_eq!(paged["matchedIds"], json!(["owned-a", "edited-store", "local:blank", "local:dated"]));
    assert_eq!(paged["products"].as_array().unwrap().len(), 1);
    assert_eq!(paged["products"][0]["id"], "edited-store");
    assert_eq!(paged["allDownloaded"], false);
    let searched=c.command("query",json!({"accountIds":["a"],"includeOffline":true,"search":"local","sort":"title"})).unwrap();
    assert_eq!(searched["matchedIds"], json!(["local:blank", "local:dated"]));
    let acquired=c.command("query",json!({"accountIds":["a"],"includeOffline":true,"sort":"acquired","direction":"asc"})).unwrap();
    assert_eq!(acquired["matchedIds"], json!(["local:dated", "owned-a", "edited-store", "local:blank"]));

    let avatar=c.command("query",json!({"accountIds":[],"includeOffline":true,"avatar":"Mafuyu","sort":"title","includeFiles":false,"presentation":"list"})).unwrap();
    assert_eq!(avatar["matchedIds"], json!(["local:blank", "local:dated"]));
    let excluded_avatar=c.command("query",json!({"accountIds":[],"includeOffline":false,"avatar":"Mafuyu"})).unwrap();
    assert_eq!(excluded_avatar["matchedIds"], json!([]));
    for id in ["local:blank", "local:dated"] {
        let unscoped = dynamic_details(&c, id);
        for accounts in [json!([]), json!(["a"])] {
            let details=c.command("product_details",json!({"id":id,"accountIds":accounts})).unwrap();
            for field in ["title", "note", "tags", "ownedAvatars", "ownedAvatarEvidence", "acquired", "acquiredSortKey", "price", "priceCurrency", "currentPriceText", "paidPrice", "paidCurrency", "provenance"] {
                assert_eq!(details[field], unscoped[field], "{id}: {field}");
            }
        }
        let summary=avatar["products"].as_array().unwrap().iter().find(|p|p["id"]==id).unwrap();
        for field in ["acquired", "acquiredSortKey", "price", "priceCurrency", "currentPriceText", "paidPrice", "paidCurrency", "ownedAvatars"] {
            assert_eq!(summary[field], unscoped[field], "{id}: {field}");
        }
        assert!(unscoped["price"].is_null());
        assert!(unscoped["paidPrice"].is_null());
    }
    assert_eq!(dynamic_details(&c,"local:blank")["acquired"], "");
    assert_eq!(dynamic_details(&c,"local:dated")["acquired"], "2026/1/15");
}

#[test]
fn scoped_acquisition_summary_and_sort_match_details_before_paging() {
    let f = Fixture::new();
    let c = f.open();
    for id in ["1", "2", "3"] {
        product(&c, id);
    }
    for (id, product_id, account, date, amount) in [
        ("1a", "1", "a", "2026/10/1 12:00:01", 300),
        ("1b", "1", "b", "2026/1/1 12:00:01", 700),
        ("2a", "2", "a", "2026/2/1 12:00:01", 400),
        ("3a", "3", "a", "Unparsed provider date", 0),
    ] {
        c.put("acquisition",id,json!({"id":id,"productId":product_id,"accountId":account,"source":"purchase","acquiredDateText":date,"paidPrice":amount,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal","variantIds":["owned"]})).unwrap();
    }
    let mut source = c.get("product", "1").unwrap().unwrap();
    source["price"] = json!(900);
    source["publicVariants"] = json!([{"id":"owned","name":"Owned","price":900,"currency":"JPY"}]);
    c.put("product", "1", source).unwrap();
    let all = c
        .command("query", json!({"sort":"acquired","direction":"asc"}))
        .unwrap();
    assert_eq!(all["matchedIds"], json!(["1", "2", "3"]));
    let scoped = c
        .command(
            "query",
            json!({"accountIds":["a"],"sort":"acquired","direction":"asc","limit":1,"offset":1}),
        )
        .unwrap();
    assert_eq!(scoped["matchedIds"], json!(["2", "1", "3"]));
    assert_eq!(scoped["products"][0]["id"], "1");
    let details = c
        .command("product_details", json!({"id":"1","accountIds":["a"]}))
        .unwrap();
    for field in [
        "acquired",
        "acquiredSortKey",
        "paidPrice",
        "paidCurrency",
        "price",
        "currentPriceText",
        "provenance",
    ] {
        assert_eq!(details[field], scoped["products"][0][field], "{field}");
    }
    assert_eq!(details["acquired"], "2026/10/1 12:00:01");
    assert_eq!(details["paidPrice"], 300.0);
    assert_eq!(details["price"], 900.0);
    assert_eq!(details["provenance"].as_array().unwrap().len(), 2);
    let desc = c
        .command(
            "query",
            json!({"accountIds":["a"],"sort":"acquired","direction":"desc"}),
        )
        .unwrap();
    assert_eq!(desc["matchedIds"], json!(["1", "2", "3"]));
    assert_eq!(desc["products"][2]["acquired"], "Unparsed provider date");
    assert!(desc["products"][2]["acquiredSortKey"].is_null());
    let all_details = c
        .command("product_details", json!({"id":"1","accountIds":null}))
        .unwrap();
    assert_eq!(all_details["acquired"], "2026/1/1 12:00:01");
    assert!(all_details["paidPrice"].is_null());
    let none = c
        .command("product_details", json!({"id":"1","accountIds":[]}))
        .unwrap();
    assert_eq!(none["acquired"], "");
    assert!(none["paidPrice"].is_null());
    assert!(none["price"].is_null());
}

#[test]
fn pinned_base_does_not_borrow_excluded_account_acquisition_or_product_fallback() {
    let f = Fixture::new();
    let c = f.open();
    c.put("product","base",json!({"id":"base","sourceTitle":"SyntheticScopeAvatar","sourceCategory":{"name":"3Dアバター"},"acquired":"2000/1/1"})).unwrap();
    c.put("acquisition","base-b",json!({"id":"base-b","productId":"base","accountId":"b","acquiredDateText":"2026/2/1 10:20:30","paidPrice":500,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal"})).unwrap();
    assert_eq!(dynamic_details(&c, "base")["acquired"], "2000/1/1");
    for accounts in [json!([]), json!(["a"])] {
        let q = c
            .command(
                "query",
                json!({"avatar":"product:base","accountIds":accounts}),
            )
            .unwrap();
        assert_eq!(q["matchedIds"], json!(["base"]));
        assert_eq!(q["products"][0]["acquired"], "");
        assert!(q["products"][0]["paidPrice"].is_null());
    }
    let b = c
        .command("product_details", json!({"id":"base","accountIds":["b"]}))
        .unwrap();
    assert_eq!(b["acquired"], "2026/2/1 10:20:30");
    assert_eq!(b["paidPrice"], 500.0);
}

#[test]
fn current_projection_prices_and_dates_survive_restart_without_source_history_changes() {
    let f = Fixture::new();
    let c = f.open();
    for (id, price, listing) in [
        ("exact", Value::Null, "¥ 1,200"),
        ("range", Value::Null, "¥ 500 ~"),
        ("closed", Value::Null, "¥ 500 ~ ¥ 900"),
        ("zero", json!(0), "¥ 900"),
        ("known", json!(700), "¥ 900"),
        ("fraction", json!(1.5), ""),
        ("negative", json!(-1), "invalid"),
        ("unsafe", json!(9_007_199_254_740_992u64), ""),
        ("fraction-fallback", json!(1.5), "¥ 800"),
        ("negative-fallback", json!(-1), "¥ 0"),
        ("unsafe-range", json!(9_007_199_254_740_992u64), "¥ 500 ~"),
    ] {
        c.put(
            "product",
            id,
            json!({"id":id,"title":id,"price":price,"listingPriceText":listing}),
        )
        .unwrap();
    }
    for (id, price) in [
        ("exact", 1200),
        ("zero", 0),
        ("known", 700),
        ("fraction-fallback", 800),
        ("negative-fallback", 0),
    ] {
        let mut source = c.get("product", id).unwrap().unwrap();
        source["publicVariants"] =
            json!([{"id":"owned","name":"Owned","price":price,"currency":"JPY"}]);
        c.put("product", id, source).unwrap();
        c.put("acquisition", &format!("owned-{id}"),json!({"id":format!("owned-{id}"),"productId":id,"accountId":"a","variantIds":["owned"]})).unwrap();
    }
    c.put("acquisition","retained",json!({"id":"retained","productId":"exact","accountId":"a","source":"purchase","sourceRecordId":"unchanged-record","acquiredDateText":"2026/2/1 10:20:30","paidAmountText":null})).unwrap();
    c.command(
        "overlay",
        json!({"ids":["exact"],"patch":{"title":"Human title","note":"Keep note"}}),
    )
    .unwrap();
    let raw_products = c.list("product").unwrap();
    let raw_acquisitions = c.list("acquisition").unwrap();
    let before = Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap();
    drop(c);
    let c = f.open();
    assert_eq!(c.list("product").unwrap(), raw_products);
    assert_eq!(c.list("acquisition").unwrap(), raw_acquisitions);
    let after = Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap();
    assert_eq!(after.1, before.1);
    assert_eq!(after.0 .0, before.0 .0);
    assert_eq!(after.0 .1, before.0 .1);
    let exact = dynamic_details(&c, "exact");
    assert_eq!(exact["price"], 1200.0);
    assert_eq!(exact["acquired"], "2026/2/1 10:20:30");
    assert_eq!(exact["title"], "Human title");
    assert_eq!(exact["note"], "Keep note");
    assert!(exact["paidPrice"].is_null());
    for id in ["range", "closed", "unsafe-range"] {
        let p = dynamic_details(&c, id);
        assert!(p["price"].is_null());
        assert!(p["currentPriceText"].is_null());
        assert_eq!(p["listingStartingPrice"], 500.0);
        assert!(!text(&c.get("product", id).unwrap().unwrap(), "listingPriceText").is_empty());
    }
    for id in ["fraction", "negative", "unsafe"] {
        let p = dynamic_details(&c, id);
        assert!(p["price"].is_null());
        assert!(p["currentPriceText"].is_null());
        let sql_price: Option<f64> = c
            .inner
            .lock()
            .unwrap()
            .query_row("SELECT price FROM products WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(sql_price.is_none());
    }
    for (id, expected) in [
        ("zero", 0.0),
        ("known", 700.0),
        ("fraction-fallback", 800.0),
        ("negative-fallback", 0.0),
    ] {
        assert_eq!(dynamic_details(&c, id)["price"].as_f64(), Some(expected));
        let sql_price: f64 = c
            .inner
            .lock()
            .unwrap()
            .query_row("SELECT price FROM products WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(sql_price, expected);
    }
}

#[test]
fn acquired_dates_sort_chronologically_preserving_provider_text() {
    let f = Fixture::new();
    let c = f.open();
    for id in ["1", "2", "3"] {
        product(&c, id);
    }
    for (id, date) in [
        ("1", "2026年2月1日"),
        ("2", "2026年10月1日"),
        ("3", "not a date"),
    ] {
        c.put(
            "acquisition",
            id,
            json!({"id":id,"productId":id,"source":"purchase","acquiredDateText":date}),
        )
        .unwrap();
    }
    assert_eq!(
        c.command("query", json!({"sort":"acquired","direction":"desc"}))
            .unwrap()["matchedIds"],
        json!(["2", "1", "3"])
    );
    assert_eq!(
        c.command("query", json!({"sort":"acquired","direction":"asc"}))
            .unwrap()["matchedIds"],
        json!(["1", "2", "3"])
    );
    assert_eq!(projection(&c, "1")["acquired"], "2026年2月1日");
    c.put(
        "acquisition",
        "earlier",
        json!({"id":"earlier","productId":"2","source":"gift","acquiredDateText":"2026年2月1日"}),
    )
    .unwrap();
    assert_eq!(projection(&c, "2")["acquired"], "2026年2月1日");
}
#[test]
fn backup_restore_keeps_concurrent_human_alternative() {
    let a = Fixture::new();
    let b = Fixture::new();
    let d = Fixture::new();
    let folder = Fixture::new();
    let ca = a.open();
    let cb = b.open();
    product(&ca, "1");
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    ca.command("overlay", json!({"ids":["1"],"patch":{"note":"Human A"}}))
        .unwrap();
    cb.command("overlay", json!({"ids":["1"],"patch":{"note":"Human B"}}))
        .unwrap();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    let old = ca.command("conflicts", json!({})).unwrap();
    assert!(old.as_array().unwrap().iter().any(|v| v["field"] == "note"));
    let saved = ca.manifest().unwrap();
    let restored = d.open();
    restored.restore_manifest(saved).unwrap();
    let conflicts = restored.command("conflicts", json!({})).unwrap();
    let notes = conflicts
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["field"] == "note")
        .unwrap();
    let values = notes["values"].as_array().unwrap();
    assert!(values.iter().any(|v| v["value"]["value"] == "Human A"));
    assert!(values.iter().any(|v| v["value"]["value"] == "Human B"));
    let alt = values
        .iter()
        .find(|v| v["value"]["value"] == "Human B")
        .unwrap();
    restored
        .command(
            "resolve_conflict",
            json!({"kind":"overlay","id":"1","field":"note","opId":alt["opId"]}),
        )
        .unwrap();
    assert_eq!(projection(&restored, "1")["note"], "Human B");
}
#[test]
fn no_op_exchange_does_not_invalidate_and_lite_keeps_summary() {
    let f = Fixture::new();
    let folder = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    let revision = c.revision().unwrap();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(c.revision().unwrap(), revision);
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(c.revision().unwrap(), revision);
    let lite = c.command("query", json!({"includeFiles":false})).unwrap();
    assert_eq!(lite["products"][0]["filesLoaded"], false);
    assert_eq!(
        lite["products"][0]["status"],
        c.command("product_details", json!({"id":"1"})).unwrap()["status"]
    );
}

#[test]
fn acquired_subsecond_sort_key_is_fixed_precision() {
    let f = Fixture::new();
    let c = f.open();
    for (id, date) in [
        ("1", "2026-01-01T00:00:00Z"),
        ("2", "2026-01-01T00:00:00.500Z"),
        ("3", "2026年1月1日"),
    ] {
        product(&c, id);
        c.put(
            "acquisition",
            id,
            json!({"id":id,"productId":id,"source":"purchase","acquiredDateText":date}),
        )
        .unwrap();
    }
    assert_eq!(
        c.command("query", json!({"sort":"acquired","direction":"desc"}))
            .unwrap()["matchedIds"],
        json!(["2", "1", "3"])
    );
}

#[test]
fn required_manifest_membership_includes_every_current_grant() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.put(
        "remote_file",
        "f",
        json!({"id":"f","productId":"1","accountId":"a","name":"test.zip","revision":"r"}),
    )
    .unwrap();
    c.put(
        "entitlement",
        "a",
        json!({"id":"a","productId":"1","accountId":"a","fileIds":["f"],"manifestComplete":true}),
    )
    .unwrap();
    c.put(
        "entitlement",
        "b",
        json!({"id":"b","productId":"1","accountId":"b","fileIds":[],"manifestComplete":false}),
    )
    .unwrap();
    assert_eq!(c.effective_entitlements("1").unwrap().len(), 2);
    assert_eq!(projection(&c, "1")["manifestComplete"], false);
    c.put(
        "entitlement",
        "b",
        json!({"id":"b","productId":"1","accountId":"b","fileIds":["f"],"manifestComplete":true}),
    )
    .unwrap();
    assert_eq!(projection(&c, "1")["manifestComplete"], true);
}
#[test]
fn undo_refuses_a_concurrent_alternative_even_when_its_value_wins() {
    let a = Fixture::new();
    let b = Fixture::new();
    let folder = Fixture::new();
    let ca = a.open();
    let cb = b.open();
    product(&ca, "1");
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    let ta = ca
        .command("overlay", json!({"ids":["1"],"patch":{"note":"A"}}))
        .unwrap();
    let tb = cb
        .command("overlay", json!({"ids":["1"],"patch":{"note":"B"}}))
        .unwrap();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    cb.command("exchange", json!({"folder":folder.0})).unwrap();
    ca.command("exchange", json!({"folder":folder.0})).unwrap();
    let winner = projection(&ca, "1")["note"].clone();
    let result = if winner == "A" {
        ca.command("undo", json!({"token":ta["token"]}))
    } else {
        cb.command("undo", json!({"token":tb["token"]}))
    }
    .unwrap();
    assert_eq!(result["changed"], 0);
    assert_eq!(result["conflicts"], json!(["1"]));
    assert_eq!(projection(&ca, "1")["note"], winner);
    assert_eq!(projection(&cb, "1")["note"], winner);
}

#[test]
fn cached_exchange_still_rejects_corruption_of_known_history() {
    let f = Fixture::new();
    let folder = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    let change = std::fs::read_dir(&folder.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "amchange"))
        .unwrap();
    let original = std::fs::read(&change).unwrap();
    let mut corrupt = original.clone();
    corrupt.push(0);
    std::fs::write(&change, corrupt).unwrap();
    let error = c
        .command("exchange", json!({"folder":folder.0}))
        .unwrap_err();
    assert!(error.to_string().contains("checksum"));
    std::fs::write(&change, original).unwrap();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    assert_eq!(projection(&c, "1")["title"], "Synthetic ＡＢＣ 髪型");
}

#[test]
fn conditional_batch_preserves_intervening_writes_and_negative_reads() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    let initial = c.get("product", "1").unwrap();
    let newer = json!({"id":"1","title":"Live source refresh"});
    c.put("product", "1", newer.clone()).unwrap();
    let revision = c.revision().unwrap();
    let attempted = vec![(
        "product".into(),
        "1".into(),
        json!({"id":"1","title":"Stale import"}),
    )];
    assert!(!c
        .put_many_if_unchanged(
            attempted.clone(),
            vec![("product".into(), "1".into(), initial)]
        )
        .unwrap());
    assert_eq!(c.revision().unwrap(), revision);
    assert_eq!(c.get("product", "1").unwrap(), Some(newer.clone()));
    product(&c, "2");
    let revision = c.revision().unwrap();
    let chunks: i64 = c
        .inner
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM am_changes", [], |r| r.get(0))
        .unwrap();
    assert!(!c
        .put_many_if_unchanged(
            attempted.clone(),
            vec![
                ("product".into(), "1".into(), Some(newer.clone())),
                ("product".into(), "2".into(), None)
            ]
        )
        .unwrap());
    assert_eq!(c.revision().unwrap(), revision);
    assert_eq!(
        c.inner
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM am_changes", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        chunks
    );
    assert_eq!(c.get("product", "1").unwrap(), Some(newer.clone()));
    assert!(c
        .put_many_if_unchanged(attempted, vec![("product".into(), "1".into(), Some(newer))])
        .unwrap());
    assert_eq!(
        c.get("product", "1").unwrap().unwrap()["title"],
        "Stale import"
    );
}

#[test]
fn restart_rejects_corrupt_truncated_or_missing_middle_history() {
    for mode in ["corrupt", "truncate", "missing"] {
        let f = Fixture::new();
        let c = f.open();
        product(&c, "1");
        c.flush_sync_outbox().unwrap();
        c.command("overlay", json!({"ids":["1"],"patch":{"note":"Middle"}}))
            .unwrap();
        c.flush_sync_outbox().unwrap();
        c.command(
            "overlay",
            json!({"ids":["1"],"patch":{"title":"Latest projection"}}),
        )
        .unwrap();
        c.flush_sync_outbox().unwrap();
        {
            let conn = c.inner.lock().unwrap();
            let (seq, body): (i64, Vec<u8>) = conn
                .query_row(
                    "SELECT seq,body FROM am_changes ORDER BY seq LIMIT 1 OFFSET 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            if mode == "missing" {
                conn.execute("DELETE FROM am_changes WHERE seq=?1", [seq])
                    .unwrap();
            } else {
                let corrupt = if mode == "truncate" {
                    body[..body.len() / 2].to_vec()
                } else {
                    b"invalid middle history".to_vec()
                };
                conn.execute(
                    "UPDATE am_changes SET body=?1 WHERE seq=?2",
                    params![corrupt, seq],
                )
                .unwrap();
            }
        }
        drop(c);
        let reopened = f.open();
        assert!(reopened.ensure_document().is_err(), "{mode}");
        // Broken sync history cannot destroy or block independent local edits.
        let revision = reopened.revision().unwrap();
        reopened
            .command(
                "overlay",
                json!({"ids":["1"],"patch":{"note":"Durable local edit"}}),
            )
            .unwrap();
        assert!(reopened.revision().unwrap() > revision);
        let local = reopened.get("overlay", "1").unwrap().unwrap();
        assert_eq!(local["title"], "Latest projection");
        assert_eq!(local["note"], "Durable local edit");
        assert!(reopened.prepare_sync_document().is_err());
        assert_eq!(
            reopened
                .inner
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM sync_outbox", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn concurrent_cold_preparation_loads_once_without_blocking_reads() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Duration;

    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.flush_sync_outbox().unwrap();
    drop(c);
    let c = f.open();
    let before = Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let (validated_tx, validated_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let owner = c.clone();
    let owner_loads = loads.clone();
    let owner = std::thread::spawn(move || {
        owner.prepare_document(
            || {},
            |bytes, actor| {
                let doc = sync::load_doc(bytes, actor)?;
                owner_loads.fetch_add(1, Ordering::SeqCst);
                validated_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                Ok(doc)
            },
        )
    });
    validated_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let (waiting_tx, waiting_rx) = mpsc::channel();
    let mut waiters = vec![];
    for _ in 0..4 {
        let waiter = c.clone();
        let waiting_tx = waiting_tx.clone();
        let loads = loads.clone();
        waiters.push(std::thread::spawn(move || {
            waiter.prepare_document(
                || waiting_tx.send(()).unwrap(),
                |bytes, actor| {
                    let doc = sync::load_doc(bytes, actor)?;
                    loads.fetch_add(1, Ordering::SeqCst);
                    Ok(doc)
                },
            )
        }));
    }
    // Every competitor has observed the cold cache before the owner installs.
    let all_waiting = (0..4).all(|_| waiting_rx.recv_timeout(Duration::from_secs(5)).is_ok());
    let reader = c.clone();
    let (read_tx, read_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        read_tx.send(reader.get("product", "1")).unwrap();
    });
    let read = read_rx.recv_timeout(Duration::from_secs(5));
    // Release workers before assertions, including on a lock regression.
    resume_tx.send(()).unwrap();
    assert!(all_waiting, "cold waiters must not hold SQLite/cache locks");
    assert_eq!(read.unwrap().unwrap().unwrap()["id"], "1");
    owner.join().unwrap().unwrap();
    for waiter in waiters {
        waiter.join().unwrap().unwrap();
    }
    reader.join().unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(
        Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap(),
        before,
        "preparation must not alter durable history or revisions"
    );

    // A warm caller must complete even while the preparation gate is occupied.
    let gate = c.document_preparation.lock().unwrap();
    let warm = c.clone();
    let (warm_tx, warm_rx) = mpsc::channel();
    let warm = std::thread::spawn(move || {
        warm_tx
            .send(warm.prepare_document(
                || panic!("warm cache must bypass preparation"),
                |_, _| panic!("warm cache must not reload history"),
            ))
            .unwrap();
    });
    let result = warm_rx.recv_timeout(Duration::from_secs(5));
    drop(gate);
    warm.join().unwrap();
    assert_eq!(result.unwrap().unwrap(), (0, 0));
}

#[test]
fn cold_restart_small_write_persists_only_new_delta_and_reaches_peer() {
    let f = Fixture::new();
    let c = f.open();
    c.put_many(
        (0..32)
            .map(|i| {
                let id = format!("history-{i}");
                (
                    "product".into(),
                    id.clone(),
                    json!({"id":id,"title":format!("Synthetic retained product {i}"),"sourceTags":["Synthetic"]}),
                )
            })
            .collect(),
    )
    .unwrap();
    c.flush_sync_outbox().unwrap();
    drop(c);

    let c = f.open();
    let history = |c: &Catalogue| -> Vec<(i64, Vec<u8>)> {
        let conn = c.inner.lock().unwrap();
        let mut statement = conn
            .prepare("SELECT seq,body FROM am_changes ORDER BY seq")
            .unwrap();
        let chunks = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        chunks
    };
    let before_chunks = history(&c);
    let before = Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap();
    c.ensure_document().unwrap();
    assert_eq!(
        Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap(),
        before
    );
    let before_heads = c
        .document_cache
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .get_heads();
    c.command(
        "collection",
        json!({"operation":"create","id":"cold-edit","value":{"name":"Synthetic cold edit","productIds":["history-0"]}}),
    )
    .unwrap();
    c.flush_sync_outbox().unwrap();
    let after_chunks = history(&c);
    assert_eq!(after_chunks.len(), before_chunks.len() + 1);
    assert_eq!(
        &after_chunks[..before_chunks.len()],
        before_chunks.as_slice()
    );
    let delta = &after_chunks.last().unwrap().1;
    assert!(
        delta.len() < before.1.len(),
        "a small edit must not append the full baseline history"
    );
    // Exactly one new change depends on the prior heads. A full-history append
    // would contain extra changes and cannot decode as this single change.
    let change = automerge::Change::from_bytes(delta.clone()).unwrap();
    assert_eq!(change.deps(), before_heads.as_slice());
    assert!(!before_heads.contains(&change.hash()));
    drop(c);

    let c = f.open();
    c.ensure_document().unwrap();
    let expected = sync::entities(c.document_cache.lock().unwrap().as_ref().unwrap()).unwrap();
    let collection = expected
        .iter()
        .find(|(kind, id, _)| kind == "collection" && id == "cold-edit")
        .unwrap();
    assert_eq!(collection.2["name"], "Synthetic cold edit");
    assert_eq!(collection.2["productIds"], json!(["history-0"]));
    assert_eq!(
        expected
            .iter()
            .filter(|(kind, _, _)| kind == "product")
            .count(),
        32
    );
    let folder = Fixture::new();
    let peer_fixture = Fixture::new();
    let peer = peer_fixture.open();
    c.command("exchange", json!({"folder":folder.0})).unwrap();
    peer.command("exchange", json!({"folder":folder.0}))
        .unwrap();
    assert_eq!(
        sync::entities(peer.document_cache.lock().unwrap().as_ref().unwrap()).unwrap(),
        expected
    );
    drop(peer);
    let peer = peer_fixture.open();
    peer.ensure_document().unwrap();
    assert_eq!(
        sync::entities(peer.document_cache.lock().unwrap().as_ref().unwrap()).unwrap(),
        expected
    );
}

#[test]
fn delayed_cold_document_cannot_replace_an_intervening_writer() {
    let f = Fixture::new();
    let c = f.open();
    product(&c, "1");
    c.flush_sync_outbox().unwrap();
    let (version, bytes, _) = Catalogue::document_snapshot(&c.inner.lock().unwrap()).unwrap();
    let mut delayed = sync::load_doc(&bytes, &version.0).unwrap();
    let _ = delayed.save_incremental();
    c.command(
        "overlay",
        json!({"ids":["1"],"patch":{"note":"Newer writer"}}),
    )
    .unwrap();
    c.flush_sync_outbox().unwrap();
    // Simulate the rollback/invalidation interleaving while an older decoder
    // finishes. A changed durable generation must reject its installation.
    *c.document_cache.lock().unwrap() = None;
    assert!(!c.install_document(&version, delayed, false).unwrap());
    c.ensure_document().unwrap();
    let saved = c.manifest().unwrap();
    let restored = Fixture::new();
    let other = restored.open();
    other.restore_manifest(saved).unwrap();
    assert_eq!(projection(&other, "1")["note"], "Newer writer");
}

#[test]
fn related_lookup_plan_bounds_product_and_preserves_order() {
    let f = Fixture::new();
    let c = f.open();
    let conn = c.inner.lock().unwrap();
    for (id, product) in [("z", "1"), ("a", "1"), ("other", "2")] {
        set_raw(
            &conn,
            "remote_file",
            id,
            &json!({"id":id,"productId":product,"name":"Synthetic.unitypackage"}),
        )
        .unwrap();
    }
    let details: Vec<String> = conn.prepare("EXPLAIN QUERY PLAN SELECT body FROM entities WHERE kind=?1 AND product_id=?2 ORDER BY id").unwrap()
        .query_map(params!["remote_file","1"], |r| r.get(3)).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("entities_product_order")
                && detail.contains("kind=? AND product_id=?")),
        "{details:?}"
    );
    assert!(
        !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
        "{details:?}"
    );
    let found = related(&conn, "remote_file", "1").unwrap();
    assert_eq!(
        found.iter().map(|v| text(v, "id")).collect::<Vec<_>>(),
        ["a", "z"]
    );
}

#[test]
fn unnamed_public_variant_price_requires_exact_owned_variant_identity() {
    let fixture = Fixture::new();
    let c = fixture.open();
    for (id, owned) in [
        ("matched-unnamed", Some("800001")),
        ("different-unnamed", Some("800002")),
        ("unowned-unnamed", None),
    ] {
        c.put("product", id, json!({"id":id,"store":"BOOTH","sourceTitle":"Authored clothing","sourceCategory":{"name":"3D衣装"},"sourceTags":["VRChat"],"price":6000,"listingPriceText":"¥ 6000","publicVariants":[{"id":"800001","type":"digital","price":6000,"currency":"JPY"}]})).unwrap();
        if let Some(variant) = owned {
            let file_id = format!("{id}-file");
            let acquisition_id = format!("{id}-order");
            c.put("remote_file", &file_id, json!({"id":file_id,"productId":id,"accountId":"a","variantId":variant,"name":"authored.zip","revision":"r1"})).unwrap();
            c.put(
                "acquisition",
                &acquisition_id,
                json!({"id":acquisition_id,"productId":id,"accountId":"a","variantIds":[variant]}),
            )
            .unwrap();
        }
    }
    let matched = dynamic_details(&c, "matched-unnamed");
    assert_eq!(matched["price"], 6000.0);
    assert_eq!(matched["priceCurrency"], "JPY");
    assert_eq!(matched["currentPriceText"], "¥ 6000");
    assert_eq!(matched["ownedCurrentPrices"].as_array().unwrap().len(), 1);
    assert_eq!(matched["ownedCurrentPrices"][0]["variantId"], "800001");
    assert_eq!(matched["ownedCurrentPrices"][0]["price"], 6000.0);
    assert!(matched["paidPrice"].is_null());
    let different = dynamic_details(&c, "different-unnamed");
    assert!(different["price"].is_null());
    assert!(different["currentPriceText"].is_null());
    assert_eq!(different["listingStartingPrice"], 6000.0);
    assert_eq!(different["ownedCurrentPrices"][0]["variantId"], "800002");
    assert!(different["ownedCurrentPrices"][0]["price"].is_null());
    let unowned = dynamic_details(&c, "unowned-unnamed");
    assert!(unowned["price"].is_null());
    assert_eq!(unowned["listingStartingPrice"], 6000.0);
    assert!(unowned["ownedCurrentPrices"].as_array().unwrap().is_empty());
}

#[test]
fn multi_provider_refresh_unlink_restart_preserves_grants_money_and_manual_tags() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put(
        "root",
        "authored-root",
        json!({"id":"authored-root","path":fixture.0,"status":"online"}),
    )
    .unwrap();
    let providers = [
        ("booth", "BOOTH", "authored-booth", "JPY"),
        ("gumroad", "Gumroad", "gumroad:authored", "USD"),
        ("jinxxy", "Jinxxy", "jinxxy:authored", "EUR"),
    ];
    for (provider, store, id, currency) in providers {
        let source = json!({"id":id,"provider":provider,"store":store,"sourceTitle":"Authored source Hair","sourceCategory":{"name":"Hair"},"sourceTags":["Source tag","Removed tag"],"price":if provider=="booth"{1.0}else{22.5},"priceCurrency":currency,"listingPriceText":"¥ 1","publicVariants":[{"id":"owned-a","name":"Owned A","type":"digital","price":1100,"currency":"JPY"},{"id":"owned-b","name":"Owned B","type":"digital","price":2200,"currency":"JPY"},{"id":"unowned-cheap","name":"Unowned minimum","type":"digital","price":1,"currency":"JPY"}]});
        c.put("product", id, source).unwrap();
        for (suffix, paid, date) in [("a", 8.0, "2026/10/01"), ("b", 16.0, "2026/10/02")] {
            let account = format!("{provider}-{suffix}");
            let file_id = format!("{provider}-file-{suffix}");
            let entitlement_id = format!("{provider}-grant-{suffix}");
            let acquisition_id = format!("{provider}-purchase-{suffix}");
            let variant = format!("owned-{suffix}");
            let relative = format!("{provider}/Authored-{suffix}.unitypackage");
            let bytes = format!("Independently authored {provider} {suffix} content");
            let path = fixture.0.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            c.put_many(vec![
                ("account".into(),account.clone(),json!({"id":account,"provider":provider,"store":store,"identityVerified":true,"name":format!("Authored {suffix}")})),
                ("remote_file".into(),file_id.clone(),json!({"id":file_id,"productId":id,"accountId":account,"provider":provider,"sourceFileId":format!("source-{suffix}"),"variantId":variant,"name":format!("Authored-{suffix}.unitypackage"),"revision":"r1","sizeBytes":bytes.len(),"requiresContentVerification":false})),
                ("entitlement".into(),entitlement_id.clone(),json!({"id":entitlement_id,"productId":id,"accountId":account,"provider":provider,"variantIds":[variant],"fileIds":[file_id],"manifestComplete":true})),
                ("acquisition".into(),acquisition_id.clone(),json!({"id":acquisition_id,"productId":id,"accountId":account,"provider":provider,"source":"purchase","variantIds":[variant],"acquiredDateText":date,"paidPrice":paid,"paidCurrency":currency,"paidPriceBasis":"productLineTotal","variantPayments":[{"variantId":variant,"name":format!("Owned {suffix}"),"price":paid,"currency":currency,"quantity":1}]})),
                ("artifact".into(),file_id.clone(),json!({"id":file_id,"fileId":file_id,"productId":id,"revision":"r1","rootId":"authored-root","status":"ready","hash":format!("{:x}",Sha256::digest(bytes.as_bytes())),"sizeBytes":bytes.len(),"relativePath":relative,"originalAvailable":true})),
            ]).unwrap();
        }
        c.command("overlay", json!({"ids":[id],"patch":{"title":format!("Human label {provider}"),"category":"Accessory","note":"Persistent human note","tags":["Manual retained","Manual second"],"rejectedTags":["Removed tag"],"thumbnailHidden":true}})).unwrap();
        let initial = dynamic_details(&c, id);
        assert_eq!(initial["status"], "ready", "{provider}");
        assert_eq!(initial["total"], 2);
        assert!(
            initial["paidPrice"].is_null(),
            "Two distinct paid amounts must not become a guessed scalar"
        );
        if provider == "booth" {
            assert!(initial["price"].is_null());
            assert_eq!(initial["ownedCurrentPrices"].as_array().unwrap().len(), 2);
            assert!(!initial["ownedCurrentPrices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|quote| quote["variantId"] == "unowned-cheap"));
        } else {
            assert_eq!(initial["price"], 22.5);
            assert_eq!(initial["priceCurrency"], currency);
        }
        for (suffix, amount) in [("a", 8.0), ("b", 16.0)] {
            let scoped = c
                .command(
                    "product_details",
                    json!({"id":id,"accountIds":[format!("{provider}-{suffix}")]}),
                )
                .unwrap();
            assert_eq!(scoped["paidPrice"], amount);
            assert_eq!(scoped["paidCurrency"], currency);
            if provider == "booth" {
                assert_eq!(
                    scoped["ownedCurrentPrices"][0]["variantId"],
                    format!("owned-{suffix}")
                );
                assert_eq!(scoped["price"], if suffix == "a" { 1100.0 } else { 2200.0 });
                assert_eq!(scoped["ownedPaidPrices"][0]["price"], amount);
            }
        }
    }
    // A real source change must invalidate the old artifact until the new bytes
    // and matching descriptor are committed; a complete second grant remains usable.
    for (provider, _, id, _) in providers {
        let mut source = c.get("product", id).unwrap().unwrap();
        source["sourceTitle"] = json!("Updated source Clothes");
        source["sourceCategory"] = json!({"name":"Clothes"});
        source["sourceTags"] = json!(["New source tag", "Removed tag"]);
        source["price"] = json!(if provider == "booth" { 1.0 } else { 27.5 });
        source["publicVariants"][0]["price"] = json!(1400);
        source["publicVariants"][1]["price"] = json!(2600);
        let file_id = format!("{provider}-file-a");
        let mut changed_file = c.get("remote_file", &file_id).unwrap().unwrap();
        changed_file["revision"] = json!("r2");
        changed_file["requiresContentVerification"] = json!(true);
        c.put_many(vec![
            ("product".into(), id.into(), source),
            ("remote_file".into(), file_id.clone(), changed_file.clone()),
        ])
        .unwrap();
        let pending = dynamic_details(&c, id);
        assert_eq!(pending["status"], "partial", "{provider}");
        assert_eq!(pending["downloaded"], 1);
        assert_eq!(pending["manifestComplete"], true);
        let mut verified = c.get("artifact", &file_id).unwrap().unwrap();
        let bytes = format!("Independently authored refreshed {provider} content");
        std::fs::write(fixture.0.join(text(&verified, "relativePath")), &bytes).unwrap();
        verified["revision"] = json!("r2");
        verified["hash"] = json!(format!("{:x}", Sha256::digest(bytes.as_bytes())));
        verified["sizeBytes"] = json!(bytes.len());
        changed_file["sizeBytes"] = json!(bytes.len());
        changed_file["requiresContentVerification"] = json!(false);
        c.put_many(vec![
            ("remote_file".into(), file_id.clone(), changed_file),
            ("artifact".into(), file_id, verified),
        ])
        .unwrap();
        assert_eq!(dynamic_details(&c, id)["status"], "ready", "{provider}");
    }
    for (provider, _, id, currency) in providers {
        let account_a = format!("{provider}-a");
        let account_b = format!("{provider}-b");
        let file_b = format!("{provider}-file-b");
        let retained = c.get("remote_file", &file_b).unwrap();
        let other_products: Vec<_> = providers
            .iter()
            .filter(|(_, _, other, _)| *other != id)
            .map(|(_, _, other, _)| ((*other).to_owned(), dynamic_details(&c, other)))
            .collect();
        assert_eq!(
            c.remove_account(&account_a, true).unwrap(),
            json!({"accountRemoved":1,"productRemoved":0,"retainedShared":1})
        );
        assert!(c
            .get("acquisition", &format!("{provider}-purchase-a"))
            .unwrap()
            .is_none());
        assert!(c
            .get("entitlement", &format!("{provider}-grant-a"))
            .unwrap()
            .is_none());
        assert!(c
            .get("remote_file", &format!("{provider}-file-a"))
            .unwrap()
            .is_none());
        assert_eq!(c.get("remote_file", &file_b).unwrap(), retained);
        for (other, expected) in other_products {
            assert_eq!(
                dynamic_details(&c, &other),
                expected,
                "Unlinking {provider} changed another provider"
            );
        }
        let kept = dynamic_details(&c, id);
        assert_eq!(kept["accountIds"], json!([account_b]));
        assert_eq!(kept["total"], 1);
        assert_eq!(kept["downloaded"], 1);
        assert_eq!(kept["status"], "ready");
        assert_eq!(kept["files"][0]["id"], file_b);
        assert_eq!(kept["files"][0]["provenance"][0]["accountId"], account_b);
        assert_eq!(kept["paidPrice"], 16.0);
        assert_eq!(kept["paidCurrency"], currency);
        assert_eq!(kept["acquired"], "2026/10/02");
        assert_eq!(
            kept["price"],
            if provider == "booth" { 2600.0 } else { 27.5 }
        );
        if provider == "booth" {
            assert_eq!(kept["ownedCurrentPrices"].as_array().unwrap().len(), 1);
            assert_eq!(kept["ownedCurrentPrices"][0]["variantId"], "owned-b");
            assert_eq!(kept["ownedPaidPrices"].as_array().unwrap().len(), 1);
            assert_eq!(kept["ownedPaidPrices"][0]["price"], 16.0);
        }
    }
    let expected: Vec<_> = providers
        .iter()
        .map(|(_, _, id, _)| ((*id).to_owned(), dynamic_details(&c, id)))
        .collect();
    drop(c);
    let reopened = fixture.open();
    let list = reopened
        .command("query", json!({"includeFiles":false,"presentation":"list"}))
        .unwrap();
    assert_eq!(list["total"], 3);
    assert_eq!(list["allDownloaded"], true);
    for (id, expected) in &expected {
        let details = dynamic_details(&reopened, id);
        assert_eq!(&details, expected);
        let summary = list["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id.as_str())
            .unwrap();
        for field in [
            "title",
            "category",
            "note",
            "tags",
            "rejectedTags",
            "thumbnailHidden",
            "accountIds",
            "price",
            "priceCurrency",
            "paidPrice",
            "paidCurrency",
            "acquired",
            "status",
            "fullyDownloaded",
        ] {
            assert_eq!(
                summary[field], details[field],
                "Reopened list/detail {id}/{field}"
            );
        }
        assert_eq!(
            details["title"],
            format!("Human label {}", text(&details, "provider"))
        );
        assert_eq!(details["category"], "Accessory");
        assert_eq!(details["note"], "Persistent human note");
        assert_eq!(details["tags"], json!(["Manual retained", "Manual second"]));
        assert_eq!(
            details["rejectedTags"],
            json!(["Removed tag", "Source tag"])
        );
        assert_eq!(details["thumbnailHidden"], true);
    }
    // A missing owned quote stays unknown in exact-price fields. BOOTH exposes
    // the independently labelled listing start without changing paid facts.
    for (provider, _, id, _) in providers {
        let mut source = reopened.get("product", id).unwrap().unwrap();
        source["price"] = Value::Null;
        source["publicVariants"] = json!([{"id":"unowned-cheap","name":"Unowned minimum","type":"digital","price":1,"currency":"JPY"}]);
        reopened.put("product", id, source).unwrap();
        let unknown = dynamic_details(&reopened, id);
        assert!(
            unknown["price"].is_null(),
            "{provider} must not borrow a listing minimum"
        );
        assert!(unknown["currentPriceText"].is_null());
        assert_eq!(unknown["paidPrice"], 16.0);
        assert_eq!(unknown["status"], "ready");
        assert_eq!(unknown["tags"], json!(["Manual retained", "Manual second"]));
        if provider == "booth" {
            assert!(unknown["ownedCurrentPrices"][0]["price"].is_null());
            assert_eq!(unknown["listingStartingPrice"], 1.0);
        }
    }
    drop(reopened);
    let unknown_restart = fixture.open();
    for (_, _, id, currency) in providers {
        let p = dynamic_details(&unknown_restart, id);
        assert!(p["price"].is_null());
        assert!(p["currentPriceText"].is_null());
        assert_eq!(p["paidPrice"], 16.0);
        assert_eq!(p["paidCurrency"], currency);
        assert_eq!(p["status"], "ready");
    }
}

#[test]
fn public_variants_and_account_fullpack_remain_distinct_under_filters_and_overrides() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let source = json!({"id":"900001","store":"BOOTH","sourceTitle":"Authored VRChat clothing","sourceCategory":{"name":"3D衣装"},"sourceTags":["VRChat"],"publicVariants":[
        {"id":"900010","type":"digital","name":"全部入りフルパック","price":3300,"currency":"JPY"},
        {"id":"900011","type":"digital","name":"しなの専用"},
        {"id":"900012","type":"digital","name":"マヌカ専用","price":800,"currency":"JPY"},
        {"id":"900013","type":"digital","name":"Lapwing専用"},
        {"id":"900014","type":"digital","name":"マリシア専用"}]});
    let a_file = json!({"id":"a-file","productId":"900001","accountId":"a","variantId":"900010","name":"opaque-bundle.zip","revision":"r1"});
    let b_file = json!({"id":"b-file","productId":"900001","accountId":"b","variantId":"900012","variantName":"マヌカ専用","name":"opaque-single.zip","revision":"r1"});
    c.put_many(vec![
        ("product".into(),"900001".into(),source.clone()),
        ("account".into(),"a".into(),json!({"id":"a","store":"BOOTH"})),
        ("account".into(),"b".into(),json!({"id":"b","store":"BOOTH"})),
        ("remote_file".into(),"a-file".into(),a_file.clone()),
        ("remote_file".into(),"b-file".into(),b_file.clone()),
        ("entitlement".into(),"a-ent".into(),json!({"id":"a-ent","productId":"900001","accountId":"a","fileIds":["a-file"],"manifestComplete":true})),
        ("entitlement".into(),"b-ent".into(),json!({"id":"b-ent","productId":"900001","accountId":"b","fileIds":["b-file"],"manifestComplete":true}))
    ]).unwrap();
    c.put("acquisition", "a-order", json!({"id":"a-order","productId":"900001","accountId":"a","variantIds":["900010"],"paidPrice":3000,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal","variantPayments":[{"variantId":"900010","name":"全部入りフルパック","price":3000,"currency":"JPY","quantity":1}]})).unwrap();
    // The aggregate price across two variants is not a per-variant payment.
    c.put("acquisition", "b-order", json!({"id":"b-order","productId":"900001","accountId":"b","variantIds":["900012","900013"],"paidPrice":1600,"paidCurrency":"JPY","paidPriceBasis":"productLineTotal"})).unwrap();
    c.put("acquisition", "b-unproven", json!({"id":"b-unproven","productId":"900001","accountId":"b","variantIds":["900012"],"paidPrice":700,"paidCurrency":"JPY","paidPriceBasis":"unproven","variantPayments":[{"variantId":"900012","name":"マヌカ専用","price":700,"currency":"JPY"}]})).unwrap();
    let details = dynamic_details(&c, "900001");
    assert!(details["price"].is_null());
    assert_eq!(details["ownedCurrentPrices"].as_array().unwrap().len(), 3);
    assert_eq!(details["ownedPaidPrices"].as_array().unwrap().len(), 3);
    assert_eq!(details["ownedPaidPrices"][0]["price"], 3000.0);
    let scoped = c
        .command(
            "query",
            json!({"accountIds":["a"],"includeFiles":false,"presentation":"list"}),
        )
        .unwrap();
    assert_eq!(scoped["products"][0]["price"], 3300.0);
    assert_eq!(
        scoped["products"][0]["ownedCurrentPrices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        scoped["products"][0]["ownedPaidPrices"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let scoped = c
        .command("product_details", json!({"id":"900001","accountIds":["b"]}))
        .unwrap();
    assert!(scoped["price"].is_null());
    assert_eq!(scoped["ownedPaidPrices"].as_array().unwrap().len(), 2);
    assert!(scoped["ownedPaidPrices"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["price"].is_null() && row["currency"].is_null()));
    assert!(scoped["ownedCurrentPrices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["variantId"] == "900013" && v["price"].is_null()));
    assert_eq!(strings(&details["listingAvatars"]).len(), 4);
    assert_eq!(strings(&details["ownedAvatars"]).len(), 4);
    let account_a = details["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["accountId"] == "a")
        .count();
    let account_b = details["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["accountId"] == "b")
        .count();
    assert_eq!(account_a, 4);
    assert_eq!(account_b, 1);
    for (scope, matched) in [
        (json!(["a"]), 1),
        (json!(["b"]), 0),
        (json!([]), 0),
        (Value::Null, 1),
    ] {
        let result = c
            .command(
                "query",
                json!({"avatar":"booth:4993931","accountIds":scope}),
            )
            .unwrap();
        assert_eq!(result["matched"], matched);
    }
    let mut public_only = source.clone();
    public_only["id"] = json!("900002");
    c.put("product", "900002", public_only).unwrap();
    assert_eq!(
        strings(&dynamic_details(&c, "900002")["listingAvatars"]).len(),
        4
    );
    assert!(strings(&dynamic_details(&c, "900002")["ownedAvatars"]).is_empty());
    c.command(
        "overlay",
        json!({"ids":["900001"],"patch":{"ownedAvatars":["Human base"]}}),
    )
    .unwrap();
    assert_eq!(
        dynamic_details(&c, "900001")["ownedAvatars"],
        json!(["Human base"])
    );
    c.command(
        "overlay",
        json!({"ids":["900001"],"patch":{},"clearFields":["ownedAvatars"]}),
    )
    .unwrap();
    assert_eq!(
        strings(&dynamic_details(&c, "900001")["ownedAvatars"]).len(),
        4
    );
    assert_eq!(c.get("remote_file", "a-file").unwrap(), Some(a_file));
    assert_eq!(c.get("remote_file", "b-file").unwrap(), Some(b_file));
    drop(c);
    let c = fixture.open();
    assert_eq!(
        strings(&dynamic_details(&c, "900001")["ownedAvatars"]).len(),
        4
    );
    c.put("entitlement","a-ent",json!({"id":"a-ent","productId":"900001","accountId":"a","fileIds":[],"manifestComplete":true})).unwrap();
    assert_eq!(
        strings(&dynamic_details(&c, "900001")["ownedAvatars"]).len(),
        1
    );
    assert_eq!(
        strings(&dynamic_details(&c, "900001")["listingAvatars"]).len(),
        4
    );
    assert_eq!(
        c.command(
            "query",
            json!({"avatar":"booth:4993931","accountIds":["a"]})
        )
        .unwrap()["matched"],
        0
    );
}

#[test]
fn clear_activity_removes_only_terminal_jobs_and_preserves_current_facts() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "p");
    let account = json!({"id":"a","store":"BOOTH","lastSyncAt":"2026-10-11T00:00:00Z"});
    let checkpoint = json!({"id":"restart","accountId":"a","page":3});
    let artifact =
        json!({"id":"artifact","productId":"p","fileId":"f","status":"missing","revision":"r"});
    c.put("account", "a", account.clone()).unwrap();
    c.put("checkpoint", "restart", checkpoint.clone()).unwrap();
    c.put("artifact", "artifact", artifact.clone()).unwrap();
    for state in [
        "queued",
        "running",
        "completed",
        "failed",
        "needsAction",
        "cancelled",
    ] {
        c.put(
            "job",
            state,
            json!({"id":state,"type":"download","productId":"p","state":state}),
        )
        .unwrap();
    }
    let result = c.command("clear_activity", json!({})).unwrap();
    assert_eq!(result, json!({"cleared":4,"remainingActive":2}));
    assert_eq!(c.list("job").unwrap().len(), 2);
    assert!(c.get("job", "queued").unwrap().is_some());
    assert!(c.get("job", "running").unwrap().is_some());
    assert_eq!(c.get("account", "a").unwrap(), Some(account));
    assert_eq!(c.get("checkpoint", "restart").unwrap(), Some(checkpoint));
    assert_eq!(c.get("artifact", "artifact").unwrap(), Some(artifact));
    assert!(c.get("product", "p").unwrap().is_some());
    assert_eq!(
        c.command("clear_activity", json!({})).unwrap()["cleared"],
        0
    );
}

#[test]
fn spending_summary_counts_only_unique_proven_payments_with_exact_currency_totals() {
    let fixture = Fixture::new();
    let c = fixture.open();
    for (id, provider) in [
        ("booth-paid", "booth"),
        ("usd-paid", "jinxxy"),
        ("unknown", "gumroad"),
        ("local:free", "local"),
    ] {
        c.put("product",id,json!({"id":id,"provider":provider,"title":format!("Authored {id}"),"price":999999,"priceCurrency":"USD"})).unwrap();
    }
    let add = |id: &str,
               product: &str,
               account: &str,
               fact: &str,
               amount: Value,
               currency: &str| {
        c.put("acquisition",id,json!({"id":id,"productId":product,"accountId":account,"source":"purchase","sourceRecordId":fact,"orderId":"same-order","paidPrice":amount,"paidCurrency":currency,"paidPriceBasis":"productLineTotal","variantPayments":[{"variantId":"v","price":amount,"currency":currency}]})).unwrap();
    };
    add("line-a", "booth-paid", "a", "line-a", json!(100), "JPY");
    add("duplicate", "booth-paid", "a", "line-a", json!(100), "JPY");
    add("line-b", "booth-paid", "a", "line-b", json!(200), "JPY");
    add(
        "other-account",
        "booth-paid",
        "b",
        "line-a",
        json!(50),
        "JPY",
    );
    add("usd-one", "usd-paid", "a", "one", json!(0.1), "USD");
    add("usd-two", "usd-paid", "a", "two", json!(0.2), "USD");
    add(
        "usd-duplicate-grant",
        "usd-paid",
        "a",
        "one",
        json!(0.1),
        "USD",
    );
    add(
        "local-false-paid",
        "local:free",
        "a",
        "local",
        json!(1000),
        "USD",
    );
    c.put("acquisition","unproven",json!({"id":"unproven","productId":"unknown","accountId":"a","source":"purchase","sourceRecordId":"unknown","paidPrice":777,"paidCurrency":"USD","paidPriceBasis":"listingPrice"})).unwrap();
    let selection =
        json!({"productIds":["booth-paid","usd-paid","unknown","local:free","booth-paid"]});
    let result = c.command("spending_summary", selection.clone()).unwrap();
    assert_eq!(
        result["totals"],
        json!([
            {"currency":"JPY","amount":"350","minorUnits":"350","fractionDigits":0},
            {"currency":"USD","amount":"0.30","minorUnits":"30","fractionDigits":2}
        ])
    );
    assert_eq!(result["skippedCount"], 2);
    assert_eq!(result["products"].as_array().unwrap().len(), 4);
    assert_eq!(result["products"][0]["title"], "Authored booth-paid");
    let mut scoped = selection.clone();
    scoped["accountIds"] = json!(["a"]);
    assert_eq!(
        c.command("spending_summary", scoped.clone()).unwrap()["totals"][0]["minorUnits"],
        "300"
    );
    scoped["accountIds"] = json!([]);
    assert!(c.command("spending_summary", scoped).unwrap()["totals"]
        .as_array()
        .unwrap()
        .is_empty());
    // Conflicting observations of one fact are unknown, never an invented sum.
    add("conflict", "usd-paid", "a", "one", json!(0.4), "USD");
    let conflicted = c.command("spending_summary", selection).unwrap();
    assert_eq!(conflicted["totals"][1]["amount"], "0.20");
    assert_eq!(
        conflicted["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["productId"] == "usd-paid")
            .unwrap()["unknownPaymentCount"],
        1
    );
    add(
        "fractional",
        "unknown",
        "a",
        "fractional",
        json!(0.001),
        "USD",
    );
    add("three-digits", "unknown", "a", "kwd", json!(1.234), "KWD");
    let precise = c
        .command("spending_summary", json!({"productIds":["unknown"]}))
        .unwrap();
    assert_eq!(
        precise["totals"],
        json!([{"currency":"KWD","amount":"1.234","minorUnits":"1234","fractionDigits":3}])
    );
    assert_eq!(precise["products"][0]["unknownPaymentCount"], 2);
}

#[test]
fn current_ui_preferences_validate_explicit_patches_without_backfill() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let initial = c.get("preferences", "app").unwrap().unwrap();
    assert_eq!(initial["downloadConcurrency"], 3);
    assert_eq!(initial["aiFeaturesEnabled"], false);
    assert_eq!(initial["sensitiveMode"], "blur");
    assert_eq!(initial["batchActions"], default_batch_actions());
    assert_eq!(initial["cardColumns"], json!(["shop"]));
    assert_eq!(
        initial["listColumns"],
        json!(["shop", "category", "files", "acquired", "price"])
    );
    let saved = c.command("preferences",json!({"patch":{"aiFeaturesEnabled":true,"sensitiveMode":"only","batchActions":{"spending":false},"cardColumns":["price","category"]}})).unwrap();
    assert_eq!(saved["batchActions"]["spending"], false);
    assert_eq!(saved["batchActions"]["download"], true);
    assert_eq!(saved["cardColumns"], json!(["price", "category"]));
    assert_eq!(saved["listColumns"], initial["listColumns"]);
    let saved = c
        .command("preferences", json!({"patch":{"listColumns":[]}}))
        .unwrap();
    assert_eq!(saved["cardColumns"], json!(["price", "category"]));
    assert_eq!(saved["listColumns"], json!([]));
    for patch in [
        json!({"aiFeaturesEnabled":"true"}),
        json!({"sensitiveMode":"invalid"}),
        json!({"batchActions":{"madeUp":true}}),
        json!({"batchActions":{"edit":0}}),
        json!({"cardColumns":"shop"}),
        json!({"cardColumns":["unknown"]}),
        json!({"cardColumns":["shop","shop"]}),
        json!({"listColumns":["price",false]}),
        json!({"listColumns":null}),
    ] {
        assert!(c.command("preferences", json!({"patch":patch})).is_err());
        assert_eq!(c.get("preferences", "app").unwrap().unwrap(), saved);
    }
    drop(c);
    let c = fixture.open();
    assert_eq!(c.get("preferences", "app").unwrap().unwrap(), saved);
    // Sparse explicitly authored preferences stay sparse on open.
    c.put("preferences", "app", json!({"id":"app","locale":"ja"}))
        .unwrap();
    drop(c);
    let c = fixture.open();
    let sparse = c.get("preferences", "app").unwrap().unwrap();
    for key in ["batchActions", "cardColumns", "listColumns"] {
        assert!(sparse.get(key).is_none());
    }
}

#[test]
fn query_all_downloaded_covers_all_matches_before_pagination() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put(
        "root",
        "r",
        json!({"id":"r","path":"X:/authored","status":"online"}),
    )
    .unwrap();
    for id in ["a-ready", "z-pending"] {
        product(&c, id);
        c.put(
            "remote_file",
            id,
            json!({"id":id,"productId":id,"name":"authored.unitypackage","revision":"r1"}),
        )
        .unwrap();
        c.put(
            "entitlement",
            id,
            json!({"id":id,"productId":id,"fileIds":[id],"manifestComplete":true}),
        )
        .unwrap();
    }
    let artifact = |id: &str| json!({"id":id,"productId":id,"fileId":id,"rootId":"r","revision":"r1","status":"ready","relativePath":format!("{id}/authored.unitypackage")});
    c.put("artifact", "a-ready", artifact("a-ready")).unwrap();
    let args = json!({"sort":"title","limit":1,"includeFiles":false});
    let first = c.command("query", args.clone()).unwrap();
    assert_eq!(first["products"].as_array().unwrap().len(), 1);
    assert_eq!(first["products"][0]["id"], "a-ready");
    assert_eq!(first["products"][0]["fullyDownloaded"], true);
    assert_eq!(first["allDownloaded"], false);
    c.put("artifact", "z-pending", artifact("z-pending"))
        .unwrap();
    assert_eq!(c.command("query", args).unwrap()["allDownloaded"], true);
    assert_eq!(
        c.command("query", json!({"offset":100,"limit":0})).unwrap()["allDownloaded"],
        true
    );
    c.put("entitlement","z-pending",json!({"id":"z-pending","productId":"z-pending","fileIds":["z-pending"],"manifestComplete":false})).unwrap();
    assert_eq!(
        c.command("query", json!({"limit":1})).unwrap()["allDownloaded"],
        true
    );
    assert_eq!(
        c.command("query", json!({"search":"nothing-matches"}))
            .unwrap()["allDownloaded"],
        false
    );
}

#[test]
fn warm_list_actions_skip_stored_rows_and_edits_reread_only_their_row() {
    // Every list column follows the file-heavy body, so reading a stored row is
    // the network cost on a shared library. Warm searches, avatar and account
    // filters read the TEMP copy; an edit rereads only the edited product.
    let f = Fixture::new();
    let c = f.open();
    let mut records = vec![];
    for n in 0..60 {
        let id = format!("{}", 920000000 + n);
        let account = if n % 2 == 0 { "b" } else { "a" };
        let avatar = ["Manuka", "Kikyo", "Shinano"][n % 3];
        records.push(("product".into(), id.clone(), json!({"id":id,"title":format!("Authored outfit {n:02}"),"sourceCategory":{"name":"Outfit"},"tags":[avatar,"Casual"],"description":"Authored description. ".repeat(80)})));
        let file_ids: Vec<_> = (0..30).map(|file| format!("{id}:file:{file}")).collect();
        for file in &file_ids {
            records.push(("remote_file".into(), file.clone(), json!({"id":file,"productId":id,"accountId":account,"name":format!("{file}_authored.unitypackage"),"revision":"authored-v1","sizeBytes":123456})));
        }
        records.push(("entitlement".into(), format!("{account}:{id}"), json!({"id":format!("{account}:{id}"),"productId":id,"accountId":account,"fileIds":file_ids,"manifestComplete":true})));
    }
    c.put_many(records).unwrap();
    // A tiny library cache turns each stored-row read into a miss; the TEMP copy stays cached.
    let stored: i64 = {
        let conn = c.inner.lock().unwrap();
        conn.execute_batch("PRAGMA main.cache_size=8; PRAGMA temp.cache_size=-65536;")
            .unwrap();
        conn.query_row("SELECT COUNT(*) FROM dbstat WHERE name='products'", [], |r| r.get(0))
            .unwrap()
    };
    let list = |patch: Value, known: &Value| {
        let misses = || {
            let conn = c.inner.lock().unwrap();
            let (mut current, mut high) = (0, 0);
            unsafe {
                rusqlite::ffi::sqlite3_db_status(conn.handle(), rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS, &mut current, &mut high, 0)
            };
            i64::from(current)
        };
        let mut args = json!({"limit":10000,"includeFiles":false,"presentation":"list","allSummaries":true,"knownSummaries":known});
        for (key, value) in patch.as_object().unwrap() {
            args[key] = value.clone();
        }
        let before = misses();
        let result = c.command("query", args).unwrap();
        (result, misses() - before)
    };
    let (first, _) = list(json!({}), &json!({}));
    let known = first["fingerprints"].clone();
    assert_eq!(known.as_object().unwrap().len(), 60);
    for patch in [
        json!({"search":"outfit 1"}),
        json!({"avatar":"Manuka"}),
        json!({"accountIds":["a"],"includeOffline":true,"sort":"price"}),
        json!({"accountIds":["b"],"includeOffline":true,"avatar":"Kikyo"}),
    ] {
        let (result, read) = list(patch.clone(), &known);
        assert!(result["matched"].as_u64().unwrap() > 0, "{patch}");
        assert!(read * 10 < stored, "{patch} read {read} of {stored} product pages");
    }
    c.command("overlay", json!({"changes":[{"id":"920000007","patch":{"title":"Edited authored outfit"}}]}))
        .unwrap();
    let (result, read) = list(json!({}), &known);
    let sent: Vec<_> = result["products"].as_array().unwrap().iter().map(|p| text(p, "id")).collect();
    assert_eq!(sent, ["920000007"]);
    assert_eq!(result["products"][0]["title"], "Edited authored outfit");
    assert!(read * 10 < stored, "an edit reread {read} of {stored} product pages");
}

#[test]
fn custom_categories_stay_manual_and_deleting_one_restores_automatic_classification() {
    let fixture = Fixture::new();
    let c = fixture.open();
    // Backups made before custom categories have no record list for them.
    let mut old = c.manifest().unwrap();
    old["records"].as_object_mut().unwrap().remove("custom_category");
    let restored = Fixture::new();
    restored.open().restore_manifest(old).unwrap();
    product(&c, "hair");
    c.put("product", "plain", json!({"id":"plain","title":"Authored unrelated print"})).unwrap();
    let create = |name: &str| c.command("custom_category", json!({"op":"create","name":name}));
    let props = text(&create("Props").unwrap()["customCategories"][0], "id").to_owned();
    assert!(props.starts_with("custom:"));
    assert!(create(" ｐｒｏｐｓ ").is_err());
    for missing in [format!("custom:{}", Uuid::new_v4()), "custom:authored".into()] {
        assert!(c.command("overlay", json!({"ids":["hair"],"patch":{"category":missing}})).is_err());
    }
    let category = |id| projection(&c, id)["category"].clone();
    let in_all = |id| c.command("query", json!({})).unwrap()["matchedIds"].as_array().unwrap().contains(&json!(id));
    assert!(!in_all("plain"));
    c.command("overlay", json!({"ids":["hair","plain"],"patch":{"category":props}})).unwrap();
    assert_eq!(c.command("query", json!({"category":props})).unwrap()["matchedIds"].as_array().unwrap().len(), 2);
    assert!(in_all("plain"));
    // A new source observation cannot reclassify a manually chosen custom category.
    c.put("product", "hair", json!({"id":"hair","title":"Authored shader","sourceCategory":{"name":"Shader"}})).unwrap();
    assert_eq!(category("hair"), props);
    c.command("custom_category", json!({"op":"update","id":props,"name":"Small props"})).unwrap();
    create("Kept").unwrap();
    let away = c.command("overlay", json!({"ids":["hair"],"patch":{"category":"Tool"}})).unwrap();
    let copy = Fixture::new();
    let peer = copy.open();
    peer.restore_manifest(c.manifest().unwrap()).unwrap();
    assert_eq!(projection(&peer, "plain")["category"], props);
    assert_eq!(peer.command("bootstrap", json!({})).unwrap()["customCategories"].as_array().unwrap().len(), 2);
    let remaining = c.command("custom_category", json!({"op":"delete","id":props})).unwrap();
    assert_eq!(remaining["customCategories"].as_array().unwrap().iter().map(|row| text(row, "name")).collect::<Vec<_>>(), ["Kept"]);
    assert!(c.get("overlay", "plain").unwrap().unwrap().get("category").is_none());
    assert_eq!(category("plain"), "NonVRChat");
    assert!(!in_all("plain"));
    // Tool is still current, yet undoing the move must not bring back the deleted category.
    let undone = c.command("undo", json!({"token":away["undoToken"]})).unwrap();
    assert_eq!(undone["conflicts"], json!(["hair"]));
    assert_eq!(category("hair"), "Tool");
}
