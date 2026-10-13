//! Opt-in page bound for authorized live checks in an isolated debug profile.
pub(crate) fn source_page_limit() -> Option<usize> {
    #[cfg(debug_assertions)]
    if std::env::var_os("NNA_DATA_DIR").is_some()
        && std::env::var("NNA_VERIFY_SOURCE_PAGES").as_deref() == Ok("5")
    {
        return Some(5);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, OpenFlags};
    use serde_json::{json, Value};
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };

    fn compare(actual: &Value, expected: &Value, failures: &mut BTreeMap<&'static str, usize>) {
        for (field, code) in [
            ("category", "CATEGORY_PROJECTION_LOST"),
            ("categoryInference", "CATEGORY_INFERENCE_LOST"),
            ("listingAvatars", "PUBLIC_SUPPORT_SCOPE_LOST"),
            ("ownedAvatars", "OWNED_COMPATIBILITY_LOST"),
            ("ownedAvatarEvidence", "OWNED_COMPATIBILITY_EVIDENCE_LOST"),
            ("tags", "EFFECTIVE_TAGS_LOST"),
        ] {
            let same = if matches!(field, "category" | "categoryInference") {
                actual[field] == expected[field]
            } else {
                let set = |value: &Value| {
                    value
                        .as_array()
                        .map(|rows| rows.iter().map(Value::to_string).collect::<BTreeSet<_>>())
                };
                set(&actual[field]) == set(&expected[field]) && actual[field].is_array()
            };
            if !same {
                *failures.entry(code).or_default() += 1;
            }
        }
    }

    fn audit(conn: &Connection) -> anyhow::Result<Value> {
        let mut failures = BTreeMap::new();
        let mut counts = BTreeMap::new();
        let mut query = conn.prepare("SELECT p.id,p.body FROM products p JOIN entities e ON e.kind='product' AND e.id=p.id WHERE lower(coalesce(json_extract(e.body,'$.provider'),json_extract(e.body,'$.store'))) IN ('booth','gumroad','jinxxy')")?;
        for row in query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, body) = row?;
            let actual: Value = serde_json::from_str(&body)?;
            let expected = crate::catalogue::verification_expected(conn, &id)?
                .ok_or_else(|| anyhow::anyhow!("SOURCE_PRODUCT_MISSING"))?;
            compare(&actual, &expected, &mut failures);
            for (key, field) in [
                ("PUBLIC_AVATARS", "listingAvatars"),
                ("OWNED_AVATARS", "ownedAvatars"),
                ("EFFECTIVE_TAGS", "tags"),
            ] {
                *counts.entry(key).or_insert(0usize) +=
                    expected[field].as_array().map_or(0, Vec::len);
            }
            *counts.entry("PRODUCTS").or_insert(0usize) += 1;
        }
        if counts.get("PRODUCTS").copied().unwrap_or(0) == 0 {
            failures.insert("NO_STORE_PRODUCTS_OBSERVED", 1);
        }
        Ok(
            json!({"result":if failures.is_empty(){"PASS"}else{"FAIL"},"failures":failures,"counts":counts}),
        )
    }

    #[test]
    #[ignore = "Lead-only authorized isolated real-session projection audit"]
    fn real_store_projection_audit() {
        let checked = || -> anyhow::Result<Value> {
            anyhow::ensure!(
                source_page_limit() == Some(5),
                "VERIFICATION_AUTHORIZATION_REQUIRED"
            );
            let profile = PathBuf::from(
                std::env::var_os("NNA_DATA_DIR")
                    .ok_or_else(|| anyhow::anyhow!("VERIFICATION_PROFILE_REQUIRED"))?,
            )
            .canonicalize()?;
            let database = PathBuf::from(
                std::env::var_os("NNA_VERIFY_DATABASE")
                    .ok_or_else(|| anyhow::anyhow!("VERIFICATION_DATABASE_REQUIRED"))?,
            )
            .canonicalize()?;
            let expected_database =
                if let Some(library) = crate::profile::selected_library(&profile)? {
                    library
                        .path
                        .join(".nna")
                        .join("data")
                        .join("catalogue.sqlite3")
                } else {
                    profile.join("catalogue.sqlite3")
                };
            anyhow::ensure!(
                database == expected_database.canonicalize()?,
                "VERIFICATION_DATABASE_SCOPE_INVALID"
            );
            let conn = Connection::open_with_flags(
                database,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch("PRAGMA query_only=ON; BEGIN")?;
            audit(&conn)
        };
        // Error details can contain private values or paths: emit fixed codes only.
        let report = checked().unwrap_or_else(|_| json!({"result":"FAIL","failures":{"VERIFICATION_AUDIT_UNAVAILABLE":1},"counts":{}}));
        println!("{}", report);
        assert!(report["result"] == "PASS", "VERIFICATION_PROJECTION_FAILED");
    }

    #[test]
    fn source_projection_audit_detects_lost_authored_components() {
        let directory =
            std::env::temp_dir().join(format!("nna-authored-projection-{}", uuid::Uuid::new_v4()));
        let catalogue = crate::catalogue::Catalogue::open(&directory).unwrap();
        catalogue
            .put(
                "account",
                "authored-account",
                json!({"id":"authored-account","provider":"booth"}),
            )
            .unwrap();
        for index in 0..12 {
            let id = format!("booth:{}", 900001 + index);
            let file = format!("authored-file-{index}");
            let grant = format!("authored-grant-{index}");
            catalogue.put_many(vec![
                ("product".into(), id.clone(), json!({"id":id,"provider":"booth","title":"Authored clothing","sourceTags":["VRChat","Authored","マヌカ"],"sourceCategory":{"name":"Clothes"},"publicVariants":[{"id":"authored-variant","type":"digital","name":"マヌカ専用"}]})),
                ("remote_file".into(), file.clone(), json!({"id":file,"productId":id,"accountId":"authored-account","sourceFileId":"authored","revision":"authored","name":"Manuka.unitypackage","variantId":"authored-variant"})),
                ("entitlement".into(), grant.clone(), json!({"id":grant,"productId":id,"accountId":"authored-account","fileIds":[file]})),
            ]).unwrap();
        }
        let conn = Connection::open_with_flags(
            directory.join("catalogue.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let report = audit(&conn).unwrap();
        assert!(report["result"] == "PASS", "AUTHORED_PROJECTION_MISMATCH");
        assert!(
            report["counts"]["PRODUCTS"] == 12,
            "AUTHORED_PRODUCT_COUNT_INVALID"
        );
        let expected = crate::catalogue::verification_expected(&conn, "booth:900001")
            .unwrap()
            .unwrap();
        for field in [
            "tags",
            "category",
            "listingAvatars",
            "ownedAvatars",
            "ownedAvatarEvidence",
        ] {
            let mut actual = expected.clone();
            actual[field] = Value::Null;
            let mut failures = BTreeMap::new();
            compare(&actual, &expected, &mut failures);
            assert!(failures.len() == 1, "AUTHORED_LOSS_NOT_DETECTED");
        }
        drop(conn);
        drop(catalogue);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
