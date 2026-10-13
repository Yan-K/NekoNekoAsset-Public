//! Synthetic in-memory coverage for transaction-local compatibility projection.
use super::*;
use std::collections::BTreeSet;

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE entities(kind TEXT NOT NULL,id TEXT NOT NULL,product_id TEXT NOT NULL DEFAULT '',body TEXT NOT NULL,PRIMARY KEY(kind,id));
        CREATE TABLE products(id TEXT PRIMARY KEY,body TEXT NOT NULL,summary TEXT NOT NULL DEFAULT '{}',category TEXT NOT NULL,status TEXT NOT NULL,sensitive INTEGER NOT NULL,downloaded INTEGER NOT NULL,title TEXT NOT NULL,acquired TEXT,price REAL,search TEXT NOT NULL,in_all INTEGER NOT NULL);
        CREATE TABLE search_grams(gram TEXT NOT NULL,product_id TEXT NOT NULL,PRIMARY KEY(gram,product_id));").unwrap();
    set_raw(
        &conn,
        "product",
        "p",
        &json!({"id":"p","sourceTitle":"Synthetic coat","sourceTags":["Soraneko"]}),
    )
    .unwrap();
    conn
}

fn option(id: &str) -> Value {
    json!({"id":id,"officialName":"ソラネコ","englishName":"Soraneko","displayName":"ソラネコ / Soraneko","aliases":["Star Cat"]})
}

fn resolver() -> crate::taxonomy::AvatarTagResolver {
    crate::taxonomy::AvatarTagResolver::from_options(&[option("product:synthetic-base")])
}

fn materialized(conn: &Connection, id: &str) -> Value {
    let body: String = conn
        .query_row("SELECT body FROM products WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    serde_json::from_str(&body).unwrap()
}

#[test]
fn manual_avatar_ids_disambiguate_names_and_survive_refresh_and_empty_override() {
    let conn = fixture();
    let resolver = crate::taxonomy::AvatarTagResolver::from_options(&[
        option("product:avatar-a"),
        option("product:avatar-b"),
    ]);
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","ownedAvatars":["product:avatar-b","Human unmatched"]}),
    )
    .unwrap();
    projection::refresh(&conn, "p").unwrap();
    projection::reconcile_tags(&conn, &resolver, None).unwrap();
    let selected = materialized(&conn, "p");
    let evidence = selected["ownedAvatarEvidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 2);
    assert!(evidence
        .iter()
        .any(|entry| entry["avatarId"] == "product:avatar-b"
            && entry["avatar"] == "ソラネコ / Soraneko"
            && entry["source"] == "user"));
    assert!(evidence
        .iter()
        .any(|entry| entry["avatar"] == "Human unmatched" && entry["source"] == "user"));
    assert!(!evidence
        .iter()
        .any(|entry| entry["avatarId"] == "product:avatar-a"));
    set_raw(
        &conn,
        "remote_file",
        "f",
        &json!({"id":"f","productId":"p","name":"plain.unitypackage","revision":"r1"}),
    )
    .unwrap();
    let file_changes = projection::refresh(&conn, "p").unwrap();
    assert!(file_changes.tags.is_empty());
    assert!(!file_changes.identities_changed);
    assert_eq!(
        materialized(&conn, "p")["ownedAvatarEvidence"],
        selected["ownedAvatarEvidence"]
    );
    set_raw(&conn, "product", "p", &json!({"id":"p","sourceTitle":"Refreshed synthetic coat","sourceTags":["Mafuyu","Star Cat"]})).unwrap();
    projection::refresh(&conn, "p").unwrap();
    projection::reconcile_tags(&conn, &resolver, None).unwrap();
    assert_eq!(
        materialized(&conn, "p")["ownedAvatarEvidence"],
        selected["ownedAvatarEvidence"]
    );
    assert_eq!(
        raw(&conn, "overlay", "p").unwrap().unwrap()["ownedAvatars"],
        json!(["product:avatar-b", "Human unmatched"])
    );
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","category":"Base","ownedAvatars":["product:avatar-b","Human unmatched"]}),
    )
    .unwrap();
    assert!(projection::refresh(&conn, "p").unwrap().identities_changed);
    projection::reconcile_tags(&conn, &resolver, None).unwrap();
    set_raw(
        &conn,
        "job",
        "j",
        &json!({"id":"j","productId":"p","type":"download","state":"running"}),
    )
    .unwrap();
    let job_changes = projection::refresh(&conn, "p").unwrap();
    assert!(job_changes.tags.is_empty());
    assert!(!job_changes.identities_changed);
    assert_eq!(
        materialized(&conn, "p")["ownedAvatarEvidence"],
        selected["ownedAvatarEvidence"]
    );
    assert!(!projection::refresh(&conn, "p").unwrap().identities_changed);
    set_raw(&conn, "overlay", "p", &json!({"id":"p","ownedAvatars":[]})).unwrap();
    projection::refresh(&conn, "p").unwrap();
    projection::reconcile_tags(&conn, &resolver, None).unwrap();
    assert_eq!(materialized(&conn, "p")["ownedAvatarEvidence"], json!([]));
    assert_eq!(materialized(&conn, "p")["ownedAvatars"], json!([]));
}

#[test]
fn file_refresh_preserves_dynamic_tags_but_replaces_stale_variant_evidence() {
    let conn = fixture();
    set_raw(
        &conn,
        "remote_file",
        "f",
        &json!({"id":"f","productId":"p","name":"Mafuyu.unitypackage","revision":"r1"}),
    )
    .unwrap();
    let changes = projection::refresh(&conn, "p").unwrap();
    assert_eq!(changes.tags, BTreeSet::from(["p".to_owned()]));
    assert!(!changes.identities_changed);
    projection::reconcile_tags(&conn, &resolver(), Some(&changes.tags)).unwrap();
    let first = materialized(&conn, "p");
    assert!(first["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == "accountFilename"));
    assert!(first["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["avatarId"] == "product:synthetic-base"));
    set_raw(
        &conn,
        "remote_file",
        "f",
        &json!({"id":"f","productId":"p","name":"plain.unitypackage","revision":"r2"}),
    )
    .unwrap();
    set_raw(
        &conn,
        "job",
        "j",
        &json!({"id":"j","productId":"p","type":"download","state":"running"}),
    )
    .unwrap();
    let changes = projection::refresh(&conn, "p").unwrap();
    assert!(changes.tags.is_empty());
    assert!(!changes.identities_changed);
    let next = materialized(&conn, "p");
    assert_eq!(next["status"], "processing");
    assert_eq!(next["files"][0]["revision"], "r2");
    assert_eq!(next["tags"], first["tags"]);
    let evidence = next["ownedAvatarEvidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["source"], "sourceTag");
    assert_eq!(evidence[0]["avatarId"], "product:synthetic-base");
}

#[test]
fn reconciliation_uses_only_materialized_fields_and_retracts_ambiguous_matches() {
    let conn = fixture();
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","tags":["Star Cat"]}),
    )
    .unwrap();
    projection::refresh(&conn, "p").unwrap();
    let before = materialized(&conn, "p");
    conn.execute_batch("DROP TABLE entities;").unwrap();
    projection::reconcile_tags(&conn, &resolver(), None).unwrap();
    let matched = materialized(&conn, "p");
    assert_eq!(matched["ownedAvatarEvidence"].as_array().unwrap().len(), 2);
    assert_eq!(matched["files"], before["files"]);
    assert_eq!(matched["status"], before["status"]);
    assert!(matched["ownedAvatarEvidence"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["entitlementEstablished"] == false));
    let writes = conn.total_changes();
    projection::reconcile_tags(&conn, &resolver(), None).unwrap();
    assert_eq!(
        conn.total_changes(),
        writes,
        "unchanged reconciliation must not rewrite or reindex"
    );
    let ambiguous = crate::taxonomy::AvatarTagResolver::from_options(&[
        option("product:a"),
        option("product:b"),
    ]);
    projection::reconcile_tags(&conn, &ambiguous, None).unwrap();
    assert_eq!(materialized(&conn, "p")["ownedAvatarEvidence"], json!([]));
    let search: String = conn
        .query_row("SELECT search FROM products WHERE id='p'", [], |r| r.get(0))
        .unwrap();
    assert!(
        !search.contains("ソラネコ"),
        "removed compatibility must leave the search index"
    );
}

#[test]
fn canonical_rejection_and_explicit_empty_override_are_inputs_not_inference() {
    let conn = fixture();
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","tags":["Star Cat"],"rejectedTags":["ソラネコ"]}),
    )
    .unwrap();
    projection::refresh(&conn, "p").unwrap();
    projection::reconcile_tags(&conn, &resolver(), None).unwrap();
    let rejected = materialized(&conn, "p");
    assert_eq!(rejected["ownedAvatarEvidence"], json!([]));
    assert_eq!(rejected["tags"], json!([]));
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","tags":["Star Cat"],"ownedAvatars":[]}),
    )
    .unwrap();
    assert!(projection::refresh(&conn, "p").unwrap().tags.contains("p"));
    projection::reconcile_tags(&conn, &resolver(), None).unwrap();
    assert_eq!(materialized(&conn, "p")["ownedAvatarEvidence"], json!([]));
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","tags":["Star Cat"]}),
    )
    .unwrap();
    assert!(projection::refresh(&conn, "p").unwrap().tags.contains("p"));
    projection::reconcile_tags(&conn, &resolver(), None).unwrap();
    assert_eq!(
        materialized(&conn, "p")["ownedAvatarEvidence"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn base_identity_delta_ignores_jobs_but_tracks_manual_links_and_removal() {
    let conn = fixture();
    set_raw(&conn, "overlay", "p", &json!({"id":"p","category":"Base"})).unwrap();
    assert!(projection::refresh(&conn, "p").unwrap().identities_changed);
    assert!(!projection::refresh(&conn, "p").unwrap().identities_changed);
    set_raw(
        &conn,
        "job",
        "j",
        &json!({"id":"j","productId":"p","type":"download","state":"running"}),
    )
    .unwrap();
    assert!(!projection::refresh(&conn, "p").unwrap().identities_changed);
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","category":"Base","ownedAvatars":["Mafuyu"]}),
    )
    .unwrap();
    assert!(projection::refresh(&conn, "p").unwrap().identities_changed);
    conn.execute("DELETE FROM entities WHERE kind='product' AND id='p'", [])
        .unwrap();
    assert!(projection::refresh(&conn, "p").unwrap().identities_changed);
}

#[test]
fn dynamic_resolver_preserves_generic_tag_alias_rejection_and_deduplication() {
    let conn = fixture();
    set_raw(&conn,"overlay","p",&json!({"id":"p","tags":["カジュアル","Casual","配信","Streaming"],"rejectedTags":["Casual"]})).unwrap();
    projection::refresh(&conn, "p").unwrap();
    projection::reconcile_tags(&conn, &resolver(), None).unwrap();
    assert_eq!(materialized(&conn, "p")["tags"], json!(["配信"]));
}

#[test]
fn source_alias_deltas_retain_previous_names_without_saving_human_titles() {
    let conn = fixture();
    set_raw(
        &conn,
        "product",
        "p",
        &json!({"id":"p","sourceTitle":"Soraneko"}),
    )
    .unwrap();
    set_raw(
        &conn,
        "overlay",
        "p",
        &json!({"id":"p","category":"Base","title":"HumanOnlyName"}),
    )
    .unwrap();
    let mut combined = projection::refresh(&conn, "p").unwrap();
    assert!(combined.source_aliases["p"]
        .iter()
        .any(|name| normalized(name) == "soraneko"));
    assert!(projection::refresh(&conn, "p")
        .unwrap()
        .source_aliases
        .is_empty());
    set_raw(
        &conn,
        "product",
        "p",
        &json!({"id":"p","sourceTitle":"Umineko"}),
    )
    .unwrap();
    let changed = projection::refresh(&conn, "p").unwrap();
    for expected in ["soraneko", "umineko"] {
        assert!(changed.source_aliases["p"]
            .iter()
            .any(|name| normalized(name) == expected));
    }
    combined.merge(changed);
    assert!(!combined.source_aliases["p"]
        .iter()
        .any(|name| normalized(name) == "humanonlyname"));
    conn.execute("DELETE FROM entities WHERE kind='product' AND id='p'", [])
        .unwrap();
    let removed = projection::refresh(&conn, "p").unwrap();
    assert!(removed.source_aliases["p"]
        .iter()
        .any(|name| normalized(name) == "umineko"));
    combined.merge(removed);
    assert!(combined.source_aliases["p"]
        .iter()
        .any(|name| normalized(name) == "soraneko"));
}
