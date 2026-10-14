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
