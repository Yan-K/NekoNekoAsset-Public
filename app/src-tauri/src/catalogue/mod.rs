//! Local SQLite catalogue. Edits, projections and durable sync intents commit together.
mod accounts;
mod acquisition;
mod avatars;
#[cfg(test)]
mod dynamic_tag_tests;
#[cfg(test)]
mod hide_tests;
mod local_delete;
mod portable;
mod projection;
#[cfg(test)]
pub(crate) use projection::verification_expected;
mod query;
mod shared;
mod spending;
#[cfg(test)]
mod status_tests;
mod sync;
#[cfg(test)]
mod tests;
mod worker;

#[cfg(test)]
mod ai_overlay_tests {
    use super::*;

    #[test]
    fn image_analysis_tracks_preview_identity_and_chat_candidates_exclude_private_fields() {
        let dir = std::env::temp_dir().join(format!("nna-ai-preview-synthetic-{}", Uuid::new_v4()));
        let c = Catalogue::open(&dir).unwrap();
        let id = "local:synthetic-preview";
        let mut source = json!({"id":id,"provider":"local","store":"Local Files","custom":true,"title":"Authored purple jacket","sourceTags":["Purple"],"previewUrl":"https://example.invalid/one.png","privateEvidence":"DO_NOT_SEND","sourceCategory":{"name":"Hair"}});
        c.put("product", id, source.clone()).unwrap();
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"note":"DO_NOT_SEND"}}),
        )
        .unwrap();
        let mut snapshot = c.collect_ai_metadata(id).unwrap();
        let reference = format!("i-{}", "a".repeat(64));
        snapshot.input.files.push(crate::ai_analysis::FileEvidence {
            evidence_ref: reference.clone(),
            file_name: "image-1.png".into(),
            text_excerpt: String::new(),
            candidate_public_urls: vec![],
        });
        let result=serde_json::from_value(json!({"category":{"category":"Clothes","evidenceRefs":[reference],"basis":"Visual garment evidence","confidence":0.8},"tags":[],"tagAliases":[],"sourceUrlCandidates":[]})).unwrap();
        assert!(c
            .publish_ai_analysis(
                id,
                &snapshot.catalogue_fingerprint,
                &snapshot.input,
                &result,
                "synthetic-model"
            )
            .is_err());
        c.publish_ai_analysis_with_preview(
            id,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &result,
            "synthetic-model",
            Some(&snapshot.preview_fingerprint),
        )
        .unwrap();
        assert_eq!(
            c.command("product_details", json!({"id":id})).unwrap()["aiCurrent"],
            true
        );
        let candidates = c
            .ai_recommendation_candidates("purple jacket", None, None, &[])
            .unwrap();
        assert_eq!(candidates[0]["productId"], id);
        assert!(!serde_json::to_string(&candidates)
            .unwrap()
            .contains("DO_NOT_SEND"));
        assert!(candidates[0].get("accountIds").is_none());
        assert!(candidates[0].get("previewUrl").is_none());
        source["previewUrl"] = json!("https://example.invalid/two.png");
        c.put("product", id, source).unwrap();
        assert_eq!(
            c.command("product_details", json!({"id":id})).unwrap()["aiCurrent"],
            false
        );
        assert_eq!(
            c.publish_ai_analysis_with_preview(
                id,
                &snapshot.catalogue_fingerprint,
                &snapshot.input,
                &result,
                "synthetic-model",
                Some(&snapshot.preview_fingerprint)
            )
            .unwrap_err()
            .to_string(),
            "AI_INPUT_STALE"
        );
        snapshot.input.files.clear();
        let text_result=serde_json::from_value(json!({"category":{"category":"Clothes","evidenceRefs":["title"],"basis":"Title evidence","confidence":0.8},"tags":[],"tagAliases":[],"sourceUrlCandidates":[]})).unwrap();
        c.publish_ai_analysis(
            id,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &text_result,
            "synthetic-model",
        )
        .unwrap();
        assert_eq!(
            c.command("product_details", json!({"id":id})).unwrap()["aiCurrent"],
            true
        );
        let mut preview_product = c.get("product", id).unwrap().unwrap();
        preview_product["previewUrl"] = json!("https://i.pximg.net/authored-source.png");
        preview_product["thumbnailPath"] = json!("media/source-only.png");
        c.put("product", id, preview_product.clone()).unwrap();
        c.put("overlay", id, json!({"id":id})).unwrap();
        let handles = c.ai_preview_source(id).unwrap();
        assert_eq!(
            handles.preview_fingerprint,
            c.collect_ai_metadata(id).unwrap().preview_fingerprint
        );
        assert_eq!(
            handles.public_url.as_deref(),
            Some("https://i.pximg.net/authored-source.png")
        );
        assert!(handles.managed_relative_path.is_none());
        c.put(
            "overlay",
            id,
            json!({"id":id,"sourceThumbnailUrl":"https://i.pximg.net/authored-override.png"}),
        )
        .unwrap();
        assert_eq!(
            c.ai_preview_source(id).unwrap().public_url.as_deref(),
            Some("https://i.pximg.net/authored-override.png")
        );
        c.put("overlay", id, json!({"id":id,"sourceThumbnailUrl":"https://i.pximg.net/authored-override.png","thumbnailPath":"media\\authored.png"})).unwrap();
        let handles = c.ai_preview_source(id).unwrap();
        assert_eq!(
            handles.managed_relative_path.as_deref(),
            Some("media/authored.png")
        );
        assert!(handles.public_url.is_none());
        c.put(
            "overlay",
            id,
            json!({"id":id,"thumbnailPath":"media/authored.png","thumbnailHidden":true}),
        )
        .unwrap();
        let handles = c.ai_preview_source(id).unwrap();
        assert!(handles.managed_relative_path.is_none() && handles.public_url.is_none());
        assert_eq!(
            handles.preview_fingerprint,
            c.collect_ai_metadata(id).unwrap().preview_fingerprint
        );
        c.put("overlay", id, json!({"id":id,"sourceThumbnailUrl":""}))
            .unwrap();
        assert!(c.ai_preview_source(id).unwrap().public_url.is_none());
        c.put(
            "overlay",
            id,
            json!({"id":id,"thumbnailPath":"other/authored.png"}),
        )
        .unwrap();
        assert!(c.ai_preview_source(id).is_err());
        c.put("overlay", id, json!({"id":id})).unwrap();
        preview_product["previewUrl"] = Value::Null;
        c.put("product", id, preview_product).unwrap();
        let handles = c.ai_preview_source(id).unwrap();
        assert!(handles.managed_relative_path.is_none() && handles.public_url.is_none());
        drop(c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ai_metadata_result_survives_download_but_publication_still_rejects_stale_snapshot() {
        let dir =
            std::env::temp_dir().join(format!("nna-ai-evidence-synthetic-{}", Uuid::new_v4()));
        let c = Catalogue::open(&dir).unwrap();
        let id = "local:metadata-ai";
        c.put("product", id, json!({"id":id,"provider":"local","store":"Local Files","custom":true,"title":"Synthetic garment","sourceTags":["Source Tag"],"sourceCategory":{"name":"Hair"}})).unwrap();
        c.put("remote_file", "synthetic-file", json!({"id":"synthetic-file","productId":id,"provider":"local","name":"Assets/model.zip","revision":"one"})).unwrap();
        let mut snapshot = c.collect_ai_metadata(id).unwrap();
        snapshot.input.files.clear();
        let result = serde_json::from_value(json!({"category":{"category":"Clothes","evidenceRefs":["title"],"basis":"Authored title","confidence":0.9},"tags":[],"tagAliases":[],"sourceUrlCandidates":[]})).unwrap();
        c.publish_ai_analysis(
            id,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        let aliases = vec![crate::ai_analysis::TagAliasSuggestion {
            from: "Source Tag".into(),
            to: "Organized Tag".into(),
            evidence_refs: vec!["sourceTags".into()],
            basis: "Explicit organization".into(),
            confidence: 0.9,
        }];
        c.publish_ai_tag_organization(
            id,
            &snapshot.catalogue_fingerprint,
            &aliases,
            "synthetic-model",
        )
        .unwrap();
        let artifact = json!({"id":"synthetic-artifact","productId":id,"fileId":"synthetic-file","revision":"one","hash":"downloaded-hash","relativePath":"managed/model.zip","status":"ready","extractedInventory":[{"relativePath":"README.md","hash":"readme-hash","sizeBytes":3,"isDirectory":false}]});
        c.put("artifact", "synthetic-artifact", artifact).unwrap();
        let details = c.command("product_details", json!({"id":id})).unwrap();
        assert_eq!(details["aiCurrent"], true);
        assert_eq!(details["category"], "Clothes");
        assert!(strings(&details["tags"]).contains(&"Organized Tag".into()));
        assert_eq!(
            c.publish_ai_analysis(
                id,
                &snapshot.catalogue_fingerprint,
                &snapshot.input,
                &result,
                "synthetic-model"
            )
            .unwrap_err()
            .to_string(),
            "AI_INPUT_STALE"
        );
        // Filename-only evidence likewise does not claim to have read file bytes.
        let mut names = c.collect_ai_metadata(id).unwrap();
        names
            .input
            .files
            .retain(|file| file.evidence_ref == opaque_ai_file_ref("synthetic-file", ""));
        c.publish_ai_analysis(
            id,
            &names.catalogue_fingerprint,
            &names.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        let mut artifact = c.get("artifact", "synthetic-artifact").unwrap().unwrap();
        artifact["hash"] = json!("changed-unread-bytes");
        c.put("artifact", "synthetic-artifact", artifact).unwrap();
        assert_eq!(
            c.command("product_details", json!({"id":id})).unwrap()["aiCurrent"],
            true
        );
        drop(c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ai_catalogue_candidates_match_filename_but_never_establish_local_origin() {
        let dir =
            std::env::temp_dir().join(format!("nna-ai-candidates-synthetic-{}", Uuid::new_v4()));
        let c = Catalogue::open(&dir).unwrap();
        let local = "local:candidate-synthetic";
        c.put("product",local,json!({"id":local,"provider":"local","store":"Local Files","custom":true,"title":"Imported product"})).unwrap();
        c.put("remote_file","synthetic-zip",json!({"id":"synthetic-zip","productId":local,"provider":"local","name":"Futura_Meadow_v2.zip","revision":"authored"})).unwrap();
        for (id, provider, store, title, url) in [
            (
                "gumroad:futura",
                "gumroad",
                "Gumroad",
                "Futura Meadow VRChat Outfit",
                "https://maker.gumroad.com/l/futura-meadow",
            ),
            (
                "jinxxy:other",
                "jinxxy",
                "Jinxxy",
                "Unrelated Urban Clothes Version 2",
                "https://jinxxy.com/maker/urban",
            ),
            (
                "local:other",
                "local",
                "Local Files",
                "Futura Meadow",
                "https://maker.gumroad.com/l/local-copy",
            ),
            (
                "gumroad:bad",
                "gumroad",
                "Gumroad",
                "Futura Meadow",
                "https://unproved.example/l/futura",
            ),
        ] {
            c.put("product",id,json!({"id":id,"provider":provider,"store":store,"sourceTitle":title,"sourceUrl":url})).unwrap();
        }
        let mut snapshot = c.collect_ai_metadata(local).unwrap();
        let candidates = c.ai_source_candidates(&snapshot.input).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].evidence_ref.starts_with("c-"));
        assert_eq!(
            candidates[0].candidate_public_urls,
            vec!["https://maker.gumroad.com/l/futura-meadow"]
        );
        let result:crate::ai_analysis::AnalysisOutput=serde_json::from_value(json!({"category":null,"tags":[],"tagAliases":[],"sourceUrlCandidates":[{"url":candidates[0].candidate_public_urls[0],"evidenceRefs":[candidates[0].evidence_ref],"basis":"Filename matches an existing public title; local origin unverified","confidence":0.99}]})).unwrap();
        snapshot.input.files.extend(candidates);
        c.publish_ai_analysis(
            local,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        let p = c.command("product_details", json!({"id":local})).unwrap();
        assert_eq!(p["aiSourceUrlCandidates"][0]["status"], "uncertain");
        assert_eq!(p["provider"], "local");
        assert!(p["sourceUrl"].is_null());
        assert!(c.related("entitlement", local).unwrap().is_empty());
        let mut unrelated = snapshot.input;
        unrelated.title = "VRChat Outfit Version 2".into();
        unrelated.files = vec![crate::ai_analysis::FileEvidence {
            evidence_ref: "e-generic".into(),
            file_name: "Unity_Avatar_v2.unitypackage".into(),
            text_excerpt: String::new(),
            candidate_public_urls: vec![],
        }];
        assert!(c.ai_source_candidates(&unrelated).unwrap().is_empty());
        drop(c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ai_overlay_preserves_manual_edits_rejections_and_invalidates_changed_evidence() {
        let dir =
            std::env::temp_dir().join(format!("nna-ai-catalogue-synthetic-{}", Uuid::new_v4()));
        let c = Catalogue::open(&dir).unwrap();
        let id = "local:synthetic-ai";
        let mut source = json!({"id":id,"provider":"local","store":"Local Files","custom":true,"title":"Synthetic Hair","sourceTags":["Source Tag"],"sourceCategory":{"name":"Hair"},"sourceDescription":"Authored description"});
        c.put("product", id, source.clone()).unwrap();
        let records=(0..140).map(|n| {
            let file=format!("synthetic-file-{n:03}");
            ("remote_file".into(),file.clone(),json!({"id":file,"productId":id,"provider":"local","name":format!("Assets/{n:03}.txt"),"revision":"one"}))
        }).collect();
        c.put_many(records).unwrap();
        let mut artifact = json!({"id":"synthetic-artifact","productId":id,"fileId":"synthetic-file-000","revision":"one","hash":"hash-one","relativePath":"managed/000.txt","status":"ready","extractedInventory":[{"relativePath":"docs/readme.txt","hash":"readme-hash","sizeBytes":3,"isDirectory":false}]});
        c.put("artifact", "synthetic-artifact", artifact.clone())
            .unwrap();
        c.command("overlay",json!({"ids":[id],"patch":{"title":"Human Hair","note":"Human note","rejectedTags":["カジュアル"]}})).unwrap();
        let mut snapshot = c.collect_ai_metadata(id).unwrap();
        assert_eq!(snapshot.input.files.len(), 64);
        assert_eq!(snapshot.input.title, "Human Hair");
        assert!(snapshot
            .input
            .files
            .iter()
            .any(|f| f.file_name == "docs/readme.txt"));
        assert!(snapshot
            .input
            .files
            .iter()
            .all(|f| f.evidence_ref.starts_with("e-") && f.text_excerpt.is_empty()));
        assert!(serde_json::to_vec(&snapshot.input).unwrap().len() <= 128 * 1024);
        assert_eq!(
            snapshot.catalogue_fingerprint,
            c.collect_ai_metadata(id).unwrap().catalogue_fingerprint
        );
        let url = "https://maker.gumroad.com/l/synthetic";
        assert_eq!(snapshot.input.files[0].file_name, "docs/readme.txt");
        snapshot.input.files[0].text_excerpt = format!("Product reference: {url}");
        snapshot.input.files[0]
            .candidate_public_urls
            .push(url.into());
        let result: crate::ai_analysis::AnalysisOutput=serde_json::from_value(json!({
            "category":{"category":"Clothes","evidenceRefs":["title"],"basis":"Authored garment evidence","confidence":0.8},
            "tags":[{"label":"AI Tag","evidenceRefs":["title"],"basis":"Authored style","confidence":0.8},{"label":"Casual","evidenceRefs":["title"],"basis":"Rejected style","confidence":0.8}],
            "tagAliases":[],"sourceUrlCandidates":[{"url":url,"evidenceRefs":[snapshot.input.files[0].evidence_ref],"basis":"Embedded reference only","confidence":0.7}]
        })).unwrap();
        c.publish_ai_analysis(
            id,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        let details = |c: &Catalogue| c.command("product_details", json!({"id":id})).unwrap();
        let p = details(&c);
        assert_eq!(p["category"], "Clothes");
        assert_eq!(p["sourceCategory"]["name"], "Hair");
        assert!(strings(&p["tags"]).contains(&"Source Tag".into()));
        assert!(strings(&p["tags"]).contains(&"AI Tag".into()));
        assert!(!strings(&p["tags"]).contains(&"Casual".into()));
        assert_eq!(p["note"], "Human note");
        assert_eq!(p["provider"], "local");
        assert!(p["sourceUrl"].is_null());
        assert!(strings(&p["accountIds"]).is_empty());
        assert_eq!(p["aiSourceUrlCandidates"][0]["status"], "supported");
        let overlay = c.get("overlay", id).unwrap().unwrap();
        assert!(!overlay["ai"].to_string().contains("textExcerpt"));
        c.dismiss_ai_source_url_candidate(id, url).unwrap();
        c.publish_ai_analysis(
            id,
            &snapshot.catalogue_fingerprint,
            &snapshot.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        assert!(details(&c)["aiSourceUrlCandidates"]
            .as_array()
            .unwrap()
            .is_empty());
        artifact["status"] = json!("verifying");
        c.put("artifact", "synthetic-artifact", artifact.clone())
            .unwrap();
        assert_eq!(details(&c)["category"], "Clothes");
        artifact["hash"] = json!("hash-two");
        artifact["extractedInventory"][0]["hash"] = json!("changed-readme-hash");
        c.put("artifact", "synthetic-artifact", artifact.clone())
            .unwrap();
        assert_eq!(details(&c)["aiCurrent"], false);
        assert_eq!(details(&c)["category"], "Hair");
        assert_eq!(
            c.publish_ai_analysis(
                id,
                &snapshot.catalogue_fingerprint,
                &snapshot.input,
                &result,
                "synthetic-model"
            )
            .unwrap_err()
            .to_string(),
            "AI_INPUT_STALE"
        );
        let mut fresh = c.collect_ai_metadata(id).unwrap();
        fresh.input.files[0].candidate_public_urls.push(url.into());
        c.publish_ai_analysis(
            id,
            &fresh.catalogue_fingerprint,
            &fresh.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"tags":["Source Tag"],"category":"Tool"}}),
        )
        .unwrap();
        assert!(
            strings(&c.get("overlay", id).unwrap().unwrap()["rejectedTags"])
                .contains(&"AI Tag".into())
        );
        let fresh = c.collect_ai_metadata(id).unwrap();
        c.publish_ai_analysis(
            id,
            &fresh.catalogue_fingerprint,
            &fresh.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        assert_eq!(details(&c)["category"], "Tool");
        assert_eq!(details(&c)["tags"], json!(["Source Tag"]));
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{},"clearFields":["tags","category"]}),
        )
        .unwrap();
        let fresh = c.collect_ai_metadata(id).unwrap();
        c.publish_ai_analysis(
            id,
            &fresh.catalogue_fingerprint,
            &fresh.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        assert!(!strings(&details(&c)["tags"]).contains(&"AI Tag".into()));
        assert_eq!(details(&c)["category"], "Clothes");
        source["price"] = json!(7.0);
        c.put("product", id, source.clone()).unwrap();
        assert_eq!(details(&c)["aiCurrent"], true);
        let stale = c.collect_ai_metadata(id).unwrap();
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"title":"New human title"}}),
        )
        .unwrap();
        assert_eq!(
            c.publish_ai_analysis(
                id,
                &stale.catalogue_fingerprint,
                &stale.input,
                &result,
                "synthetic-model"
            )
            .unwrap_err()
            .to_string(),
            "AI_INPUT_STALE"
        );
        assert_eq!(details(&c)["title"], "New human title");
        assert!(c
            .command("overlay", json!({"ids":[id],"patch":{"ai":{}}}))
            .is_err());
        let fresh = c.collect_ai_metadata(id).unwrap();
        c.publish_ai_analysis(
            id,
            &fresh.catalogue_fingerprint,
            &fresh.input,
            &result,
            "synthetic-model",
        )
        .unwrap();
        assert_eq!(c.product_ids().unwrap(), [id]);
        c.prepare_sync_document().unwrap();
        drop(c);
        let c = Catalogue::open(&dir).unwrap();
        assert_eq!(details(&c)["aiCurrent"], true);
        assert_eq!(details(&c)["category"], "Clothes");
        assert!(details(&c)["aiSourceUrlCandidates"]
            .as_array()
            .unwrap()
            .is_empty());
        let organized = c.collect_ai_metadata(id).unwrap();
        let aliases = vec![
            crate::ai_analysis::TagAliasSuggestion {
                from: "Source Tag".into(),
                to: "Organized Tag".into(),
                evidence_refs: vec!["sourceTags".into()],
                basis: "Explicit organization".into(),
                confidence: 0.9,
            },
            crate::ai_analysis::TagAliasSuggestion {
                from: "Not An Existing Tag".into(),
                to: "Invented".into(),
                evidence_refs: vec!["sourceTags".into()],
                basis: "Ineligible".into(),
                confidence: 0.8,
            },
        ];
        let ownership = details(&c)["ownedAvatarEvidence"].clone();
        c.publish_ai_tag_organization(
            id,
            &organized.catalogue_fingerprint,
            &aliases,
            "synthetic-model",
        )
        .unwrap();
        assert_eq!(details(&c)["category"], "Clothes");
        assert_eq!(details(&c)["sourceTags"], json!(["Source Tag"]));
        assert!(strings(&details(&c)["tags"]).contains(&"Organized Tag".into()));
        assert!(!strings(&details(&c)["tags"]).contains(&"Source Tag".into()));
        assert!(!strings(&details(&c)["tags"]).contains(&"Invented".into()));
        assert_eq!(details(&c)["ownedAvatarEvidence"], ownership);
        c.command(
            "overlay",
            json!({"ids":[id],"patch":{"tags":["Human Tag"]}}),
        )
        .unwrap();
        let organized = c.collect_ai_metadata(id).unwrap();
        c.publish_ai_tag_organization(
            id,
            &organized.catalogue_fingerprint,
            &aliases,
            "synthetic-model",
        )
        .unwrap();
        assert_eq!(details(&c)["tags"], json!(["Human Tag"]));
        assert!(
            strings(&c.get("overlay", id).unwrap().unwrap()["rejectedTags"])
                .contains(&"Organized Tag".into())
        );
        source["sourceTags"] = json!(["Next Source"]);
        c.put("product", id, source).unwrap();
        assert_eq!(
            c.publish_ai_tag_organization(
                id,
                &organized.catalogue_fingerprint,
                &aliases,
                "synthetic-model"
            )
            .unwrap_err()
            .to_string(),
            "AI_INPUT_STALE"
        );
        let prefs = c
            .command("preferences", json!({"patch":{"aiEnabled":true}}))
            .unwrap();
        assert_eq!(prefs["aiEnabled"], true);
        assert_eq!(prefs["aiScopes"], default_ai_scopes());
        let mut scopes = default_ai_scopes();
        scopes["images"] = json!(true);
        c.command("preferences", json!({"patch":{"aiScopes":scopes.clone()}}))
            .unwrap();
        assert_eq!(
            c.get("preferences", "app").unwrap().unwrap()["aiScopes"]["images"],
            true
        );
        scopes["unrecognizedScope"] = json!(true);
        assert!(c
            .command("preferences", json!({"patch":{"aiScopes":scopes}}))
            .is_err());
        assert!(c
            .command("preferences", json!({"patch":{"aiEnabled":"true"}}))
            .is_err());
        drop(c);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

const KINDS: &[&str] = &[
    "product",
    "overlay",
    "acquisition",
    "entitlement",
    "remote_file",
    "artifact",
    "account",
    "root",
    "job",
    "unity_request",
    "checkpoint",
    "collection",
    "saved_search",
    "preferences",
    "source_coverage",
    "device",
    "download_intent",
];
const SYNC_KINDS: &[&str] = &[
    "product",
    "overlay",
    "acquisition",
    "entitlement",
    "remote_file",
    "collection",
    "saved_search",
    "source_coverage",
];
const EDITABLE: &[&str] = &[
    "title",
    "category",
    "tags",
    "note",
    "ownedAvatars",
    "sensitive",
    "rejectedTags",
    "thumbnailPath",
    "thumbnailHidden",
    "hidden",
    "sourceThumbnailUrl",
    "sourceUrl",
];

pub struct AiMetadataSnapshot {
    pub input: crate::ai_analysis::AnalysisInput,
    pub catalogue_fingerprint: String,
    pub preview_fingerprint: String,
}

/// Native-only handles for the effective selected-product thumbnail. The caller
/// validates managed filesystem access or uses PreviewCache for a public URL.
pub struct AiPreviewSource {
    pub preview_fingerprint: String,
    pub managed_relative_path: Option<String>,
    pub public_url: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiOverlay {
    analysis: crate::ai_analysis::AnalysisOutput,
    evidence_refs: Vec<String>,
    observed_source_urls: Vec<String>,
    catalogue_fingerprint: String,
    input_fingerprint: String,
    #[serde(default)]
    evidence_fingerprint: Option<String>,
    #[serde(default)]
    content_evidence_refs: Vec<String>,
    #[serde(default)]
    preview_fingerprint: Option<String>,
    model: String,
    generated_at: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiTagOrganization {
    aliases: Vec<crate::ai_analysis::TagAliasSuggestion>,
    catalogue_fingerprint: String,
    #[serde(default)]
    evidence_fingerprint: Option<String>,
    model: String,
    generated_at: String,
}

fn normalize_ai_aliases(
    aliases: Vec<crate::ai_analysis::TagAliasSuggestion>,
    rejected: &[String],
) -> Result<Vec<crate::ai_analysis::TagAliasSuggestion>> {
    let output = crate::ai_analysis::normalize_output(
        crate::ai_analysis::AnalysisOutput {
            category: None,
            tags: vec![],
            tag_aliases: aliases,
            source_url_candidates: vec![],
        },
        &BTreeSet::from(["sourceTags".into()]),
        rejected,
    )?;
    let mut targets = std::collections::BTreeMap::new();
    for alias in &output.tag_aliases {
        let from = crate::taxonomy::canonical_tag(&alias.from);
        let to = crate::taxonomy::canonical_tag(&alias.to);
        if let Some(prior) = targets.insert(from, to.clone()) {
            ensure!(prior == to, "AI_INVALID_ANALYSIS");
        }
    }
    Ok(output.tag_aliases)
}

fn validate_ai_organization(value: &Value) -> Result<AiTagOrganization> {
    ensure!(
        serde_json::to_vec(value)?.len() <= 96 * 1024,
        "AI_INVALID_ANALYSIS"
    );
    let mut organization: AiTagOrganization = serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("AI_INVALID_ANALYSIS"))?;
    if let Some(fingerprint) = &organization.evidence_fingerprint {
        ensure!(
            fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
            "AI_INVALID_ANALYSIS"
        );
    }
    ensure!(
        organization.catalogue_fingerprint.len() == 64
            && organization
                .catalogue_fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            && !organization.model.trim().is_empty()
            && organization.model.len() <= 160
            && !organization.model.chars().any(char::is_control)
            && chrono::DateTime::parse_from_rfc3339(&organization.generated_at).is_ok(),
        "AI_INVALID_ANALYSIS"
    );
    organization.aliases = normalize_ai_aliases(organization.aliases, &[])?;
    Ok(organization)
}

fn ai_digest(value: &impl serde::Serialize) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

/// Shared with the managed-file reader. Never exposes account IDs or device paths.
pub fn opaque_ai_file_ref(file_id: &str, member_relative_path: &str) -> String {
    format!(
        "e-{}",
        ai_digest(&(file_id, member_relative_path)).expect("string tuple serializes")
    )
}

fn ai_artifact_facts(value: &Value) -> Value {
    json!({"fileId":value["fileId"],"revision":value["revision"],"hash":value["hash"],
        "extractedInventory":value["extractedInventory"]})
}

fn ai_preview_fingerprint(source: &Value, overlay: &Value) -> Result<String> {
    ai_digest(
        &json!({"source":source["previewUrl"],"override":overlay["sourceThumbnailUrl"],
        "path":overlay["thumbnailPath"],"hidden":overlay["thumbnailHidden"]}),
    )
}

fn ai_fingerprint(
    source: &Value,
    overlay: &Value,
    files: &[Value],
    artifacts: &[Value],
) -> Result<String> {
    let source_fields: Map<String, Value> = [
        "id",
        "provider",
        "sourceTitle",
        "title",
        "name",
        "sourceTags",
        "tags",
        "sourceDescription",
        "description",
        "sourceCategory",
        "category",
        "sourceUrl",
        "localManifest",
    ]
    .iter()
    .map(|key| (key.to_string(), source[*key].clone()))
    .collect();
    let manual: Map<String, Value> = [
        "title",
        "category",
        "tags",
        "ownedAvatars",
        "rejectedTags",
        "sourceUrl",
    ]
    .iter()
    .filter_map(|key| overlay.get(*key).map(|v| (key.to_string(), v.clone())))
    .collect();
    let mut files: Vec<_> = files.iter().map(|v| json!({"id":v["id"],"name":v["name"],"sourceRelativePath":v["sourceRelativePath"],"revision":v["revision"]})).collect();
    let mut artifacts: Vec<_> = artifacts.iter().map(ai_artifact_facts).collect();
    files.sort_by_cached_key(Value::to_string);
    artifacts.sort_by_cached_key(Value::to_string);
    ai_digest(&json!({"source":source_fields,"manual":manual,"files":files,"artifacts":artifacts}))
}

/// Persisted validity follows supplied evidence; the full snapshot above still
/// protects the initial publish transaction against concurrent source changes.
fn ai_evidence_fingerprint(
    source: &Value,
    overlay: &Value,
    files: &[Value],
    artifacts: &[Value],
    refs: &[String],
    content_refs: &[String],
) -> Result<String> {
    let refs: BTreeSet<_> = refs.iter().map(String::as_str).collect();
    let contents: BTreeSet<_> = content_refs.iter().map(String::as_str).collect();
    let mut selected = json!({"id":source["id"],"provider":source["provider"]});
    for (reference, fields) in [
        ("title", &["sourceTitle", "title", "name"][..]),
        ("sourceTags", &["sourceTags", "tags"][..]),
        ("description", &["sourceDescription", "description"][..]),
    ] {
        if refs.contains(reference) {
            for field in fields {
                selected[*field] = source[*field].clone();
            }
        }
    }
    let mut selected_ids = BTreeSet::new();
    let mut selected_artifacts = vec![];
    for file in files {
        let id = text(file, "id");
        let original = opaque_ai_file_ref(id, "");
        if refs.contains(original.as_str()) {
            selected_ids.insert(id);
        }
        for artifact in artifacts
            .iter()
            .filter(|a| text(a, "fileId") == id && a["revision"] == file["revision"])
        {
            let mut members = vec![];
            for entry in artifact["extractedInventory"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let reference = opaque_ai_file_ref(id, text(entry, "relativePath"));
                if refs.contains(reference.as_str()) {
                    selected_ids.insert(id);
                    let mut member = json!({"relativePath":entry["relativePath"],"isDirectory":entry["isDirectory"]});
                    if contents.contains(reference.as_str()) {
                        member["hash"] = entry["hash"].clone();
                        member["sizeBytes"] = entry["sizeBytes"].clone();
                    }
                    members.push(member);
                }
            }
            let original_content = contents.contains(original.as_str());
            if original_content || !members.is_empty() {
                let mut facts =
                    json!({"fileId":id,"revision":file["revision"],"extractedInventory":members});
                if original_content {
                    facts["hash"] = artifact["hash"].clone();
                }
                selected_artifacts.push(facts);
            }
        }
    }
    let selected_files: Vec<_> = files
        .iter()
        .filter(|file| selected_ids.contains(text(file, "id")))
        .cloned()
        .collect();
    ai_fingerprint(&selected, overlay, &selected_files, &selected_artifacts)
}

fn ai_document_priority(name: &str) -> bool {
    let lower = name.to_lowercase();
    let leaf = lower.rsplit('/').next().unwrap_or("");
    let Some((stem, extension)) = leaf.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "txt" | "md" | "markdown" | "rst")
        && [
            "readme",
            "read_me",
            "read-me",
            "manual",
            "guide",
            "instructions",
            "使い方",
            "説明",
            "使用",
            "说明",
            "說明",
        ]
        .iter()
        .any(|part| stem.contains(part))
}

fn ai_text(value: &str, max: usize) -> String {
    let mut out = String::new();
    for c in value
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
    {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

fn ai_match_tokens(value: &str) -> BTreeSet<String> {
    crate::taxonomy::normalize(value)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2 && word.chars().any(char::is_alphabetic))
        .filter(|word| {
            !matches!(
                *word,
                "vrchat"
                    | "unity"
                    | "avatar"
                    | "avatars"
                    | "booth"
                    | "gumroad"
                    | "jinxxy"
                    | "readme"
                    | "license"
                    | "package"
                    | "product"
                    | "pack"
                    | "folder"
                    | "imported"
                    | "docs"
                    | "manual"
                    | "document"
                    | "assets"
                    | "asset"
                    | "model"
                    | "data"
                    | "file"
                    | "files"
                    | "version"
                    | "update"
                    | "free"
                    | "clothes"
                    | "outfit"
                    | "hair"
                    | "texture"
                    | "shader"
                    | "demo"
                    | "all"
                    | "unitypackage"
                    | "zip"
                    | "7z"
                    | "rar"
                    | "txt"
                    | "json"
                    | "fbx"
                    | "blend"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "psd"
                    | "new"
                    | "final"
                    | "synthetic"
                    | "アバター"
                    | "衣装"
                    | "対応"
                    | "説明書"
                    | "更新"
                    | "無料"
                    | "文件"
                    | "模型"
                    | "版本"
            )
        })
        .filter(|word| {
            !["v", "ver", "version"].iter().any(|prefix| {
                word.strip_prefix(prefix)
                    .is_some_and(|tail| !tail.is_empty() && tail.chars().all(char::is_numeric))
            })
        })
        .map(str::to_owned)
        .collect()
}

fn ai_input_refs(input: &crate::ai_analysis::AnalysisInput) -> BTreeSet<String> {
    let mut refs: BTreeSet<_> = input
        .files
        .iter()
        .map(|file| file.evidence_ref.clone())
        .collect();
    if !input.title.trim().is_empty() {
        refs.insert("title".into());
    }
    if !input.source_tags.is_empty() {
        refs.insert("sourceTags".into());
    }
    if !input.description.trim().is_empty() {
        refs.insert("description".into());
    }
    refs
}

fn ai_snapshot(conn: &Connection, id: &str) -> Result<AiMetadataSnapshot> {
    let source = raw(conn, "product", id)?.context("catalogue.product_missing")?;
    let overlay = raw(conn, "overlay", id)?.unwrap_or(json!({"id":id}));
    let files = related(conn, "remote_file", id)?;
    let artifacts = related(conn, "artifact", id)?;
    let catalogue_fingerprint = ai_fingerprint(&source, &overlay, &files, &artifacts)?;
    let bounded_labels = |v: &Value| {
        strings(v)
            .into_iter()
            .map(|v| ai_text(v.trim(), 160))
            .filter(|v| !v.is_empty())
            .take(128)
            .collect()
    };
    let mut input = crate::ai_analysis::AnalysisInput {
        title: ai_text(
            overlay["title"]
                .as_str()
                .or(source["sourceTitle"].as_str())
                .or(source["title"].as_str())
                .or(source["name"].as_str())
                .unwrap_or(""),
            2048,
        ),
        source_tags: bounded_labels(source.get("sourceTags").unwrap_or(&source["tags"])),
        description: ai_text(
            source["sourceDescription"]
                .as_str()
                .or(source["description"].as_str())
                .unwrap_or(""),
            32 * 1024,
        ),
        rejected_tags: bounded_labels(&overlay["rejectedTags"]),
        files: vec![],
    };
    let mut names = std::collections::BTreeMap::new();
    let mut add = |file_id: &str, member: &str, name: &str| {
        if name.is_empty()
            || name.len() > 1024
            || name.contains(['\\', ':'])
            || name.chars().any(char::is_control)
            || name
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | ".."))
        {
            return;
        }
        names
            .entry(opaque_ai_file_ref(file_id, member))
            .or_insert_with(|| name.to_owned());
    };
    for file in &files {
        let name = file["sourceRelativePath"]
            .as_str()
            .unwrap_or(text(file, "name"));
        add(text(file, "id"), "", name);
    }
    for artifact in &artifacts {
        // Original remote/local descriptor names take precedence over managed prefixes.
        add(
            text(artifact, "fileId"),
            "",
            text(artifact, "relativePath")
                .rsplit('/')
                .next()
                .unwrap_or(""),
        );
        for member in artifact["extractedInventory"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if member["isDirectory"] != true {
                add(
                    text(artifact, "fileId"),
                    text(member, "relativePath"),
                    text(member, "relativePath"),
                );
            }
        }
    }
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort_by(|a, b| {
        ai_document_priority(&b.1)
            .cmp(&ai_document_priority(&a.1))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });
    input.files = names
        .into_iter()
        .take(64)
        .map(
            |(evidence_ref, file_name)| crate::ai_analysis::FileEvidence {
                evidence_ref,
                file_name,
                text_excerpt: String::new(),
                candidate_public_urls: vec![],
            },
        )
        .collect();
    // JSON escaping can exceed the byte budget even when individual strings fit.
    while serde_json::to_vec(&input)?.len() > 128 * 1024 {
        if input.files.pop().is_some() {
            continue;
        }
        if !input.description.is_empty() {
            input.description = ai_text(&input.description, input.description.len() / 2);
        } else if input.source_tags.pop().is_none() {
            input.rejected_tags.pop();
        }
    }
    Ok(AiMetadataSnapshot {
        input: crate::ai_analysis::normalize_input(input)?,
        catalogue_fingerprint,
        preview_fingerprint: ai_preview_fingerprint(&source, &overlay)?,
    })
}

fn validate_ai_overlay(value: &Value) -> Result<AiOverlay> {
    ensure!(
        serde_json::to_vec(value)?.len() <= 96 * 1024,
        "AI_INVALID_ANALYSIS"
    );
    ensure!(
        value["analysis"].get("category").is_some(),
        "AI_INVALID_ANALYSIS"
    );
    let mut ai: AiOverlay = serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("AI_INVALID_ANALYSIS"))?;
    ensure!(
        ai.evidence_refs
            .iter()
            .filter_map(|reference| reference.strip_prefix("i-"))
            .all(|hash| hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && ai.preview_fingerprint.is_some()),
        "AI_INVALID_ANALYSIS"
    );
    ensure!(
        ai.evidence_refs.len() <= 67
            && ai.evidence_refs.iter().all(|v| !v.is_empty()
                && v.len() <= 80
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))),
        "AI_INVALID_ANALYSIS"
    );
    for fingerprint in [&ai.input_fingerprint, &ai.catalogue_fingerprint]
        .into_iter()
        .chain(ai.evidence_fingerprint.as_ref())
        .chain(ai.preview_fingerprint.as_ref())
    {
        ensure!(
            fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
            "AI_INVALID_ANALYSIS"
        );
    }
    ensure!(
        ai.content_evidence_refs.len() <= 64
            && ai.content_evidence_refs.iter().all(
                |reference| reference.starts_with("e-") && ai.evidence_refs.contains(reference)
            ),
        "AI_INVALID_ANALYSIS"
    );
    ensure!(
        !ai.model.trim().is_empty()
            && ai.model.len() <= 160
            && !ai.model.chars().any(char::is_control)
            && chrono::DateTime::parse_from_rfc3339(&ai.generated_at).is_ok(),
        "AI_INVALID_ANALYSIS"
    );
    ensure!(
        ai.observed_source_urls.len() <= 8
            && ai
                .observed_source_urls
                .iter()
                .all(|v| crate::ai_analysis::public_product_url(v).as_deref() == Some(v)),
        "AI_INVALID_ANALYSIS"
    );
    ai.analysis = crate::ai_analysis::normalize_output(
        ai.analysis,
        &ai.evidence_refs.iter().cloned().collect(),
        &[],
    )?;
    Ok(ai)
}

fn validate_ai_dismissals(value: &Value) -> Result<()> {
    let values = value.as_array().context("AI_INVALID_ANALYSIS")?;
    ensure!(
        values.len() <= 128
            && values.iter().all(|v| v
                .as_str()
                .is_some_and(|v| crate::ai_analysis::public_product_url(v).as_deref() == Some(v))),
        "AI_INVALID_ANALYSIS"
    );
    Ok(())
}

pub(crate) fn default_preferences() -> Value {
    json!({"id":"app","autoDownloadAll":false,"downloadConcurrency":3,"extractConcurrency":1,"aiEnabled":false,"aiFeaturesEnabled":false,"aiScopes":default_ai_scopes(),"sensitiveMode":"blur","batchActions":default_batch_actions(),"cardColumns":["shop"],"listColumns":["shop","category","files","acquired","price"],"avatarAutomaticEnabled":{},"setupComplete":false})
}
fn default_ai_scopes() -> Value {
    json!({"title":true,"tags":true,"description":true,"files":true,"images":false})
}

const BATCH_ACTION_KEYS: [&str; 11] = [
    "edit",
    "collection",
    "unity",
    "export",
    "ai",
    "spending",
    "delete",
    "hide",
    "selectAll",
    "update",
    "download",
];

fn default_batch_actions() -> Value {
    Value::Object(
        BATCH_ACTION_KEYS
            .into_iter()
            .map(|key| (key.into(), json!(true)))
            .collect(),
    )
}

fn validate_ui_preferences(patch: &Value) -> Result<()> {
    if let Some(value) = patch.get("aiFeaturesEnabled") {
        ensure!(value.is_boolean(), "catalogue.invalid_preferences");
    }
    if let Some(value) = patch.get("sensitiveMode") {
        ensure!(
            matches!(value.as_str(), Some("blur" | "hide" | "show" | "only")),
            "catalogue.invalid_preferences"
        );
    }
    if let Some(value) = patch.get("batchActions") {
        let actions = value.as_object().context("catalogue.invalid_preferences")?;
        ensure!(actions.iter().all(|(key,value)| BATCH_ACTION_KEYS.contains(&key.as_str()) && value.is_boolean()),"catalogue.invalid_preferences");
    }
    for field in ["cardColumns", "listColumns"] {
        if let Some(value) = patch.get(field) {
            let values = value.as_array().context("catalogue.invalid_preferences")?;
            let mut seen = BTreeSet::new();
            ensure!(
                values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|column| [
                        "shop", "category", "files", "acquired", "price"
                    ]
                    .contains(&column)
                        && seen.insert(column))),
                "catalogue.invalid_preferences"
            );
        }
    }
    Ok(())
}

fn validate_ai_preferences(value: &Value) -> Result<()> {
    if let Some(enabled) = value.get("aiEnabled") {
        ensure!(enabled.is_boolean(), "catalogue.invalid_preferences");
    }
    if let Some(scopes) = value.get("aiScopes") {
        let scopes = scopes
            .as_object()
            .context("catalogue.invalid_preferences")?;
        ensure!(
            scopes.len() == 5
                && ["title", "tags", "description", "files", "images"]
                    .iter()
                    .all(|key| scopes.get(*key).is_some_and(Value::is_boolean)),
            "catalogue.invalid_preferences"
        );
    }
    Ok(())
}

fn ai_artifact_requires_projection(
    conn: &Connection,
    before: Option<&Value>,
    after: Option<&Value>,
) -> Result<bool> {
    if before.map(ai_artifact_facts) == after.map(ai_artifact_facts) {
        return Ok(false);
    }
    for value in before.into_iter().chain(after) {
        if raw(conn, "overlay", text(value, "productId"))?
            .is_some_and(|v| v["ai"].is_object() || v["aiTagOrganization"].is_object())
        {
            return Ok(true);
        }
    }
    Ok(false)
}
#[derive(Clone)]
pub struct Catalogue {
    inner: Arc<Mutex<Connection>>,
    work_gate: Arc<Mutex<WorkGate>>,
    dir: PathBuf,
    device_dir: PathBuf,
    device_id: String,
    library_root: Option<PathBuf>,
    library_lock: Option<Arc<crate::library_lock::LibraryLock>>,
    pending_move: Arc<Mutex<Option<(Arc<crate::library_lock::LibraryLock>, CatalogueWorkPermit)>>>,
    document_cache: Arc<Mutex<Option<automerge::AutoCommit>>>,
    document_preparation: Arc<Mutex<()>>,
    exchange_cache: Arc<Mutex<Option<(PathBuf, u64, Vec<String>)>>>,
    worker: Arc<worker::Worker>,
    is_sync_worker: bool,
}

#[derive(Default)]
struct WorkGate {
    active: usize,
    restoring: bool,
}

/// Registers catalogue-backed work for its entire lifetime, including waits and cleanup.
pub(crate) struct CatalogueWorkPermit {
    gate: Arc<Mutex<WorkGate>>,
    restore: bool,
}

impl Drop for CatalogueWorkPermit {
    fn drop(&mut self) {
        let mut state = self.gate.lock().unwrap_or_else(|error| error.into_inner());
        if self.restore {
            state.restoring = false;
        } else {
            state.active -= 1;
        }
    }
}
pub(super) fn normalized(s: &str) -> String {
    s.nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .trim()
        .to_string()
}
pub(super) fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
pub(super) fn text<'a>(v: &'a Value, field: &str) -> &'a str {
    v[field].as_str().unwrap_or("")
}
pub(super) fn raw(conn: &Connection, kind: &str, id: &str) -> Result<Option<Value>> {
    let data: Option<String> = conn
        .query_row(
            "SELECT body FROM entities WHERE kind=?1 AND id=?2",
            params![kind, id],
            |r| r.get(0),
        )
        .optional()?;
    data.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
pub(super) fn rows(conn: &Connection, kind: &str) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT body FROM entities WHERE kind=?1 ORDER BY id")?;
    let data = stmt
        .query_map([kind], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    data.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .collect()
}
pub(super) fn related(conn: &Connection, kind: &str, product: &str) -> Result<Vec<Value>> {
    let mut stmt =
        conn.prepare("SELECT body FROM entities WHERE kind=?1 AND product_id=?2 ORDER BY id")?;
    let data = stmt
        .query_map(params![kind, product], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    data.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .collect()
}
pub(super) fn effective_entitlements(conn: &Connection, product: &str) -> Result<Vec<Value>> {
    related(conn, "entitlement", product)
}
pub(super) fn set_raw(conn: &Connection, kind: &str, id: &str, value: &Value) -> Result<()> {
    conn.execute("INSERT INTO entities(kind,id,product_id,body) VALUES(?1,?2,?3,?4) ON CONFLICT(kind,id) DO UPDATE SET product_id=excluded.product_id,body=excluded.body",params![kind,id,text(value,"productId"),serde_json::to_string(value)?])?;
    Ok(())
}
fn kind_valid(kind: &str) -> Result<()> {
    ensure!(KINDS.contains(&kind), "catalogue.invalid_kind");
    Ok(())
}
fn validate(kind: &str, id: &str, value: &Value) -> Result<()> {
    kind_valid(kind)?;
    ensure!(
        !id.is_empty() && id.len() <= 2048 && value.is_object() && value["id"].as_str() == Some(id),
        "catalogue.invalid_record"
    );
    portable::validate_no_secrets(value)?;
    if kind == "preferences" {
        validate_ai_preferences(value)?;
    }
    if kind == "product" && value["provider"] == "local" {
        if let Some(manifest) = value.get("localManifest") {
            ensure!(
                manifest["complete"].is_boolean()
                    && manifest["fileIds"].as_array().is_some_and(|ids| ids
                        .iter()
                        .all(|id| id.as_str().is_some_and(|id| !id.is_empty())))
                    && manifest["directories"]
                        .as_array()
                        .is_some_and(|dirs| dirs.iter().all(Value::is_string)),
                "catalogue.invalid_local_manifest"
            );
            ensure!(
                manifest["complete"] != true || !strings(&manifest["fileIds"]).is_empty(),
                "catalogue.invalid_local_manifest"
            );
            for path in strings(&manifest["directories"]) {
                portable::validate_relative(&path)?;
            }
        }
    }
    if kind == "overlay" {
        for key in value.as_object().unwrap().keys() {
            ensure!(
                key == "id"
                    || key == "ai"
                    || key == "aiDismissedSourceUrls"
                    || key == "aiTagOrganization"
                    || EDITABLE.contains(&key.as_str()),
                "catalogue.invalid_overlay_field"
            );
        }
        let mut editable = value.clone();
        if let Some(ai) = editable.as_object_mut().unwrap().remove("ai") {
            validate_ai_overlay(&ai)?;
        }
        if let Some(urls) = editable
            .as_object_mut()
            .unwrap()
            .remove("aiDismissedSourceUrls")
        {
            validate_ai_dismissals(&urls)?;
        }
        if let Some(organization) = editable
            .as_object_mut()
            .unwrap()
            .remove("aiTagOrganization")
        {
            validate_ai_organization(&organization)?;
        }
        validate_patch(&editable)?;
    }
    if kind == "artifact" {
        for f in ["relativePath", "extractedPath", "productDirectory"] {
            if let Some(path) = value[f].as_str() {
                portable::validate_relative(path)?;
            }
        }
    }
    if kind == "remote_file" {
        if let Some(url) = value["sourceUrl"].as_str() {
            ensure!(
                url.starts_with("https://booth.pm/")
                    && !url.contains('?')
                    && !url.contains('#')
                    && !url.contains('@'),
                "catalogue.invalid_resolver"
            );
        }
    }
    Ok(())
}
fn validate_patch(patch: &Value) -> Result<()> {
    ensure!(patch.is_object(), "catalogue.invalid_patch");
    for (field, value) in patch.as_object().unwrap() {
        if field == "id" {
            continue;
        }
        ensure!(
            EDITABLE.contains(&field.as_str()),
            "catalogue.invalid_overlay_field"
        );
        match field.as_str() {
            "tags" | "ownedAvatars" | "rejectedTags" => ensure!(
                value
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string)),
                "catalogue.invalid_patch"
            ),
            "sensitive" | "thumbnailHidden" | "hidden" => {
                ensure!(value.is_boolean(), "catalogue.invalid_patch")
            }
            _ => ensure!(value.is_string(), "catalogue.invalid_patch"),
        }
        if field == "category" {
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
                .contains(&value.as_str().unwrap_or("")),
                "catalogue.invalid_category"
            );
        }
        if field == "thumbnailPath" {
            portable::validate_relative(value.as_str().unwrap_or(""))?;
        }
        if field == "sourceUrl" {
            validate_source_url(value.as_str().unwrap_or(""))?;
        }
        if field == "sourceThumbnailUrl" {
            validate_source_thumbnail_url(value.as_str().unwrap_or(""))?;
        }
    }
    Ok(())
}
pub(super) fn validate_source_url(value: &str) -> Result<()> {
    if !value.is_empty() {
        crate::native::normalize_public_url(value)?;
    }
    Ok(())
}
pub(super) fn validate_source_thumbnail_url(value: &str) -> Result<()> {
    if !value.is_empty() {
        crate::preview::parse_public_url(value)?;
    }
    Ok(())
}
impl Catalogue {
    pub fn open(data_dir: &Path) -> Result<Self> {
        Self::open_at(data_dir, data_dir, None, None)
    }
    fn open_at(
        data_dir: &Path,
        device_dir: &Path,
        library_root: Option<PathBuf>,
        library_lock: Option<Arc<crate::library_lock::LibraryLock>>,
    ) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        std::fs::create_dir_all(device_dir)?;
        let conn = Connection::open(data_dir.join("catalogue.sqlite3"))?;
        Self::from_connection(conn, data_dir, device_dir, library_root, library_lock)
    }
    pub(crate) fn open_setup(device_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(device_dir)?;
        Self::from_connection(
            Connection::open_in_memory()?,
            device_dir,
            device_dir,
            None,
            None,
        )
    }
    fn from_connection(
        mut conn: Connection,
        data_dir: &Path,
        device_dir: &Path,
        library_root: Option<PathBuf>,
        library_lock: Option<Arc<crate::library_lock::LibraryLock>>,
    ) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(if library_root.is_some() {
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"
        } else {
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"
        })?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(version <= 1, "catalogue.newer_schema");
        if version == 0 {
            let tx = conn.transaction()?;
            tx.execute_batch("CREATE TABLE entities(kind TEXT NOT NULL,id TEXT NOT NULL,product_id TEXT NOT NULL DEFAULT '',body TEXT NOT NULL CHECK(json_valid(body)),PRIMARY KEY(kind,id)); CREATE INDEX entities_product ON entities(kind,product_id);
            CREATE TABLE meta(key TEXT PRIMARY KEY,value BLOB NOT NULL);
            CREATE TABLE am_changes(seq INTEGER PRIMARY KEY AUTOINCREMENT,body BLOB NOT NULL); CREATE TABLE undo(token TEXT PRIMARY KEY,body TEXT NOT NULL);
            CREATE TABLE products(id TEXT PRIMARY KEY,body TEXT NOT NULL,category TEXT NOT NULL,status TEXT NOT NULL,sensitive INTEGER NOT NULL,downloaded INTEGER NOT NULL,title TEXT NOT NULL,acquired TEXT,price REAL,search TEXT NOT NULL,in_all INTEGER NOT NULL DEFAULT 0,summary TEXT NOT NULL);
            CREATE INDEX products_category ON products(category); CREATE INDEX products_status ON products(status); CREATE INDEX products_title ON products(title); CREATE INDEX products_acquired ON products(acquired); CREATE INDEX products_price ON products(price);
            CREATE TABLE search_grams(gram TEXT NOT NULL,product_id TEXT NOT NULL,PRIMARY KEY(gram,product_id)) WITHOUT ROWID;
            CREATE TABLE sync_outbox(seq INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,id TEXT NOT NULL,before_json TEXT,after_json TEXT);
            CREATE TABLE sync_trust(key TEXT PRIMARY KEY,digest TEXT NOT NULL);
            CREATE INDEX search_grams_product ON search_grams(product_id);
            CREATE INDEX entities_product_order ON entities(kind,product_id,id);
            CREATE INDEX entities_artifact_file ON entities(product_id,json_extract(body,'$.fileId'),id) WHERE kind='artifact';
            CREATE INDEX products_in_all ON products(in_all);
            CREATE INDEX products_downloaded ON products(downloaded);
            CREATE INDEX products_avatar_evidence ON products(id,json_extract(summary,'$.ownedAvatarEvidence')) WHERE json_array_length(summary,'$.ownedAvatarEvidence')>0;
            PRAGMA user_version=1;")?;
            tx.execute_batch("CREATE TRIGGER products_changed_insert AFTER INSERT ON products BEGIN INSERT INTO meta(key,value) VALUES('products_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;
        CREATE TRIGGER products_changed_update AFTER UPDATE ON products WHEN NEW.summary<>OLD.summary OR NEW.search<>OLD.search BEGIN INSERT INTO meta(key,value) VALUES('products_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;
        CREATE TRIGGER products_changed_delete AFTER DELETE ON products BEGIN INSERT INTO meta(key,value) VALUES('products_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;
        CREATE TRIGGER collections_changed_insert AFTER INSERT ON entities WHEN NEW.kind='collection' BEGIN INSERT INTO meta(key,value) VALUES('collections_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;
        CREATE TRIGGER collections_changed_update AFTER UPDATE ON entities WHEN NEW.kind='collection' AND NEW.body<>OLD.body BEGIN INSERT INTO meta(key,value) VALUES('collections_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;
        CREATE TRIGGER collections_changed_delete AFTER DELETE ON entities WHEN OLD.kind='collection' BEGIN INSERT INTO meta(key,value) VALUES('collections_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;")?;
            tx.commit()?;
        }
        // Existing files must already use this build's schema. Opening a profile
        // never upgrades source records or rebuilds the catalogue.
        conn.prepare("SELECT summary,in_all FROM products LIMIT 0")?;
        conn.prepare("SELECT seq,kind,id,before_json,after_json FROM sync_outbox LIMIT 0")?;
        projection::reconcile_ready_badges(&conn)?;

        let actor: Option<String> = conn
            .query_row(
                "SELECT CAST(value AS TEXT) FROM meta WHERE key='actor'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let actor = actor.unwrap_or_else(|| Uuid::new_v4().to_string());
        conn.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES('actor',?1)",
            [&actor],
        )?;
        if raw(&conn, "preferences", "app")?.is_none() {
            let prefs = default_preferences();
            set_raw(&conn, "preferences", "app", &prefs)?;
        }
        let this = Self {
            inner: Arc::new(Mutex::new(conn)),
            work_gate: Arc::new(Mutex::new(WorkGate::default())),
            dir: data_dir.to_path_buf(),
            device_dir: device_dir.to_path_buf(),
            device_id: shared::device_id(device_dir)?,
            library_root,
            library_lock,
            pending_move: Arc::new(Mutex::new(None)),
            document_cache: Arc::new(Mutex::new(None)),
            document_preparation: Arc::new(Mutex::new(())),
            exchange_cache: Arc::new(Mutex::new(None)),
            worker: Arc::new(worker::Worker::default()),
            is_sync_worker: false,
        };
        // Durable document is saved atomically with projections; no startup network is needed.
        Ok(this)
    }
    pub fn data_dir(&self) -> PathBuf {
        self.dir.clone()
    }
    pub(crate) fn begin_catalogue_work(&self) -> Result<CatalogueWorkPermit> {
        self.check_library_access()?;
        let mut state = self
            .work_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        ensure!(!state.restoring, "storage_busy");
        state.active += 1;
        Ok(CatalogueWorkPermit {
            gate: self.work_gate.clone(),
            restore: false,
        })
    }
    pub(crate) fn begin_restore_work(&self) -> Result<CatalogueWorkPermit> {
        self.check_library_access()?;
        let mut state = self
            .work_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        ensure!(!state.restoring && state.active == 0, "storage_busy");
        state.restoring = true;
        drop(state);
        let permit = CatalogueWorkPermit {
            gate: self.work_gate.clone(),
            restore: true,
        };
        // Queued work has no live permit yet, but can still be picked up later.
        ensure!(
            !self
                .list("job")?
                .iter()
                .any(|job| { matches!(job["state"].as_str(), Some("queued" | "running")) }),
            "storage_busy"
        );
        Ok(permit)
    }
    pub fn get(&self, kind: &str, id: &str) -> Result<Option<Value>> {
        kind_valid(kind)?;
        raw(
            &*self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
            kind,
            id,
        )
    }
    /// Only allowlisted product text and opaque, relative file evidence names.
    pub fn collect_ai_metadata(&self, product_id: &str) -> Result<AiMetadataSnapshot> {
        ai_snapshot(
            &*self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
            product_id,
        )
    }
    pub fn ai_preview_source(&self, product_id: &str) -> Result<AiPreviewSource> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let source = raw(&conn, "product", product_id)?.context("catalogue.product_missing")?;
        let overlay = raw(&conn, "overlay", product_id)?.unwrap_or(json!({"id":product_id}));
        let mut result = AiPreviewSource {
            preview_fingerprint: ai_preview_fingerprint(&source, &overlay)?,
            managed_relative_path: None,
            public_url: None,
        };
        // Keep the same priority and explicit-empty override behavior as projection.
        if overlay["thumbnailHidden"] == true {
            return Ok(result);
        }
        if let Some(path) = overlay["thumbnailPath"].as_str() {
            portable::validate_relative(path)?;
            let relative = path.replace('\\', "/");
            ensure!(relative.starts_with("media/"), "preview_path_refused");
            result.managed_relative_path = Some(relative);
        } else if let Some(url) = overlay
            .get("sourceThumbnailUrl")
            .unwrap_or(&source["previewUrl"])
            .as_str()
            .filter(|url| !url.is_empty())
        {
            result.public_url = Some(
                crate::ai_suggestions::public_thumbnail_url(url)
                    .ok_or_else(|| anyhow::anyhow!("preview_url_refused"))?
                    .to_string(),
            );
        }
        Ok(result)
    }
    /// Durable live source rows; sync tombstones remove these rows on projection.
    pub fn product_ids(&self) -> Result<Vec<String>> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let mut statement =
            conn.prepare("SELECT id FROM entities WHERE kind='product' ORDER BY id")?;
        let ids = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    /// Explicit chat retrieval: bounded effective metadata, without account,
    /// acquisition, note, file paths or original source payloads.
    pub fn ai_recommendation_candidates(
        &self,
        query: &str,
        category: Option<&str>,
        avatar_id: Option<&str>,
        context_ids: &[String],
    ) -> Result<Vec<Value>> {
        let terms = ai_match_tokens(query);
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let mut stmt = conn.prepare("SELECT id,json_object('title',json_extract(summary,'$.title'),'category',category,'provider',json_extract(summary,'$.provider'),'store',json_extract(summary,'$.store'),'tags',json_extract(summary,'$.tags'),'ownedAvatars',json_extract(summary,'$.ownedAvatars'),'listingAvatars',json_extract(summary,'$.listingAvatars'),'sourceUrl',json_extract(summary,'$.sourceUrl'),'free',CASE WHEN price=0 THEN 1 ELSE 0 END),EXISTS(SELECT 1 FROM json_each(summary,'$.ownedAvatarEvidence') e WHERE json_extract(e.value,'$.avatarId')=?1 OR json_extract(e.value,'$.avatar')=?1) FROM products WHERE COALESCE(json_extract(summary,'$.hidden'),0)=0 ORDER BY id")?;
        let mut best: Vec<(usize, String, Value)> = vec![];
        for row in stmt.query_map([avatar_id.unwrap_or("")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })? {
            let (id, body, avatar_match) = row?;
            let mut value: Value = serde_json::from_str(&body)?;
            let context = context_ids.contains(&id);
            if !context
                && (category.is_some_and(|c| text(&value, "category") != c)
                    || avatar_id.is_some() && !avatar_match)
            {
                continue;
            }
            let title = ai_text(text(&value, "title"), 512);
            let tags: Vec<String> = strings(&value["tags"])
                .into_iter()
                .take(32)
                .map(|v| ai_text(&v, 160))
                .collect();
            let haystack = ai_match_tokens(&format!(
                "{} {} {}",
                title,
                text(&value, "category"),
                tags.join(" ")
            ));
            let score = terms.intersection(&haystack).count() * 10 + usize::from(context) * 1000;
            let provider = match text(&value, "provider") {
                "booth" | "gumroad" | "jinxxy" | "local" => text(&value, "provider").to_owned(),
                _ => match text(&value, "store") {
                    "BOOTH" => "booth",
                    "Gumroad" => "gumroad",
                    "Jinxxy" => "jinxxy",
                    "Local Files" => "local",
                    _ => "unknown",
                }
                .into(),
            };
            let url = crate::ai_analysis::public_product_url(text(&value, "sourceUrl"));
            value = json!({"productId":id,"title":title,"category":value["category"],"provider":provider,"tags":tags,
                "ownedAvatars":strings(&value["ownedAvatars"]).into_iter().take(24).map(|label| ai_text(&label,160)).collect::<Vec<_>>(),
                "listingAvatars":strings(&value["listingAvatars"]).into_iter().take(24).map(|label| ai_text(&label,160)).collect::<Vec<_>>(),"url":url,"free":value["free"]==1});
            best.push((score, id, value));
            best.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            best.truncate(48);
        }
        Ok(best.into_iter().map(|(_, _, v)| v).collect())
    }

    /// Public metadata from already-synced products is candidate context only.
    /// No accounts, prices, receipts, remote fetch, or ownership conversion.
    pub fn ai_source_candidates(
        &self,
        input: &crate::ai_analysis::AnalysisInput,
    ) -> Result<Vec<crate::ai_analysis::FileEvidence>> {
        let input = crate::ai_analysis::normalize_input(input.clone())?;
        let samples: Vec<_> = std::iter::once(input.title.as_str())
            .chain(
                input
                    .files
                    .iter()
                    .map(|f| f.file_name.rsplit('/').next().unwrap_or("")),
            )
            .map(ai_match_tokens)
            .filter(|tokens| !tokens.is_empty())
            .collect();
        if samples.is_empty() {
            return Ok(vec![]);
        }
        let existing: BTreeSet<_> = input
            .files
            .iter()
            .flat_map(|f| f.candidate_public_urls.iter().cloned())
            .collect();
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let mut stmt=conn.prepare("SELECT COALESCE(json_extract(body,'$.provider'),json_extract(body,'$.store'),''),COALESCE(json_extract(body,'$.sourceTitle'),json_extract(body,'$.title'),''),COALESCE(json_extract(body,'$.sourceUrl'),'') FROM entities WHERE kind='product' AND COALESCE(json_extract(body,'$.custom'),0)<>1 ORDER BY id")?;
        let mut best: Vec<(usize, String, String)> = vec![];
        for row in stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (provider, title, url) = row?;
            let provider = provider.to_ascii_lowercase();
            if !matches!(provider.as_str(), "booth" | "gumroad" | "jinxxy") {
                continue;
            }
            let Some(url) = crate::ai_analysis::public_product_url(&url) else {
                continue;
            };
            if existing.contains(&url) {
                continue;
            }
            let host = url::Url::parse(&url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_default();
            if !(match provider.as_str() {
                "booth" => host == "booth.pm" || host.ends_with(".booth.pm"),
                "gumroad" => host.ends_with(".gumroad.com"),
                "jinxxy" => host == "jinxxy.com" || host.ends_with(".jinxxy.com"),
                _ => false,
            }) {
                continue;
            }
            let title = ai_text(&title, 2048);
            let tokens = ai_match_tokens(&title);
            let score = samples
                .iter()
                .map(|sample| {
                    let shared: Vec<_> = tokens.intersection(sample).collect();
                    if shared.len() >= 2 {
                        return shared.len() * 4;
                    }
                    if shared.iter().any(|word| word.chars().count() >= 6) {
                        return 4;
                    }
                    // Covers unspaced CJK names and filename prefixes/suffixes.
                    tokens
                        .iter()
                        .filter(|word| {
                            word.chars().count() >= 6
                                || (!word.is_ascii() && word.chars().count() >= 4)
                        })
                        .any(|word| {
                            sample.iter().any(|s| {
                                s.contains(word.as_str())
                                    || word.contains(s.as_str()) && s.chars().count() >= 6
                            })
                        })
                        .then_some(3)
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            if score == 0 || best.iter().any(|(_, known, _)| known == &url) {
                continue;
            }
            best.push((score, url, title));
            best.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            best.truncate(8);
        }
        best.into_iter()
            .enumerate()
            .map(|(index, (_, url, title))| {
                Ok(crate::ai_analysis::FileEvidence {
                    evidence_ref: format!("c-{}", ai_digest(&url)?),
                    file_name: format!("catalogue-candidate-{}.txt", index + 1),
                    text_excerpt: format!("Possible catalogue match: {title}"),
                    candidate_public_urls: vec![url],
                })
            })
            .collect()
    }

    pub fn publish_ai_analysis(
        &self,
        product_id: &str,
        expected_catalogue_fingerprint: &str,
        input: &crate::ai_analysis::AnalysisInput,
        result: &crate::ai_analysis::AnalysisOutput,
        model: &str,
    ) -> Result<()> {
        self.publish_ai_analysis_with_preview(
            product_id,
            expected_catalogue_fingerprint,
            input,
            result,
            model,
            None,
        )
    }

    pub fn publish_ai_analysis_with_preview(
        &self,
        product_id: &str,
        expected_catalogue_fingerprint: &str,
        input: &crate::ai_analysis::AnalysisInput,
        result: &crate::ai_analysis::AnalysisOutput,
        model: &str,
        preview_fingerprint: Option<&str>,
    ) -> Result<()> {
        let input = crate::ai_analysis::normalize_input(input.clone())?;
        ensure!(
            !input
                .files
                .iter()
                .any(|file| file.evidence_ref.starts_with("i-"))
                || preview_fingerprint.is_some(),
            "AI_INVALID_ANALYSIS"
        );
        self.mutate(|tx, projections| {
            let current = ai_snapshot(tx, product_id)?;
            ensure!(
                current.catalogue_fingerprint == expected_catalogue_fingerprint,
                "AI_INPUT_STALE"
            );
            ensure!(
                preview_fingerprint.is_none_or(|expected| expected == current.preview_fingerprint),
                "AI_INPUT_STALE"
            );
            let mut overlay = raw(tx, "overlay", product_id)?.unwrap_or(json!({"id":product_id}));
            let known = ai_input_refs(&input);
            let analysis = crate::ai_analysis::normalize_output(
                result.clone(),
                &known,
                &strings(&overlay["rejectedTags"]),
            )?;
            let observed: BTreeSet<_> = analysis
                .source_url_candidates
                .iter()
                .filter(|candidate| {
                    input.files.iter().any(|file| {
                        file.evidence_ref.starts_with("e-")
                            && candidate.evidence_refs.contains(&file.evidence_ref)
                            && file.candidate_public_urls.contains(&candidate.url)
                    })
                })
                .map(|candidate| candidate.url.clone())
                .collect();
            let ai = AiOverlay {
                analysis,
                evidence_refs: known.into_iter().collect(),
                observed_source_urls: observed.into_iter().collect(),
                catalogue_fingerprint: current.catalogue_fingerprint,
                input_fingerprint: ai_digest(&input)?,
                preview_fingerprint: preview_fingerprint.map(str::to_owned),
                evidence_fingerprint: Some(ai_evidence_fingerprint(
                    &raw(tx, "product", product_id)?.context("catalogue.product_missing")?,
                    &overlay,
                    &related(tx, "remote_file", product_id)?,
                    &related(tx, "artifact", product_id)?,
                    &ai_input_refs(&input).into_iter().collect::<Vec<_>>(),
                    &input
                        .files
                        .iter()
                        .filter(|file| {
                            file.evidence_ref.starts_with("e-") && !file.text_excerpt.is_empty()
                        })
                        .map(|file| file.evidence_ref.clone())
                        .collect::<Vec<_>>(),
                )?),
                content_evidence_refs: input
                    .files
                    .iter()
                    .filter(|file| {
                        file.evidence_ref.starts_with("e-") && !file.text_excerpt.is_empty()
                    })
                    .map(|file| file.evidence_ref.clone())
                    .collect(),
                model: model.to_owned(),
                generated_at: chrono::Utc::now().to_rfc3339(),
            };
            overlay["ai"] = serde_json::to_value(ai)?;
            validate("overlay", product_id, &overlay)?;
            Self::write(tx, projections, "overlay", product_id, Some(overlay))
        })
    }

    pub fn dismiss_ai_source_url_candidate(&self, product_id: &str, url: &str) -> Result<()> {
        let url = crate::ai_analysis::public_product_url(url).context("AI_INVALID_SOURCE_URL")?;
        self.mutate(|tx, projections| {
            ensure!(
                raw(tx, "product", product_id)?.is_some(),
                "catalogue.product_missing"
            );
            let mut overlay = raw(tx, "overlay", product_id)?.unwrap_or(json!({"id":product_id}));
            let mut dismissed: BTreeSet<_> = strings(&overlay["aiDismissedSourceUrls"])
                .into_iter()
                .collect();
            dismissed.insert(url);
            overlay["aiDismissedSourceUrls"] = json!(dismissed);
            validate("overlay", product_id, &overlay)?;
            Self::write(tx, projections, "overlay", product_id, Some(overlay))
        })
    }
    /// Explicit organization changes displayed source labels, never raw evidence.
    pub fn publish_ai_tag_organization(
        &self,
        product_id: &str,
        expected_catalogue_fingerprint: &str,
        aliases: &[crate::ai_analysis::TagAliasSuggestion],
        model: &str,
    ) -> Result<()> {
        self.mutate(|tx, projections| {
            let current = ai_snapshot(tx, product_id)?;
            ensure!(
                current.catalogue_fingerprint == expected_catalogue_fingerprint,
                "AI_INPUT_STALE"
            );
            let source = raw(tx, "product", product_id)?.context("catalogue.product_missing")?;
            let available: BTreeSet<_> =
                strings(source.get("sourceTags").unwrap_or(&source["tags"]))
                    .iter()
                    .map(|v| crate::taxonomy::canonical_tag(v))
                    .collect();
            let mut overlay = raw(tx, "overlay", product_id)?.unwrap_or(json!({"id":product_id}));
            let mut aliases =
                normalize_ai_aliases(aliases.to_vec(), &strings(&overlay["rejectedTags"]))?;
            aliases
                .retain(|alias| available.contains(&crate::taxonomy::canonical_tag(&alias.from)));
            overlay["aiTagOrganization"] = serde_json::to_value(AiTagOrganization {
                aliases,
                catalogue_fingerprint: current.catalogue_fingerprint,
                evidence_fingerprint: Some(ai_evidence_fingerprint(
                    &source,
                    &overlay,
                    &[],
                    &[],
                    &["sourceTags".into()],
                    &[],
                )?),
                model: model.to_owned(),
                generated_at: chrono::Utc::now().to_rfc3339(),
            })?;
            validate("overlay", product_id, &overlay)?;
            Self::write(tx, projections, "overlay", product_id, Some(overlay))
        })
    }

    pub fn list(&self, kind: &str) -> Result<Vec<Value>> {
        kind_valid(kind)?;
        rows(
            &*self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
            kind,
        )
    }
    pub fn related(&self, kind: &str, product_id: &str) -> Result<Vec<Value>> {
        kind_valid(kind)?;
        related(
            &*self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
            kind,
            product_id,
        )
    }
    pub fn effective_entitlements(&self, product_id: &str) -> Result<Vec<Value>> {
        effective_entitlements(
            &*self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
            product_id,
        )
    }
    pub fn put(&self, kind: &str, id: &str, value: Value) -> Result<()> {
        validate(kind, id, &value)?;
        self.mutate(|tx, projections| {
            Self::write(tx, projections, kind, id, Some(value))?;
            Ok(())
        })
    }
    /// Native ingestion only: validate first and project each changed product once.
    pub fn put_many(&self, records: Vec<(String, String, Value)>) -> Result<()> {
        for (kind, id, value) in &records {
            validate(kind, id, value)?;
        }
        self.mutate(|tx, projections| Self::write_many(tx, projections, records))
    }
    /// Commit a bounded ingestion plan only if every raw input still matches.
    /// A stale plan changes neither rows, CRDT history nor the local revision.
    pub fn put_many_if_unchanged(
        &self,
        records: Vec<(String, String, Value)>,
        expected: Vec<(String, String, Option<Value>)>,
    ) -> Result<bool> {
        for (kind, id, value) in &records {
            validate(kind, id, value)?;
        }
        for (kind, id, _) in &expected {
            kind_valid(kind)?;
            ensure!(!id.is_empty(), "catalogue.invalid_id");
        }
        self.mutate(|tx, projections| {
            for (kind, id, value) in expected {
                if raw(tx, &kind, &id)? != value {
                    return Ok(false);
                }
            }
            Self::write_many(tx, projections, records)?;
            Ok(true)
        })
    }
    fn write_many(
        tx: &Transaction<'_>,
        projections: &mut projection::ProjectionChanges,
        records: Vec<(String, String, Value)>,
    ) -> Result<()> {
        let mut affected = BTreeSet::new();
        let mut full = BTreeSet::new();
        let mut local_files = BTreeSet::new();
        let mut roots_changed = false;
        for (kind, id, value) in records {
            let before = raw(tx, &kind, &id)?;
            if before.as_ref() == Some(&value) {
                continue;
            }
            let mut record_affected = BTreeSet::new();
            Self::collect_projection_dependencies(
                tx,
                &kind,
                &id,
                before.as_ref(),
                Some(&value),
                projections,
                &mut record_affected,
            )?;
            if SYNC_KINDS.contains(&kind.as_str()) {
                Self::enqueue_sync(tx, &kind, &id, before.as_ref(), Some(&value))?;
            }
            if kind == "product" || kind == "overlay" {
                record_affected.insert(id.clone());
            }
            for v in before.iter().chain(std::iter::once(&value)) {
                if let Some(p) = v["productId"].as_str() {
                    if Self::projects_product(&kind, before.as_ref(), Some(&value)) {
                        record_affected.insert(p.to_owned());
                    }
                }
            }
            if !matches!(kind.as_str(), "job" | "artifact" | "root")
                || (kind == "artifact"
                    && ai_artifact_requires_projection(tx, before.as_ref(), Some(&value))?)
            {
                full.extend(record_affected.iter().cloned());
            }
            if kind == "artifact" {
                for record in before.iter().chain(std::iter::once(&value)) {
                    if let Some(file) = record["fileId"].as_str() {
                        local_files.insert(file.to_owned());
                    }
                }
            }
            roots_changed |= kind == "root";
            affected.extend(record_affected);
            set_raw(tx, &kind, &id, &value)?;
        }
        for id in affected {
            if full.contains(&id) {
                projections.merge(projection::refresh(tx, &id)?);
            } else {
                projection::refresh_local(tx, &id, &local_files, roots_changed)?;
            }
        }
        Ok(())
    }
    pub fn delete(&self, kind: &str, id: &str) -> Result<()> {
        kind_valid(kind)?;
        self.mutate(|tx, projections| Self::write(tx, projections, kind, id, None))
    }
    fn document_version(conn: &Connection) -> Result<(String, u64, u64)> {
        let actor: String = conn.query_row(
            "SELECT CAST(value AS TEXT) FROM meta WHERE key='actor'",
            [],
            |r| r.get(0),
        )?;
        let generation = conn.query_row("SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='sync_revision'),0),COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='revision'),0)", [], |r| Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?)))?;
        Ok((actor, generation.0, 0))
    }
    fn document_parts(
        conn: &Connection,
    ) -> Result<((String, u64, u64), Vec<(Vec<u8>, Option<String>)>)> {
        let version = Self::document_version(conn)?;
        let bytes: Option<Vec<u8>> = conn
            .query_row("SELECT value FROM meta WHERE key='automerge'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let digest: Option<String> = conn
            .query_row(
                "SELECT digest FROM sync_trust WHERE key='base'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let mut parts = vec![(bytes.unwrap_or_default(), digest)];
        let mut stmt = conn.prepare("SELECT c.body,t.digest FROM am_changes c LEFT JOIN sync_trust t ON t.key='delta:' || c.seq ORDER BY c.seq")?;
        for part in stmt.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?))
        })? {
            parts.push(part?);
        }
        Ok((version, parts))
    }
    fn assemble_document(parts: Vec<(Vec<u8>, Option<String>)>) -> (Vec<u8>, bool) {
        let size = parts.iter().map(|(bytes, _)| bytes.len()).sum::<usize>();
        let mut parts = parts.into_iter();
        let (mut bytes, digest) = parts.next().unwrap_or_default();
        let mut trusted = digest.is_some_and(|digest| digest == Self::digest(&bytes));
        bytes.reserve(size.saturating_sub(bytes.len()));
        for (chunk, digest) in parts {
            trusted = trusted && digest.is_some_and(|digest| digest == Self::digest(&chunk));
            bytes.extend(chunk);
        }
        (bytes, trusted)
    }
    #[cfg(test)]
    fn document_snapshot(conn: &Connection) -> Result<((String, u64, u64), Vec<u8>, bool)> {
        let (version, parts) = Self::document_parts(conn)?;
        let (bytes, trusted) = Self::assemble_document(parts);
        Ok((version, bytes, trusted))
    }
    fn install_document(
        &self,
        version: &(String, u64, u64),
        mut doc: automerge::AutoCommit,
        checkpoint: bool,
    ) -> Result<bool> {
        let checkpoint = if checkpoint {
            let bytes = doc.save();
            let digest = Self::digest(&bytes);
            Some((bytes, digest, sync::conflicts(&doc)?))
        } else {
            None
        };
        let mut conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let mut cache = self
            .document_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        if cache.is_some() {
            return Ok(true);
        }
        if &Self::document_version(&conn)? != version {
            return Ok(false);
        }
        if let Some((bytes, digest, conflicts)) = checkpoint {
            // First untrusted load validates every operation exactly once. Persist a
            // complete checkpoint and digests only after successful validation.
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('automerge',?1)",
                [&bytes],
            )?;
            tx.execute_batch("DELETE FROM am_changes; DELETE FROM sync_trust;")?;
            tx.execute(
                "INSERT INTO sync_trust(key,digest) VALUES('base',?1)",
                [digest],
            )?;
            let previous = self.current_conflicts(&tx)?;
            Self::save_conflicts(&tx, &conflicts)?;
            if previous != conflicts {
                Self::bump_revision(&tx)?;
            }
            tx.commit()?;
        }
        *cache = Some(doc);
        Ok(true)
    }
    /// Returns count-only copy/decode timings for the explicit synthetic scale
    /// runner. Parsing never holds SQLite or the document-cache mutex.
    fn ensure_document(&self) -> Result<(u128, u128)> {
        self.prepare_document_impl(|| {}, sync::load_doc, true)
    }
    #[cfg(test)]
    fn prepare_document(
        &self,
        before_wait: impl FnMut(),
        load: impl FnMut(&[u8], &str) -> Result<automerge::AutoCommit>,
    ) -> Result<(u128, u128)> {
        self.prepare_document_impl(before_wait, load, false)
    }
    fn prepare_document_impl(
        &self,
        mut before_wait: impl FnMut(),
        mut load: impl FnMut(&[u8], &str) -> Result<automerge::AutoCommit>,
        allow_trusted: bool,
    ) -> Result<(u128, u128)> {
        {
            let conn = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            let cache = self
                .document_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            if cache.is_some() {
                return Ok((0, 0));
            }
            drop(cache);
            drop(conn);
        }
        // Cold callers wait without SQLite/cache locks; the winner alone loads
        // history. The loop rechecks the cache after acquiring this shared gate.
        before_wait();
        let _preparation = self
            .document_preparation
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let mut copy_us = 0;
        let mut decode_us = 0;
        loop {
            let started = std::time::Instant::now();
            let (version, parts) = {
                let conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                let cache = self
                    .document_cache
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                if cache.is_some() {
                    return Ok((copy_us, decode_us));
                }
                Self::document_parts(&conn)?
            };
            copy_us += started.elapsed().as_micros();
            let started = std::time::Instant::now();
            // Hashing and contiguous allocation never hold the foreground SQL mutex.
            let (bytes, trusted) = Self::assemble_document(parts);
            // Incremental loading can accept a truncated prefix. Strict complete
            // loading plus dependency/history validation must precede trust.
            let doc = if allow_trusted && trusted {
                sync::load_trusted_doc(&bytes, &version.0)?
            } else {
                load(&bytes, &version.0)?
            };
            decode_us += started.elapsed().as_micros();
            if self.install_document(&version, doc, !trusted)? {
                return Ok((copy_us, decode_us));
            }
        }
    }
    fn lock_document(
        &self,
    ) -> Result<(
        std::sync::MutexGuard<'_, Connection>,
        std::sync::MutexGuard<'_, Option<automerge::AutoCommit>>,
    )> {
        loop {
            self.flush_sync_outbox()?;
            let conn = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            let cache = self
                .document_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            if cache.is_some()
                && conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM sync_outbox)", [], |row| {
                    row.get::<_, bool>(0)
                })?
            {
                return Ok((conn, cache));
            }
            // A failed mutation may invalidate the cache between preparation
            // and locking. Retry against its unchanged durable history.
        }
    }
    fn current_conflicts(&self, conn: &Connection) -> Result<Vec<Value>> {
        if let Some(body) = conn
            .query_row(
                "SELECT CAST(value AS TEXT) FROM meta WHERE key='conflicts'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(serde_json::from_str(&body)?);
        }
        Ok(vec![])
    }
    fn save_conflicts(conn: &Connection, conflicts: &[Value]) -> Result<()> {
        conn.execute("INSERT INTO meta(key,value) VALUES('conflicts',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(conflicts)?])?;
        Ok(())
    }
    fn mutate<T>(
        &self,
        run: impl FnOnce(&Transaction<'_>, &mut projection::ProjectionChanges) -> Result<T>,
    ) -> Result<T> {
        self.mutate_scoped(false, run)
    }
    fn mutate_scoped<T>(
        &self,
        collection_only: bool,
        run: impl FnOnce(&Transaction<'_>, &mut projection::ProjectionChanges) -> Result<T>,
    ) -> Result<T> {
        self.check_library_access()?;
        let mut conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        let tx = conn.transaction()?;
        let before = tx.total_changes();
        let mut projections = projection::ProjectionChanges::default();
        let result = run(&tx, &mut projections)?;
        Self::reconcile_projection(&tx, &projections)?;
        if tx.total_changes() != before {
            Self::bump_revision_scoped(&tx, collection_only)?;
        }
        tx.commit()?;
        drop(conn);
        self.wake_sync_worker();
        Ok(result)
    }
    fn enqueue_sync(
        conn: &Connection,
        kind: &str,
        id: &str,
        before: Option<&Value>,
        after: Option<&Value>,
    ) -> Result<()> {
        sync::validate_entity(kind, id, after)?;
        conn.execute(
            "INSERT INTO sync_outbox(kind,id,before_json,after_json) VALUES(?1,?2,?3,?4)",
            params![
                kind,
                id,
                before.map(serde_json::to_string).transpose()?,
                after.map(serde_json::to_string).transpose()?
            ],
        )?;
        Ok(())
    }
    fn mutate_sync<T>(
        &self,
        run: impl FnOnce(
            &Transaction<'_>,
            &mut automerge::AutoCommit,
            &mut projection::ProjectionChanges,
        ) -> Result<T>,
    ) -> Result<T> {
        self.check_library_access()?;
        let (mut conn, mut cache) = self.lock_document()?;
        let mut doc = cache.take().unwrap();
        // Cache entry history is already durable. Save only changes made by
        // this transaction, without serializing old history to prime a cursor.
        let before_heads = doc.get_heads();
        let tx = conn.transaction()?;
        let before_changes = tx.total_changes();
        let mut projections = projection::ProjectionChanges::default();
        let result = run(&tx, &mut doc, &mut projections)?;
        Self::reconcile_projection(&tx, &projections)?;
        let bytes = doc.save_after(&before_heads);
        let changed = before_changes != tx.total_changes() || !bytes.is_empty();
        if !bytes.is_empty() {
            let previous: Option<String> = tx
                .query_row(
                    "SELECT CAST(value AS TEXT) FROM meta WHERE key='conflicts'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let previous: Vec<Value> = previous
                .map(|v| serde_json::from_str(&v))
                .transpose()?
                .unwrap_or_default();
            Self::save_conflicts(&tx, &sync::remaining_conflicts(&doc, &previous)?)?;
            Self::persist_delta(&tx, &bytes)?;
            tx.execute("INSERT INTO meta(key,value) VALUES('sync_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",[])?;
        }
        if changed {
            Self::bump_revision(&tx)?;
        }
        tx.commit()?;
        *cache = Some(doc);
        Ok(result)
    }
    fn bump_revision(conn: &Connection) -> Result<()> {
        Self::bump_revision_scoped(conn, false)
    }
    fn bump_revision_scoped(conn: &Connection, collection_only: bool) -> Result<()> {
        // Existing databases start from their last broad revision. A collection
        // edit must never hide an earlier source/storage change from the ticker.
        conn.execute("INSERT OR IGNORE INTO meta(key,value) VALUES('noncollection_revision',COALESCE((SELECT value FROM meta WHERE key='revision'),0))", [])?;
        conn.execute("INSERT INTO meta(key,value) VALUES('revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",[])?;
        if !collection_only {
            conn.execute("UPDATE meta SET value=(SELECT value FROM meta WHERE key='revision') WHERE key='noncollection_revision'", [])?;
        }
        Ok(())
    }
    /// Global revision and the latest revision requiring a full catalogue refresh.
    pub fn change_revisions(&self) -> Result<(u64, u64)> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        Ok(conn.query_row("SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='revision'),0),COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='noncollection_revision'),(SELECT CAST(value AS INTEGER) FROM meta WHERE key='revision'),0)", [], |row| Ok((row.get(0)?, row.get(1)?)))?)
    }
    /// (state, effective products, collections); callers can refresh only their domain.
    pub fn change_domains(&self) -> Result<(u64, u64, u64)> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        Ok(conn.query_row("SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='revision'),0),COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='products_revision'),0),COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='collections_revision'),0)", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)
    }
    /// Explicit history barrier; foreground SQLite edits never call this.
    pub fn prepare_sync_document(&self) -> Result<()> {
        self.on_sync_worker(|catalogue| catalogue.flush_sync_outbox())
    }
    pub fn revision(&self) -> Result<u64> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        Ok(conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key='revision'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }
    fn reconcile_projection(
        conn: &Connection,
        changes: &projection::ProjectionChanges,
    ) -> Result<()> {
        avatars::remember_source_aliases(conn, &changes.source_aliases)?;
        if !changes.identities_changed && changes.tags.is_empty() {
            return Ok(());
        }
        let fresh = if changes.identities_changed || changes.prepared_resolver.is_none() {
            Some(avatars::tag_resolver(conn)?)
        } else {
            None
        };
        let resolver = fresh
            .as_ref()
            .or(changes.prepared_resolver.as_ref())
            .unwrap();
        projection::reconcile_tags(
            conn,
            resolver,
            if changes.identities_changed {
                None
            } else {
                Some(&changes.tags)
            },
        )
    }
    fn collect_projection_dependencies(
        conn: &Connection,
        kind: &str,
        id: &str,
        before: Option<&Value>,
        after: Option<&Value>,
        changes: &mut projection::ProjectionChanges,
        affected: &mut BTreeSet<String>,
    ) -> Result<()> {
        if kind == "job" && projection::job_changes_product(before, after) {
            for job in before.into_iter().chain(after) {
                if job["type"] == "storage:import_local" {
                    affected.extend(strings(&job["productIds"]));
                }
            }
        }
        if kind == "preferences" {
            fn names(value: Option<&Value>) -> Value {
                let mut result = Map::new();
                if let Some(value) = value {
                    for (id, settings) in value["avatarSettings"].as_object().into_iter().flatten()
                    {
                        let mut named = Map::new();
                        for field in ["originalName", "englishName", "aliases"] {
                            if let Some(value) = settings.get(field) {
                                named.insert(field.to_owned(), value.clone());
                            }
                        }
                        if !named.is_empty() {
                            result.insert(id.clone(), Value::Object(named));
                        }
                    }
                }
                Value::Object(result)
            }
            changes.identities_changed |= names(before) != names(after)
                || before.map(|value| &value["avatarSourceAliases"])
                    != after.map(|value| &value["avatarSourceAliases"]);
        }
        let dependency = if kind == "root"
            && before.map(|v| &v["status"]) != after.map(|v| &v["status"])
        {
            Some("SELECT DISTINCT product_id FROM entities WHERE kind='artifact' AND json_extract(body,'$.rootId')=?1")
        } else if kind == "account"
            && ["name", "label", "provider", "store"]
                .iter()
                .any(|field| before.map(|v| &v[*field]) != after.map(|v| &v[*field]))
        {
            Some("SELECT DISTINCT product_id FROM entities WHERE kind IN ('acquisition','entitlement','remote_file') AND json_extract(body,'$.accountId')=?1")
        } else {
            None
        };
        if let Some(sql) = dependency {
            let mut statement = conn.prepare(sql)?;
            for product in statement.query_map([id], |row| row.get::<_, String>(0))? {
                let product = product?;
                if !product.is_empty() {
                    affected.insert(product);
                }
            }
        }
        Ok(())
    }
    fn projects_product(kind: &str, before: Option<&Value>, after: Option<&Value>) -> bool {
        match kind {
            "product" | "overlay" | "acquisition" | "entitlement" | "remote_file" | "artifact" => {
                true
            }
            "job" => projection::job_changes_product(before, after),
            _ => false,
        }
    }
    fn write(
        tx: &Connection,
        projections: &mut projection::ProjectionChanges,
        kind: &str,
        id: &str,
        after: Option<Value>,
    ) -> Result<()> {
        let before = raw(tx, kind, id)?;
        if before == after {
            return Ok(());
        }
        if SYNC_KINDS.contains(&kind) {
            Self::enqueue_sync(tx, kind, id, before.as_ref(), after.as_ref())?;
        }
        let mut affected = BTreeSet::new();
        Self::collect_projection_dependencies(
            tx,
            kind,
            id,
            before.as_ref(),
            after.as_ref(),
            projections,
            &mut affected,
        )?;
        if kind == "product" || kind == "overlay" {
            affected.insert(id.to_owned());
        }
        for v in before.iter().chain(after.iter()) {
            if let Some(p) = v["productId"].as_str() {
                if Self::projects_product(kind, before.as_ref(), after.as_ref()) {
                    affected.insert(p.to_owned());
                }
            }
        }
        let local_files: BTreeSet<String> = if kind == "artifact" {
            before
                .iter()
                .chain(after.iter())
                .filter_map(|value| value["fileId"].as_str().map(str::to_owned))
                .collect()
        } else {
            BTreeSet::new()
        };
        let ai_changed = kind == "artifact"
            && ai_artifact_requires_projection(tx, before.as_ref(), after.as_ref())?;
        match after {
            Some(v) => set_raw(tx, kind, id, &v)?,
            None => {
                tx.execute(
                    "DELETE FROM entities WHERE kind=?1 AND id=?2",
                    params![kind, id],
                )?;
            }
        }
        for p in affected {
            if matches!(kind, "job" | "artifact" | "root") && !ai_changed {
                projection::refresh_local(tx, &p, &local_files, kind == "root")?;
            } else {
                projections.merge(projection::refresh(tx, &p)?);
            }
        }
        Ok(())
    }
    pub fn manifest(&self) -> Result<Value> {
        self.on_sync_worker(|catalogue| catalogue.manifest_on_worker())
    }
    fn manifest_on_worker(&self) -> Result<Value> {
        use sha2::{Digest, Sha256};
        let (mut manifest, mut doc) = {
            let (conn, cache) = self.lock_document()?;
            (portable::manifest(&conn)?, cache.as_ref().unwrap().clone())
        };
        let bytes = doc.save();
        let mut media: BTreeSet<_> = strings(&manifest["media"]).into_iter().collect();
        for conflict in sync::conflicts(&doc)? {
            if conflict["kind"] == "overlay" && conflict["field"] == "thumbnailPath" {
                for alternative in conflict["values"].as_array().into_iter().flatten() {
                    if let Some(path) = alternative["value"]["value"].as_str() {
                        portable::validate_relative(path)?;
                        media.insert(path.to_owned());
                    }
                }
            }
        }
        manifest["media"] = json!(media);
        manifest["syncSnapshot"] = json!({"version":1,"encoding":"hex","sha256":format!("{:x}",Sha256::digest(&bytes)),"data":hex::encode(bytes)});
        Ok(manifest)
    }
    pub fn restore_manifest(&self, manifest: Value) -> Result<()> {
        self.on_sync_worker(move |catalogue| portable::restore(catalogue, manifest, None, None))
    }
    /// Storage alone calls this after verifying the complete backup payload inventory.
    pub fn restore_verified_manifest(&self, manifest: Value, root_mappings: Value) -> Result<()> {
        self.on_sync_worker(move |catalogue| {
            portable::restore(catalogue, manifest, Some(root_mappings), None)
        })
    }
    /// Preserve only the caller's currently running local backup-restore job.
    pub(crate) fn restore_verified_manifest_for_job(
        &self,
        manifest: Value,
        root_mappings: Value,
        job_id: String,
    ) -> Result<()> {
        self.on_sync_worker(move |catalogue| {
            portable::restore(catalogue, manifest, Some(root_mappings), Some(&job_id))
        })
    }
    pub fn command(&self, action: &str, args: Value) -> Result<Value> {
        let mut result = match action {
            "spending_summary" => spending::summary(
                &*self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
                &args,
            ),
            "clear_activity" => self.mutate(|tx, projections| {
                let jobs = rows(tx, "job")?;
                let mut cleared = 0usize;
                let mut active = 0usize;
                for job in jobs {
                    // This job owns staged folders until deletion or rollback
                    // finishes; clearing history must not discard its recovery.
                    if job["type"] == "storage:delete_local_products" && job["state"] != "completed"
                    {
                        active += 1;
                        continue;
                    }
                    match text(&job, "state") {
                        "completed" | "failed" | "needsAction" | "cancelled" => {
                            Self::write(tx, projections, "job", text(&job, "id"), None)?;
                            cleared += 1;
                        }
                        "queued" | "running" => active += 1,
                        _ => {}
                    }
                }
                Ok(json!({"cleared":cleared,"remainingActive":active}))
            }),
            "account_counts" => query::account_counts(
                &*self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
            ),
            "state" => {
                let conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                Ok(
                    json!({"accounts":rows(&conn,"account")?,"roots":self.display_roots(&conn)?,"jobs":rows(&conn,"job")?,"preferences":raw(&conn,"preferences","app")?,"sourceCoverage":rows(&conn,"source_coverage")?,"conflicts":self.current_conflicts(&conn)?}),
                )
            }
            "bootstrap" => {
                let mut conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                let tx = conn.transaction()?;
                let products = if args["includeProducts"] == false {
                    vec![]
                } else {
                    projection::all(&tx)?
                        .into_iter()
                        .filter(|product| product["hidden"] != true)
                        .collect()
                };
                let dictionary = crate::taxonomy::dictionary();
                let bases = projection::bases(&tx)?;
                let owned = crate::taxonomy::owned_base_options(&bases);
                let avatar_options = avatars::options(&tx, &bases, &owned)?;
                let (total, counts) = query::counts(&tx)?;
                let result = json!({"version":1,"total":total,"counts":counts,"products":products,"accounts":rows(&tx,"account")?,"roots":self.display_roots(&tx)?,"jobs":rows(&tx,"job")?,"preferences":raw(&tx,"preferences","app")?,"collections":rows(&tx,"collection")?,"savedSearches":rows(&tx,"saved_search")?,"sourceCoverage":rows(&tx,"source_coverage")?,"conflicts":self.current_conflicts(&tx)?,"tagOptions":dictionary["tags"],"avatars":dictionary,"avatarOptions":avatar_options,"ownedBaseOptions":owned});
                tx.commit()?;
                Ok(result)
            }
            "product_details" => {
                let conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                let body: Option<String> = conn
                    .query_row(
                        "SELECT body FROM products WHERE id=?1",
                        [text(&args, "id")],
                        |row| row.get(0),
                    )
                    .optional()?;
                let mut product: Value =
                    serde_json::from_str(&body.context("catalogue.product_missing")?)?;
                acquisition::apply_scope(&mut product, args["accountIds"].as_array());
                Ok(product)
            }
            "collections" => {
                let conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                Ok(json!({"collections": rows(&conn, "collection")?}))
            }
            "query" => query::query(
                &*self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?,
                &args,
            ),
            "overlay" => self.edit_overlay(args),
            "undo" => self.undo(
                args["token"]
                    .as_str()
                    .or(args["undoToken"].as_str())
                    .unwrap_or(""),
            ),
            "collection" | "save_search" | "saved_search" => self.manage(action, args),
            "preferences" => self.preferences(args),
            "conflicts" => {
                let conn = self
                    .inner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
                Ok(json!(self.current_conflicts(&conn)?))
            }
            "resolve_conflict" => self.on_sync_worker(move |catalogue| {
                catalogue.mutate_sync(|tx, doc, projections| {
                    let heads = doc.get_heads();
                    sync::resolve(doc, &args)?;
                    Self::project_document(tx, doc, projections, &heads)?;
                    Ok(json!({"ok":true}))
                })
            }),
            "exchange" => self.exchange(Path::new(text(&args, "folder"))),
            "export_csv" => portable::csv(self, args),
            "manifest" => self.manifest(),
            "restore_manifest" => {
                self.restore_manifest(args.get("manifest").cloned().unwrap_or(args))?;
                Ok(json!({"ok":true}))
            }
            _ => bail!("catalogue.unknown_action"),
        }?;
        self.resolve_previews(&mut result)?;
        Ok(result)
    }
    fn edit_overlay(&self, args: Value) -> Result<Value> {
        let changes = if let Some(changes) = args["changes"].as_array() {
            changes.clone()
        } else {
            strings(&args["ids"]).into_iter().collect::<BTreeSet<_>>().into_iter().map(|id|json!({"id":id,"patch":args.get("patch").cloned().unwrap_or(json!({})),"clearFields":args["clearFields"]})).collect()
        };
        ensure!(!changes.is_empty(), "catalogue.empty_selection");
        let mut seen = BTreeSet::new();
        for change in &changes {
            ensure!(
                !text(change, "id").is_empty() && seen.insert(text(change, "id")),
                "catalogue.invalid_selection"
            );
            validate_patch(change.get("patch").unwrap_or(&json!({})))?;
            ensure!(
                strings(&change["clearFields"])
                    .iter()
                    .all(|f| EDITABLE.contains(&f.as_str())),
                "catalogue.invalid_overlay_field"
            );
        }
        self.mutate(|tx, projections| {
            let mut entries = vec![];
            let tag_resolver = if changes
                .iter()
                .any(|change| change["patch"].get("tags").is_some())
            {
                Some(avatars::tag_resolver(tx)?)
            } else {
                None
            };
            for change in changes {
                let id = text(&change, "id").to_owned();
                let patch = change.get("patch").cloned().unwrap_or(json!({}));
                let clear = strings(&change["clearFields"]);
                let source = raw(tx, "product", &id)?.context("catalogue.product_missing")?;
                if patch.get("hidden").is_some() || clear.iter().any(|field| field == "hidden") {
                    let provider = if text(&source, "provider").is_empty() {
                        text(&source, "store")
                    } else {
                        text(&source, "provider")
                    };
                    ensure!(
                        matches!(
                            normalized(provider).as_str(),
                            "booth" | "gumroad" | "jinxxy"
                        ),
                        "catalogue.hide_store_only"
                    );
                }
                let before = raw(tx, "overlay", &id)?.unwrap_or(json!({"id":id}));
                let mut after = before.clone();
                for field in &clear {
                    after.as_object_mut().unwrap().remove(field);
                }
                for (k, v) in patch.as_object().unwrap() {
                    after[k] = v.clone();
                }
                // Removing a suggested source tag records a durable rejection.
                if let Some(tags) = patch.get("tags") {
                    let source = raw(tx, "product", &id)?.unwrap();
                    let kept: BTreeSet<_> = strings(tags)
                        .iter()
                        .map(|s| projection::resolver_tag_key(tag_resolver.as_ref().unwrap(), s))
                        .collect();
                    let mut rejected = strings(&after["rejectedTags"]);
                    let mut offered = strings(source.get("sourceTags").unwrap_or(&source["tags"]));
                    offered.extend(
                        after["ai"]["analysis"]["tags"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|t| t["label"].as_str().map(str::to_owned)),
                    );
                    offered.extend(
                        after["aiTagOrganization"]["aliases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|a| a["to"].as_str().map(str::to_owned)),
                    );
                    for tag in offered {
                        if !kept.contains(&projection::resolver_tag_key(
                            tag_resolver.as_ref().unwrap(),
                            &tag,
                        )) && !rejected.contains(&tag)
                        {
                            rejected.push(tag);
                        }
                    }
                    // Explicit re-add accepts that spelling again.
                    rejected.retain(|t| {
                        !kept.contains(&projection::resolver_tag_key(
                            tag_resolver.as_ref().unwrap(),
                            t,
                        ))
                    });
                    after["rejectedTags"] = json!(rejected);
                }
                let fields: BTreeSet<String> = before
                    .as_object()
                    .unwrap()
                    .keys()
                    .chain(after.as_object().unwrap().keys())
                    .filter(|k| before.get(*k) != after.get(*k))
                    .cloned()
                    .collect();
                if !fields.is_empty() {
                    entries.push(json!({"id":id,"before":before,"after":after,"fields":fields}));
                    Self::write(tx, projections, "overlay", &id, Some(after))?;
                }
            }
            projections.prepared_resolver = tag_resolver;
            let token = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO undo(token,body) VALUES(?1,?2)",
                params![token, serde_json::to_string(&entries)?],
            )?;
            Ok(json!({"ok":true,"token":token,"undoToken":token,"changed":entries.len()}))
        })
    }
    fn undo(&self, token: &str) -> Result<Value> {
        self.mutate(|tx, projections| {
            let body: Option<String> = tx
                .query_row("SELECT body FROM undo WHERE token=?1", [token], |r| {
                    r.get(0)
                })
                .optional()?;
            let entries: Vec<Value> =
                serde_json::from_str(&body.context("catalogue.undo_missing")?)?;
            let mut conflicts = vec![];
            let mut changed = 0;
            for entry in entries {
                let id = text(&entry, "id");
                let current = raw(tx, "overlay", id)?.unwrap_or(json!({"id":id}));
                let fields = strings(&entry["fields"]);
                let conflict_keys = fields
                    .iter()
                    .map(|field| json!({"kind":"overlay","id":id,"field":field}))
                    .collect::<Vec<_>>();
                if self.current_conflicts(tx)?.iter().any(|conflict| {
                    conflict_keys.iter().any(|key| {
                        key["kind"] == conflict["kind"]
                            && key["id"] == conflict["id"]
                            && key["field"] == conflict["field"]
                    })
                }) || fields
                    .iter()
                    .any(|f| current.get(f) != entry["after"].get(f))
                {
                    conflicts.push(id.to_owned());
                    continue;
                }
                let mut restored = current;
                for field in fields {
                    match entry["before"].get(&field) {
                        Some(v) => {
                            restored[&field] = v.clone();
                        }
                        None => {
                            restored.as_object_mut().unwrap().remove(&field);
                        }
                    }
                }
                Self::write(tx, projections, "overlay", id, Some(restored))?;
                changed += 1;
            }
            tx.execute("DELETE FROM undo WHERE token=?1", [token])?;
            Ok(json!({"ok":conflicts.is_empty(),"changed":changed,"conflicts":conflicts}))
        })
    }
    fn manage(&self, action: &str, args: Value) -> Result<Value> {
        let kind = if action == "collection" {
            "collection"
        } else {
            "saved_search"
        };
        let op = args["operation"]
            .as_str()
            .or(args["op"].as_str())
            .unwrap_or("create");
        self.mutate_scoped(kind == "collection", |tx, projections| {
            if op == "add" || op == "add_members" {
                ensure!(kind == "collection", "catalogue.invalid_operation");
                let ids = strings(&args["ids"]);
                let mut collections = strings(&args["collectionIds"]);
                for name in strings(&args["names"]) {
                    let name = name.trim();
                    ensure!(!name.is_empty(), "catalogue.name_required");
                    let existing: Option<String> = tx.query_row(
                        "SELECT id FROM entities WHERE kind='collection' AND json_extract(body,'$.name')=?1 ORDER BY id LIMIT 1",
                        [name], |row| row.get(0),
                    ).optional()?;
                    let id = if let Some(id) = existing {
                        id
                    } else {
                        let id = Uuid::new_v4().to_string();
                        Self::write(
                            tx,
                            projections,
                            "collection",
                            &id,
                            Some(json!({"id":id,"name":name,"productIds":[]})),
                        )?;
                        id
                    };
                    collections.push(id);
                }
                ensure!(!collections.is_empty(), "catalogue.empty_selection");
                for id in &ids {
                    ensure!(
                        tx.query_row("SELECT EXISTS(SELECT 1 FROM entities WHERE kind='product' AND id=?1)", [id], |row| row.get::<_, bool>(0))?,
                        "catalogue.product_missing"
                    );
                }
                for id in collections.into_iter().collect::<BTreeSet<_>>() {
                    let mut value = raw(tx, kind, &id)?.context("catalogue.collection_missing")?;
                    let mut members: BTreeSet<_> =
                        strings(&value["productIds"]).into_iter().collect();
                    members.extend(ids.clone());
                    value["productIds"] = json!(members);
                    Self::write(tx, projections, kind, &id, Some(value))?;
                }
                return Ok(json!({"ok":true,"collections":rows(tx, "collection")?}));
            }
            let id = args["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            if op == "delete" {
                Self::write(tx, projections, kind, &id, None)?;
                return Ok(if kind == "collection" { json!({"ok":true,"collections":rows(tx, kind)?}) } else { json!({"ok":true}) });
            }
            ensure!(
                ["create", "update", "rename", "remove_members"].contains(&op),
                "catalogue.invalid_operation"
            );
            let before = raw(tx, kind, &id)?;
            if op != "create" {
                ensure!(before.is_some(), "catalogue.entity_missing");
            }
            if op == "create" {
                ensure!(before.is_none(), "catalogue.already_exists");
            }
            let mut value = before.unwrap_or(json!({"id":id,"productIds":[]}));
            let patch = args.get("value").or(args.get("patch")).unwrap_or(&args);
            for field in if kind == "collection" {
                vec!["name", "productIds"]
            } else {
                vec![
                    "name",
                    "query",
                    "category",
                    "status",
                    "sort",
                    "direction",
                    "accountIds",
                    "avatar",
                    "sensitiveMode",
                    "r18",
                    "quick",
                    "hiddenOnly",
                    "collection",
                    "filters",
                ]
            } {
                if let Some(v) = patch.get(field) {
                    value[field] = v.clone();
                }
            }
            if op == "remove_members" {
                let remove: BTreeSet<_> = strings(&args["ids"]).into_iter().collect();
                value["productIds"] = json!(strings(&value["productIds"])
                    .into_iter()
                    .filter(|i| !remove.contains(i))
                    .collect::<Vec<_>>());
            }
            ensure!(
                !text(&value, "name").trim().is_empty(),
                "catalogue.name_required"
            );
            validate(kind, &id, &value)?;
            Self::write(tx, projections, kind, &id, Some(value.clone()))?;
            // Read within this same transaction; the UI can commit membership
            // without an extra list request or an intervening writer's snapshot.
            if kind == "collection" {
                value["collections"] = json!(rows(tx, kind)?);
            }
            Ok(value)
        })
    }
    fn preferences(&self, args: Value) -> Result<Value> {
        self.mutate(|tx, projections| self.apply_preferences(tx, projections, &args))
    }
    fn apply_preferences(
        &self,
        tx: &Transaction<'_>,
        projections: &mut projection::ProjectionChanges,
        args: &Value,
    ) -> Result<Value> {
        let mut value = raw(tx, "preferences", "app")?.unwrap_or(json!({"id":"app"}));
        let was_complete = value["setupComplete"] == true;
        let patch = args.get("patch").unwrap_or(&args);
        ensure!(patch.is_object(), "catalogue.invalid_preferences");
        validate_ui_preferences(patch)?;
        for (k, v) in patch.as_object().unwrap() {
            if k != "id" {
                if k == "batchActions" {
                    let mut fields = value[k].as_object().cloned().unwrap_or_default();
                    fields.extend(v.as_object().unwrap().clone());
                    value[k] = Value::Object(fields);
                } else {
                    value[k] = v.clone();
                }
            }
        }
        if !was_complete && value["setupComplete"] == true {
            let root = raw(tx, "root", text(&value, "libraryRootId"))?;
            ensure!(
                root.as_ref().is_some_and(|root| root["status"] == "online"
                    && root["active"] == true
                    && !text(root, "path").is_empty()
                    && (Path::new(text(root, "path")).is_absolute()
                        || (root["libraryRelative"] == true
                            && self.library_root().is_some()
                            && (text(root, "path") == "."
                                || portable::validate_relative(text(root, "path")).is_ok())))),
                "library_not_configured"
            );
        }
        let n = value["downloadConcurrency"]
            .as_u64()
            .unwrap_or(3)
            .clamp(1, 6);
        value["downloadConcurrency"] = json!(n);
        value["extractConcurrency"] = json!(1);
        if value.get("aiEnabled").is_none() {
            value["aiEnabled"] = json!(false);
        }
        if value.get("aiScopes").is_none() {
            value["aiScopes"] = default_ai_scopes();
        }
        validate_ai_preferences(&value)?;
        value["uiScale"] = json!(value["uiScale"]
            .as_f64()
            .filter(|n| n.is_finite())
            .unwrap_or(100.0)
            .clamp(80.0, 300.0)
            .round() as u64);
        avatars::normalize_preferences(tx, &mut value)?;
        validate("preferences", "app", &value)?;
        Self::write(tx, projections, "preferences", "app", Some(value.clone()))?;
        Ok(value)
    }
    pub(crate) fn commit_setup(
        &self,
        patch: Value,
        accounts: Vec<Value>,
        select: impl FnOnce() -> Result<()>,
    ) -> Result<Value> {
        self.mutate(|tx, projections| {
            for account in accounts {
                let id = text(&account, "id");
                ensure!(
                    raw(tx, "account", id)?.is_none(),
                    "ACCOUNT_IDENTITY_MISMATCH"
                );
                validate("account", id, &account)?;
                Self::write(tx, projections, "account", id, Some(account.clone()))?;
            }
            let preferences = self.apply_preferences(tx, projections, &json!({"patch":patch}))?;
            // Locator failure rolls back the same SQL transaction, including
            // newly attached accounts. No initial sync starts before restart.
            select()?;
            Ok(preferences)
        })
    }
    fn project_document(
        tx: &Connection,
        doc: &mut automerge::AutoCommit,
        projections: &mut projection::ProjectionChanges,
        heads: &[automerge::ChangeHash],
    ) -> Result<()> {
        let fields = sync::changed_fields(doc, heads)?;
        Self::project_fields(tx, doc, projections, fields)
    }
    fn project_fields(
        tx: &Connection,
        doc: &automerge::AutoCommit,
        projections: &mut projection::ProjectionChanges,
        fields: Vec<(String, String, String)>,
    ) -> Result<()> {
        let mut changes = std::collections::BTreeMap::<(String, String), Vec<String>>::new();
        for (kind, id, field) in fields {
            changes.entry((kind, id)).or_default().push(field);
        }
        let mut affected = BTreeSet::new();
        for ((kind, id), fields) in changes {
            ensure!(
                SYNC_KINDS.contains(&kind.as_str()),
                "catalogue.invalid_sync_kind"
            );
            let before = raw(tx, &kind, &id)?;
            let after = sync::project_entity(doc, &kind, &id, before.clone(), &fields)?;
            if before == after {
                continue;
            }
            Self::collect_projection_dependencies(
                tx,
                &kind,
                &id,
                before.as_ref(),
                after.as_ref(),
                projections,
                &mut affected,
            )?;
            if kind == "product" || kind == "overlay" {
                affected.insert(id.clone());
            }
            for value in before.iter().chain(after.iter()) {
                if let Some(p) = value["productId"].as_str() {
                    affected.insert(p.to_owned());
                }
            }
            match after {
                Some(value) => {
                    validate(&kind, &id, &value)?;
                    set_raw(tx, &kind, &id, &value)?;
                }
                None => {
                    tx.execute(
                        "DELETE FROM entities WHERE kind=?1 AND id=?2",
                        params![kind, id],
                    )?;
                }
            }
        }
        for id in affected {
            projections.merge(projection::refresh(tx, &id)?);
        }
        Ok(())
    }
    fn exchange(&self, folder: &Path) -> Result<Value> {
        let folder = folder.to_path_buf();
        self.on_sync_worker(move |catalogue| catalogue.exchange_on_worker(&folder))
    }
    fn exchange_on_worker(&self, folder: &Path) -> Result<Value> {
        self.exchange_with_hook(folder, |_| Ok(()))
    }
    fn exchange_with_hook(
        &self,
        folder: &Path,
        after_transfer: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<Value> {
        self.flush_sync_outbox()?;
        ensure!(folder.is_absolute(), "catalogue.absolute_folder_required");
        let fingerprint = Self::folder_fingerprint(folder)?;
        let generation = {
            let conn = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            conn.query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key='sync_revision'",
                [],
                |r| r.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0)
        };
        {
            let cache = self
                .exchange_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
            if cache.as_ref().is_some_and(|(path, rev, files)| {
                path == folder && *rev == generation && *files == fingerprint
            }) {
                return Ok(json!({"ok":true,"unchanged":true}));
            }
        }
        // Snapshot the already validated history. Folder IO, incoming operation
        // validation, batch application and conflict discovery happen unlocked.
        let (mut outgoing, snapshot_generation, previous_conflicts) = {
            let (conn, cache) = self.lock_document()?;
            let previous = conn
                .query_row(
                    "SELECT CAST(value AS INTEGER) FROM meta WHERE key='sync_revision'",
                    [],
                    |r| r.get::<_, u64>(0),
                )
                .optional()?
                .unwrap_or(0);
            let conflicts: Option<String> = conn
                .query_row(
                    "SELECT CAST(value AS TEXT) FROM meta WHERE key='conflicts'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            let conflicts: Vec<Value> = conflicts
                .map(|v| serde_json::from_str(&v))
                .transpose()?
                .unwrap_or_default();
            (cache.as_ref().unwrap().clone(), previous, conflicts)
        };
        let snapshot_heads = outgoing.get_heads();
        let transfer = sync::exchange_validated(&mut outgoing, folder)?;
        let fields = sync::changed_fields(&mut outgoing, &snapshot_heads)?;
        let conflicts = sync::changed_conflicts(&outgoing, &previous_conflicts, &fields)?;
        let remote_changed = snapshot_heads != outgoing.get_heads();
        after_transfer(self)?;
        self.mutate_sync(|tx, doc, projections| {
            let heads = doc.get_heads();
            if heads == snapshot_heads {
                if remote_changed {
                    *doc = outgoing;
                    Self::project_fields(tx, doc, projections, fields)?;
                    Self::save_conflicts(tx, &conflicts)?;
                }
            } else {
                // Concurrent local writes remain authoritative inputs to merge.
                doc.merge(&mut outgoing)?;
                if heads != doc.get_heads() {
                    let fields = sync::changed_fields(doc, &heads)?;
                    let previous: Option<String> = tx
                        .query_row(
                            "SELECT CAST(value AS TEXT) FROM meta WHERE key='conflicts'",
                            [],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let previous: Vec<Value> = previous
                        .map(|v| serde_json::from_str(&v))
                        .transpose()?
                        .unwrap_or_default();
                    let conflicts = sync::changed_conflicts(doc, &previous, &fields)?;
                    Self::project_fields(tx, doc, projections, fields)?;
                    Self::save_conflicts(tx, &conflicts)?;
                }
            }
            Ok(())
        })?;
        // A concurrent local edit was not exported; retaining the captured
        // generation forces the next poll to publish it rather than skip it.
        let exported_generation = snapshot_generation + u64::from(remote_changed);
        let after = Self::folder_fingerprint(folder)?;
        if fingerprint == after {
            *self
                .exchange_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("catalogue.lock"))? =
                Some((folder.to_path_buf(), exported_generation, after));
        }
        Ok(
            json!({"ok":true,"read":{"imported":transfer["imported"],"pendingDependencies":transfer["pendingDependencies"],"ignoredFiles":transfer["ignoredFiles"]},"write":{"exported":transfer["exported"],"pendingDependencies":transfer["pendingDependencies"]}}),
        )
    }
    fn folder_fingerprint(folder: &Path) -> Result<Vec<String>> {
        if !folder.exists() {
            return Ok(vec![]);
        }
        let mut result = vec![];
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("amchange") {
                continue;
            }
            let meta = entry.metadata()?;
            result.push(format!(
                "{}:{}:{:?}",
                entry.file_name().to_string_lossy(),
                meta.len(),
                meta.modified()?
            ));
        }
        result.sort();
        Ok(result)
    }
}
