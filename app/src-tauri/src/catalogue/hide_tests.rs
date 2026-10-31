use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/catalogue-synthetic")
            .join(format!("nna-hidden-products-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn open(&self) -> Catalogue {
        Catalogue::open(&self.0).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let cleanup = || -> Result<()> {
            let boundary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/catalogue-synthetic")
                .canonicalize()?;
            crate::storage::validate_library_path(&self.0)?;
            let target = self.0.canonicalize()?;
            ensure!(
                target.parent() == Some(boundary.as_path())
                    && target.file_name().is_some_and(|name| name
                        .to_string_lossy()
                        .starts_with("nna-hidden-products-")),
                "unsafe_fixture_cleanup"
            );
            let mut pending = vec![target.clone()];
            while let Some(path) = pending.pop() {
                crate::storage::validate_library_path(&path)?;
                if path.is_dir() {
                    for entry in std::fs::read_dir(&path)? {
                        pending.push(entry?.path());
                    }
                }
            }
            std::fs::remove_dir_all(target)?;
            Ok(())
        };
        let _ = cleanup();
    }
}

fn product(c: &Catalogue, id: &str, provider: &str) {
    let mut source = json!({"id":id,"provider":provider,"sourceTitle":id,"sourceCategory":{"name":"Hair"},"custom":provider=="local"});
    if provider == "gumroad" {
        source["sourceDescription"] = json!("Authored hair for VRChat.");
    }
    c.put(
        "product",
        id,
        source,
    )
    .unwrap();
}

fn hide(c: &Catalogue, ids: &[&str], hidden: bool) -> Value {
    c.command("overlay", json!({"ids":ids,"patch":{"hidden":hidden}}))
        .unwrap()
}

fn details(c: &Catalogue, id: &str) -> Value {
    c.command("product_details", json!({"id":id})).unwrap()
}

#[test]
fn hidden_products_filter_before_paging_selection_counts_and_collections() {
    let fixture = Fixture::new();
    let c = fixture.open();
    for id in ["a-visible", "b-hidden"] {
        product(&c, id, "booth");
        let file = format!("{id}-file");
        c.put_many(vec![
            ("account".into(), "account".into(), json!({"id":"account","provider":"booth"})),
            ("remote_file".into(), file.clone(), json!({"id":file,"productId":id,"accountId":"account","name":"Authored.unitypackage","revision":"r1"})),
            ("entitlement".into(), id.into(), json!({"id":id,"productId":id,"accountId":"account","fileIds":[file],"manifestComplete":true})),
            ("artifact".into(), file.clone(), json!({"id":file,"productId":id,"fileId":file,"revision":"r1","status":"ready"})),
        ]).unwrap();
    }
    product(&c, "c-hidden-other", "gumroad");
    c.command(
        "overlay",
        json!({"ids":["c-hidden-other"],"patch":{"category":"NonVRChat"}}),
    )
    .unwrap();
    product(&c, "d-local", "local");
    c.command("collection", json!({"op":"create","id":"collection","value":{"name":"Authored collection","productIds":["a-visible","b-hidden"]}})).unwrap();
    hide(&c, &["b-hidden", "c-hidden-other"], true);

    let first = c
        .command(
            "query",
            json!({"sort":"title","limit":1,"includeFiles":false,"presentation":"list"}),
        )
        .unwrap();
    assert_eq!(first["matchedIds"], json!(["a-visible", "d-local"]));
    assert_eq!(first["products"].as_array().unwrap().len(), 1);
    assert_eq!(first["products"][0]["hidden"], false);
    assert_eq!(first["total"], 4);
    assert_eq!(first["counts"]["all"], 2);
    assert_eq!(first["counts"]["hidden"], 2);
    assert_eq!(first["counts"]["downloaded"], 1);
    assert_eq!(first["counts"]["categories"]["Hair"], 2);
    assert!(first["counts"]["categories"].get("NonVRChat").is_none());
    let rest = c
        .command("query", json!({"sort":"title","offset":1,"limit":1}))
        .unwrap();
    assert_eq!(rest["products"][0]["id"], "d-local");
    let downloaded = c.command("query", json!({"quick":"downloaded"})).unwrap();
    assert_eq!(downloaded["matchedIds"], json!(["a-visible"]));
    assert_eq!(downloaded["allDownloaded"], true);
    assert_eq!(
        c.command("query", json!({"collectionId":"collection"}))
            .unwrap()["matchedIds"],
        json!(["a-visible"])
    );
    assert_eq!(
        c.command("account_counts", json!({})).unwrap(),
        json!({"accounts":{"account":1},"unassigned":0})
    );

    let hidden = c
        .command(
            "query",
            json!({"hiddenOnly":true,"sort":"title","includeFiles":false,"presentation":"list"}),
        )
        .unwrap();
    assert_eq!(hidden["matchedIds"], json!(["b-hidden", "c-hidden-other"]));
    assert!(hidden["products"]
        .as_array()
        .unwrap()
        .iter()
        .all(|product| product["hidden"] == true));
    assert_eq!(
        c.command("bootstrap", json!({})).unwrap()["products"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        details(&c, "b-hidden")["files"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        c.get("collection", "collection").unwrap().unwrap()["productIds"],
        json!(["a-visible", "b-hidden"])
    );

    hide(&c, &["b-hidden"], false);
    assert_eq!(
        c.command("query", json!({"collectionId":"collection"}))
            .unwrap()["matched"],
        2
    );
    assert_eq!(
        c.command("account_counts", json!({})).unwrap()["accounts"]["account"],
        2
    );
}

#[test]
fn list_snapshot_follows_product_changes_and_skips_known_summaries() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "a", "booth");
    product(&c, "b", "booth");
    product(&c, "c", "local");
    let query = |extra: Value| {
        let mut args = json!({"sort":"title","includeFiles":false,"presentation":"list","allSummaries":true});
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        c.command("query", args).unwrap()
    };
    let legacy = query(json!({}));
    assert!(legacy.get("fingerprints").is_none());
    assert_eq!(legacy["products"].as_array().unwrap().len(), 3);
    let first = query(json!({"knownSummaries":{}}));
    let known = first["fingerprints"].clone();
    assert_eq!(first["matchedIds"], json!(["a", "b", "c"]));
    assert_eq!(known.as_object().unwrap().len(), 3);
    assert_eq!((&first["counts"]["all"], &first["counts"]["hidden"]), (&json!(3), &json!(0)));
    let unchanged = query(json!({"knownSummaries":known}));
    assert_eq!(unchanged["matchedIds"], json!(["a", "b", "c"]));
    assert_eq!(unchanged["products"], json!([]));
    assert_eq!(unchanged["fingerprints"], json!({}));

    // Hiding changes the products revision, so filters and counts cannot reuse the old copy.
    hide(&c, &["a"], true);
    let visible = query(json!({"knownSummaries":known}));
    assert_eq!(visible["matchedIds"], json!(["b", "c"]));
    assert_eq!(visible["products"], json!([]));
    assert_eq!((&visible["counts"]["all"], &visible["counts"]["hidden"]), (&json!(2), &json!(1)));
    let hidden = query(json!({"hiddenOnly":true,"knownSummaries":known}));
    assert_eq!(hidden["matchedIds"], json!(["a"]));
    assert_eq!(hidden["products"][0]["hidden"], true);
    assert_ne!(hidden["fingerprints"]["a"], known["a"]);
    // A different account scope never reuses another scope's summary.
    let local = query(json!({"accountIds":[],"includeOffline":true,"knownSummaries":known}));
    assert_eq!(local["matchedIds"], json!(["c"]));
    assert_eq!(local["products"][0]["id"], "c");

    hide(&c, &["a"], false);
    let restored = query(json!({"knownSummaries":known}));
    assert_eq!(restored["matchedIds"], json!(["a", "b", "c"]));
    assert_eq!(restored["counts"]["hidden"], 0);
}

#[test]
fn hidden_choice_survives_three_store_refresh_ai_reopen_and_restore() {
    let fixture = Fixture::new();
    let c = fixture.open();
    for provider in ["booth", "gumroad", "jinxxy"] {
        product(&c, provider, provider);
        let snapshot = c.collect_ai_metadata(provider).unwrap();
        hide(&c, &[provider], true);
        let result: crate::ai_analysis::AnalysisOutput = serde_json::from_value(json!({
            "category":null,"tags":[{"label":"Authored AI tag","evidenceRefs":["title"],"basis":"Authored input","confidence":0.8}],"tagAliases":[],"sourceUrlCandidates":[]
        })).unwrap();
        // An already-running AI result preserves the later user visibility edit.
        c.publish_ai_analysis(
            provider,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &result,
            "authored-model",
        )
        .unwrap();
        assert_eq!(details(&c, provider)["hidden"], true);
        let mut source = c.get("product", provider).unwrap().unwrap();
        source["sourceTitle"] = json!("Refreshed authored store product");
        source["hidden"] = json!(false);
        for complete in [false, true] {
            c.put_many(vec![
                ("product".into(), provider.into(), source.clone()),
                ("remote_file".into(), provider.into(), json!({"id":provider,"productId":provider,"name":"Refreshed-authored.zip","revision":"r2"})),
                ("entitlement".into(), provider.into(), json!({"id":provider,"productId":provider,"fileIds":[provider],"manifestComplete":complete})),
            ]).unwrap();
            assert_eq!(details(&c, provider)["hidden"], true);
            assert_eq!(
                details(&c, provider)["files"][0]["name"],
                "Refreshed-authored.zip"
            );
        }
    }
    assert_eq!(c.command("query", json!({})).unwrap()["matched"], 0);
    assert_eq!(
        c.command("query", json!({"hiddenOnly":true})).unwrap()["matched"],
        3
    );
    drop(c);
    let reopened = fixture.open();
    assert_eq!(reopened.command("query", json!({})).unwrap()["matched"], 0);
    assert_eq!(
        reopened
            .command("query", json!({"hiddenOnly":true}))
            .unwrap()["matched"],
        3
    );
    let peer_fixture = Fixture::new();
    let peer = peer_fixture.open();
    peer.restore_manifest(reopened.manifest().unwrap()).unwrap();
    assert_eq!(peer.command("query", json!({})).unwrap()["matched"], 0);
    assert_eq!(
        peer.command("query", json!({"hiddenOnly":true})).unwrap()["matched"],
        3
    );
    hide(&peer, &["booth", "gumroad", "jinxxy"], false);
    assert_eq!(peer.command("query", json!({})).unwrap()["matched"], 3);
}

#[test]
fn avatar_pins_never_cross_hidden_scope() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put("product", "5007531", json!({"id":"5007531","provider":"booth","sourceTitle":"真冬","sourceCategory":{"name":"3Dアバター"}})).unwrap();
    product(&c, "visible-member", "booth");
    product(&c, "hidden-member", "booth");
    c.command(
        "overlay",
        json!({"ids":["visible-member","hidden-member"],"patch":{"ownedAvatars":["Mafuyu"]}}),
    )
    .unwrap();
    hide(&c, &["5007531", "hidden-member"], true);
    let visible = c.command("query", json!({"avatar":"Mafuyu"})).unwrap();
    assert_eq!(visible["matchedIds"], json!(["visible-member"]));
    let hidden = c
        .command(
            "query",
            json!({"avatar":"Mafuyu","hiddenOnly":true,"sort":"title"}),
        )
        .unwrap();
    assert_eq!(hidden["matchedIds"], json!(["5007531", "hidden-member"]));
    hide(&c, &["5007531"], false);
    assert_eq!(
        c.command("query", json!({"avatar":"Mafuyu","hiddenOnly":true}))
            .unwrap()["matchedIds"],
        json!(["hidden-member"])
    );
}

#[test]
fn hide_rejects_local_and_invalid_patches_atomically_and_ignores_source_visibility() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "store", "booth");
    product(&c, "local", "local");
    assert_eq!(c.command("overlay", json!({"changes":[{"id":"store","patch":{"hidden":true}},{"id":"local","patch":{"hidden":true}}]})).unwrap_err().to_string(), "catalogue.hide_store_only");
    assert_eq!(details(&c, "store")["hidden"], false);
    assert_eq!(details(&c, "local")["hidden"], false);
    assert!(c
        .command(
            "overlay",
            json!({"ids":["store"],"patch":{"hidden":"true"}})
        )
        .is_err());
    let mut source = c.get("product", "store").unwrap().unwrap();
    source["hidden"] = json!(true);
    c.put("product", "store", source).unwrap();
    assert_eq!(details(&c, "store")["hidden"], false);
    let hidden = hide(&c, &["store"], true);
    assert_eq!(details(&c, "store")["hidden"], true);
    c.command("undo", json!({"token":hidden["token"]})).unwrap();
    assert_eq!(details(&c, "store")["hidden"], false);
}

#[test]
fn library_recommendations_exclude_hidden_even_as_explicit_context() {
    let fixture = Fixture::new();
    let c = fixture.open();
    product(&c, "visible", "booth");
    product(&c, "hidden", "gumroad");
    hide(&c, &["hidden"], true);
    let candidates = c
        .ai_recommendation_candidates("", None, None, &["hidden".into()])
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["productId"], "visible");
    hide(&c, &["hidden"], false);
    assert_eq!(
        c.ai_recommendation_candidates("", None, None, &[])
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn unavailable_store_products_follow_the_list_preference_and_keep_their_records() {
    let fixture = Fixture::new();
    let c = fixture.open();
    c.put("account", "account", json!({"id":"account","provider":"booth"}))
        .unwrap();
    for id in ["a-file", "b-empty", "c-kept", "e-hidden-empty"] {
        product(&c, id, "booth");
    }
    product(&c, "d-local", "local");
    c.put_many(vec![
        ("remote_file".into(), "a-file-1".into(), json!({"id":"a-file-1","productId":"a-file","accountId":"account","name":"Authored.unitypackage","revision":"r1"})),
        ("entitlement".into(), "a-file".into(), json!({"id":"a-file","productId":"a-file","accountId":"account","fileIds":["a-file-1"],"manifestComplete":true})),
        // The store's complete file list is now empty.
        ("entitlement".into(), "b-empty".into(), json!({"id":"b-empty","productId":"b-empty","accountId":"account","fileIds":[],"manifestComplete":true})),
        // Bytes from a file the store no longer lists remain on disk.
        ("entitlement".into(), "c-kept".into(), json!({"id":"c-kept","productId":"c-kept","accountId":"account","fileIds":[],"manifestComplete":true})),
        ("artifact".into(), "c-kept-old".into(), json!({"id":"c-kept-old","productId":"c-kept","fileId":"c-kept-old-file","revision":"r1","status":"ready","originalAvailable":true,"relativePath":"assets/c/old.zip"})),
    ])
    .unwrap();
    hide(&c, &["e-hidden-empty"], true);
    let query = |hide: bool| {
        c.command(
            "query",
            json!({"sort":"title","hideUnavailable":hide,"includeFiles":false,"presentation":"list"}),
        )
        .unwrap()
    };
    let shown = query(false);
    assert_eq!(
        shown["matchedIds"],
        json!(["a-file", "b-empty", "c-kept", "d-local"])
    );
    let flags: Vec<_> = shown["products"]
        .as_array()
        .unwrap()
        .iter()
        .map(|product| product["unavailable"] == true)
        .collect();
    assert_eq!(flags, [false, true, false, false]);
    assert_eq!(shown["counts"]["all"], 4);
    let hidden = query(true);
    assert_eq!(hidden["matchedIds"], json!(["a-file", "c-kept", "d-local"]));
    assert_eq!(hidden["counts"]["all"], 3);
    assert_eq!(hidden["counts"]["statuses"]["notDownloaded"], 3);
    assert_eq!(hidden["total"], 5);
    // The Hidden scope still lists every product the user hid.
    assert_eq!(
        c.command("query", json!({"hiddenOnly":true,"hideUnavailable":true}))
            .unwrap()["matchedIds"],
        json!(["e-hidden-empty"])
    );
    // Bootstrap and account totals follow the saved preference, on by default.
    let totals = |c: &Catalogue| {
        (
            c.command("bootstrap", json!({"includeProducts":false}))
                .unwrap()["counts"]["all"]
                .clone(),
            c.command("account_counts", json!({})).unwrap()["accounts"]["account"].clone(),
        )
    };
    assert_eq!(totals(&c), (json!(3), json!(2)));
    c.command("preferences", json!({"patch":{"hideUnavailable":false}}))
        .unwrap();
    assert_eq!(totals(&c), (json!(4), json!(3)));
    assert!(c
        .command("preferences", json!({"patch":{"hideUnavailable":"no"}}))
        .is_err());
    // Nothing is removed: when the store lists a file again the product returns.
    c.put_many(vec![
        ("remote_file".into(), "b-empty-1".into(), json!({"id":"b-empty-1","productId":"b-empty","accountId":"account","name":"Authored.zip","revision":"r1"})),
        ("entitlement".into(), "b-empty".into(), json!({"id":"b-empty","productId":"b-empty","accountId":"account","fileIds":["b-empty-1"],"manifestComplete":true})),
    ])
    .unwrap();
    assert_eq!(
        query(true)["matchedIds"],
        json!(["a-file", "b-empty", "c-kept", "d-local"])
    );
}
