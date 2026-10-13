//! Versioned metadata portability. Filesystem payload copying belongs to StorageService.
use super::*;
use std::{collections::BTreeSet, io::Write};
const PORTABLE: &[&str] = &[
    "product",
    "overlay",
    "acquisition",
    "entitlement",
    "remote_file",
    "artifact",
    "account",
    "root",
    "collection",
    "saved_search",
    "preferences",
    "source_coverage",
];

pub(super) fn validate_relative(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.starts_with('/')
            && !value.starts_with('\\')
            && !value.contains(':')
            && !value.contains('\0'),
        "catalogue.invalid_relative_path"
    );
    ensure!(
        value
            .split(['/', '\\'])
            .all(|s| !s.is_empty() && s != "." && s != ".."),
        "catalogue.invalid_relative_path"
    );
    Ok(())
}
pub(super) fn validate_no_secrets(value: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = k
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                ensure!(
                    ![
                        "password",
                        "cookie",
                        "cookies",
                        "authorization",
                        "accesstoken",
                        "refreshtoken",
                        "apikey",
                        "secret",
                        "signedurl",
                        "downloadurl",
                        "rawhtml",
                        "rawresponse",
                        "httpbody",
                        "sessiontoken",
                        "headers"
                    ]
                    .contains(&key.as_str()),
                    "catalogue.secret_field_forbidden"
                );
                validate_no_secrets(v)?;
            }
        }
        Value::Array(a) => {
            for v in a {
                validate_no_secrets(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn portable_record(kind: &str, value: &Value) -> Value {
    if kind == "root" {
        return json!({"id":value["id"],"label":value["label"],"status":"needsMapping"});
    }
    if kind == "account" {
        let mut account = json!({"id":value["id"],"name":value["name"],"store":value["store"],"status":"disconnected"});
        if let Some(provider) = value.get("provider") {
            account["provider"] = provider.clone();
        }
        // Stable buyer binding is metadata; session credentials remain device-local.
        if let Some(identity) = value["identityHash"].as_str() {
            account["identityHash"] = json!(identity);
        }
        return account;
    }
    if kind == "preferences" {
        // Only established user settings are portable; arbitrary per-device paths
        // and future provider settings cannot accidentally enter a metadata backup.
        let mut out = Map::new();
        for field in [
            "id",
            "locale",
            "language",
            "theme",
            "presets",
            "themeTokens",
            "themeId",
            "autoDownloadAll",
            "downloadConcurrency",
            "extractConcurrency",
            "downloadRateBytesPerSecond",
            "downloadRateLimit",
            "rateLimit",
            "setupComplete",
            "libraryRootId",
            "thumb",
            "density",
            "cardColumns",
            "listColumns",
            "navWidth",
            "inspectorWidth",
            "uiScale",
            "avatarSettings",
            "avatarAutomaticEnabled",
            "avatarAliases",
            "avatarSourceAliases",
            "aiEnabled",
            "aiFeaturesEnabled",
            "aiScopes",
            "sensitiveMode",
            "batchActions",
        ] {
            if let Some(v) = value.get(field) {
                out.insert(field.to_owned(), v.clone());
            }
        }
        if let Some(scale) = out.get("uiScale") {
            let scale = scale.as_f64().unwrap_or(100.0).clamp(80.0, 300.0).round() as u64;
            out.insert("uiScale".to_owned(), json!(scale));
        }
        return Value::Object(out);
    }
    value.clone()
}
pub(super) fn manifest(conn: &Connection) -> Result<Value> {
    let mut records = Map::new();
    for kind in PORTABLE {
        let values = rows(conn, kind)?
            .iter()
            .map(|v| portable_record(kind, v))
            .collect::<Vec<_>>();
        for value in &values {
            validate_no_secrets(value)?;
        }
        records.insert((*kind).to_owned(), json!(values));
    }
    let images = rows(conn, "overlay")?
        .iter()
        .filter_map(|v| v["thumbnailPath"].as_str())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    Ok(
        json!({"format":"NekoNekoAsset","version":1,"complete":true,"records":records,"media":images,"devicePathsIncluded":false,"credentialsIncluded":false}),
    )
}
fn validate_manifest(manifest: &Value) -> Result<Vec<(String, String, Value)>> {
    ensure!(
        manifest["format"] == "NekoNekoAsset" && manifest["version"] == 1,
        "catalogue.unsupported_manifest"
    );
    ensure!(
        manifest["complete"] == true && manifest["records"].is_object(),
        "catalogue.incomplete_manifest"
    );
    validate_no_secrets(manifest)?;
    let mut seen = BTreeSet::new();
    let mut records = vec![];
    for kind in PORTABLE {
        let values = manifest["records"][*kind]
            .as_array()
            .context("catalogue.incomplete_manifest")?;
        for value in values {
            let portable_preferences;
            let value = if *kind == "preferences" {
                portable_preferences = portable_record(kind, value);
                &portable_preferences
            } else {
                value
            };
            let id = value["id"].as_str().context("catalogue.invalid_record")?;
            validate(kind, id, value)?;
            ensure!(
                seen.insert(((*kind).to_owned(), id.to_owned())),
                "catalogue.duplicate_record"
            );
            if *kind == "root" {
                ensure!(
                    value.get("path").is_none(),
                    "catalogue.device_path_in_manifest"
                );
            }
            if *kind == "account" {
                ensure!(
                    value.as_object().unwrap().keys().all(|k| [
                        "id",
                        "name",
                        "store",
                        "provider",
                        "status",
                        "identityHash"
                    ]
                    .contains(&k.as_str())),
                    "catalogue.account_private_field"
                );
            }
            records.push(((*kind).to_owned(), id.to_owned(), value.clone()));
        }
    }
    for (kind, _, v) in &records {
        if kind == "overlay" {
            ensure!(
                seen.contains(&("product".into(), text(v, "id").into())),
                "catalogue.orphan_overlay"
            );
        }
        if let Some(id) = v["productId"].as_str().filter(|s| !s.is_empty()) {
            ensure!(
                seen.contains(&("product".into(), id.into())),
                "catalogue.orphan_product_reference"
            );
        }
        if kind == "artifact" {
            for (field, target) in [("fileId", "remote_file"), ("rootId", "root")] {
                ensure!(
                    seen.contains(&(target.into(), text(v, field).into())),
                    "catalogue.orphan_artifact"
                );
            }
        }
        if kind == "entitlement" {
            for fid in strings(&v["fileIds"]) {
                ensure!(
                    seen.contains(&("remote_file".into(), fid)),
                    "catalogue.orphan_entitlement_file"
                );
            }
        }
        if kind == "collection" {
            for id in strings(&v["productIds"]) {
                ensure!(
                    seen.contains(&("product".into(), id)),
                    "catalogue.orphan_collection_member"
                );
            }
        }
    }
    let media = manifest["media"]
        .as_array()
        .context("catalogue.incomplete_manifest")?;
    for item in media {
        validate_relative(item.as_str().context("catalogue.invalid_media")?)?;
    }
    for (kind, _, v) in &records {
        if kind == "overlay" {
            if let Some(path) = v["thumbnailPath"].as_str() {
                ensure!(
                    media.iter().any(|m| m.as_str() == Some(path)),
                    "catalogue.incomplete_media_inventory"
                );
            }
        }
    }
    Ok(records)
}
pub(super) fn restore(
    catalogue: &Catalogue,
    manifest: Value,
    mappings: Option<Value>,
    active_restore_job_id: Option<&str>,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    // Validate everything before changing existing state or allocating a recovery copy.
    let records = validate_manifest(&manifest)?;
    let snapshot = &manifest["syncSnapshot"];
    ensure!(
        snapshot["version"] == 1 && snapshot["encoding"] == "hex",
        "catalogue.incomplete_sync_snapshot"
    );
    let encoded = snapshot["data"]
        .as_str()
        .context("catalogue.incomplete_sync_snapshot")?;
    ensure!(
        encoded.len() <= 512 * 1024 * 1024,
        "catalogue.sync_snapshot_too_large"
    );
    let snapshot_bytes =
        hex::decode(encoded).map_err(|_| anyhow::anyhow!("catalogue.invalid_sync_snapshot"))?;
    ensure!(
        snapshot["sha256"].as_str()
            == Some(format!("{:x}", Sha256::digest(&snapshot_bytes)).as_str()),
        "catalogue.sync_snapshot_checksum"
    );
    sync::validate_snapshot(&snapshot_bytes)?;
    if let Some(mappings) = &mappings {
        let map = mappings
            .as_object()
            .context("catalogue.invalid_root_mapping")?;
        for (kind, id, _) in &records {
            if kind == "root" {
                let path = map
                    .get(id)
                    .and_then(Value::as_str)
                    .context("catalogue.root_mapping_required")?;
                ensure!(
                    Path::new(path).is_absolute(),
                    "catalogue.absolute_path_required"
                );
            }
        }
        for id in map.keys() {
            ensure!(
                records.iter().any(|(k, i, _)| k == "root" && i == id),
                "catalogue.unknown_root_mapping"
            );
        }
    }
    let (mut conn, mut cache) = catalogue.lock_document()?;
    if let Some(id) = active_restore_job_id {
        let job = raw(&conn, "job", id)?.context("catalogue.invalid_restore_job")?;
        ensure!(
            !id.is_empty()
                && job["id"] == id
                && job["type"] == "storage:restore_backup"
                && job["state"] == "running",
            "catalogue.invalid_restore_job"
        );
    }
    // VACUUM INTO is SQLite's consistent snapshot; WAL bytes are included without copying a live DB.
    let backup_dir = catalogue.dir.join("recovery");
    std::fs::create_dir_all(&backup_dir)?;
    let backup = backup_dir.join(format!("before-restore-{}.sqlite3", Uuid::new_v4()));
    conn.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])?;
    let roots = rows(&conn, "root")?;
    // Keep causal history and tombstone replaced entities; a later folder exchange
    // must not resurrect the pre-restore catalogue or reuse actor sequence numbers.
    let mut doc = cache.take().unwrap();
    // Invalidate cached history before replacement; failed transactions reload the old durable doc.
    let tx = conn.transaction()?;
    let desired = records
        .iter()
        .filter(|(kind, _, _)| SYNC_KINDS.contains(&kind.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    sync::restore_snapshot(&mut doc, &snapshot_bytes, &desired)?;
    for kind in PORTABLE {
        tx.execute("DELETE FROM entities WHERE kind=?1", [kind])?;
    }
    tx.execute_batch("DELETE FROM undo; DELETE FROM am_changes;")?;
    // Local job continuity is explicit; jobs from the backup are never imported.
    tx.execute(
        "DELETE FROM entities WHERE kind IN ('job','checkpoint','download_intent') AND (?1 IS NULL OR kind<>'job' OR id<>?1)",
        [active_restore_job_id],
    )?;
    for (kind, id, mut value) in records {
        if kind == "root" {
            if let Some(mappings) = &mappings {
                if let Some(library) = catalogue.library_root() {
                    let mapped = Path::new(
                        mappings[&id]
                            .as_str()
                            .context("catalogue.invalid_root_mapping")?,
                    );
                    let relative = mapped
                        .strip_prefix(library)
                        .map_err(|_| anyhow::anyhow!("catalogue.invalid_root_mapping"))?;
                    value["path"] = json!(if relative.as_os_str().is_empty() {
                        ".".to_owned()
                    } else {
                        relative.to_string_lossy().replace('\\', "/")
                    });
                    value["libraryRelative"] = json!(true);
                } else {
                    value["path"] = mappings[&id].clone();
                }
                value["status"] = json!("online");
            } else if let Some(existing) = roots.iter().find(|r| text(r, "id") == id) {
                value = existing.clone();
            }
        }
        if kind == "artifact" && mappings.is_none() {
            // Restore inventory preserves hashes and prior result, but fresh local availability needs validation.
            value["restoreStatus"] = value["status"].clone();
            value["status"] = json!("unknown");
            value["originalAvailable"] = json!(false);
        }
        set_raw(&tx, &kind, &id, &value)?;
    }
    projection::rebuild(&tx)?;
    Catalogue::reconcile_projection(
        &tx,
        &projection::ProjectionChanges {
            identities_changed: true,
            ..Default::default()
        },
    )?;
    let bytes = doc.save();
    tx.execute("INSERT INTO meta(key,value) VALUES('automerge',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[&bytes])?;
    tx.execute_batch("DELETE FROM sync_trust; DELETE FROM sync_outbox;")?;
    tx.execute(
        "INSERT INTO sync_trust(key,digest) VALUES('base',?1)",
        [Catalogue::digest(&bytes)],
    )?;
    Catalogue::save_conflicts(&tx, &sync::conflicts(&doc)?)?;
    tx.execute("INSERT INTO meta(key,value) VALUES('sync_revision',1) ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",[])?;
    tx.execute("INSERT INTO meta(key,value) VALUES('last_restore_backup',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[backup.to_string_lossy().as_ref()])?;
    Catalogue::bump_revision(&tx)?;
    tx.commit()?;
    *cache = Some(doc);
    Ok(())
}
fn csv_cell(value: &str) -> String {
    // Spreadsheet formula execution is not metadata. Escape first-cell formula prefixes.
    let safe = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}
pub(super) fn csv(catalogue: &Catalogue, args: Value) -> Result<Value> {
    let conn = catalogue
        .inner
        .lock()
        .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
    let selected: Option<BTreeSet<_>> = args.get("ids").map(|v| strings(v).into_iter().collect());
    let mut output =
        String::from("id,title,sourceTitle,shop,category,status,price,acquired,note,tags,priceCurrency,paidPrice,paidCurrency\r\n");
    let mut count = 0;
    for p in projection::all(&conn)? {
        if selected
            .as_ref()
            .is_some_and(|ids| !ids.contains(text(&p, "id")))
        {
            continue;
        }
        let mut fields = vec![];
        for key in ["id", "title", "sourceTitle", "shop", "category", "status"] {
            fields.push(text(&p, key).to_owned());
        }
        fields.push(
            p["price"]
                .as_f64()
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
        fields.push(text(&p, "acquired").into());
        fields.push(text(&p, "note").into());
        fields.push(strings(&p["tags"]).join("; "));
        fields.push(text(&p, "priceCurrency").into());
        fields.push(
            p["paidPrice"]
                .as_f64()
                .map(|amount| amount.to_string())
                .unwrap_or_default(),
        );
        fields.push(text(&p, "paidCurrency").into());
        output.push_str(
            &fields
                .iter()
                .map(|s| csv_cell(s))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push_str("\r\n");
        count += 1;
    }
    drop(conn);
    if let Some(path) = args["path"].as_str() {
        let path = Path::new(path);
        ensure!(path.is_absolute(), "catalogue.absolute_path_required");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(output.as_bytes())?;
        file.sync_all()?;
        return Ok(json!({"ok":true,"count":count}));
    }
    Ok(json!({"text":output,"count":count}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_avatar_preferences_and_scale_survive_without_device_settings() -> Result<()> {
        let preferences = json!({
            "id":"app", "uiScale":125, "avatarSettings":{"hidden":["synthetic-base"]}, "avatarAutomaticEnabled":{"synthetic-base":true},
            "avatarAliases":{"synthetic-base":["Synthetic alias"]},
            "libraryPath":"X:/synthetic-device-only", "locale":"ja",
            "downloadRateBytesPerSecond":23 * 1024 * 1024,
            "cardColumns":["price"], "listColumns":["shop","files"],
            "aiFeaturesEnabled":true, "sensitiveMode":"only",
            "batchActions":{"spending":false,"download":true}
        });
        let exported = portable_record("preferences", &preferences);
        assert_eq!(exported["uiScale"], 125);
        assert_eq!(exported["avatarSettings"], preferences["avatarSettings"]);
        assert_eq!(
            exported["avatarAutomaticEnabled"],
            preferences["avatarAutomaticEnabled"]
        );
        assert_eq!(exported["avatarAliases"], preferences["avatarAliases"]);
        assert_eq!(exported["downloadRateBytesPerSecond"], 23 * 1024 * 1024);
        assert_eq!(exported["cardColumns"], json!(["price"]));
        assert_eq!(exported["listColumns"], json!(["shop", "files"]));
        for key in ["aiFeaturesEnabled", "sensitiveMode", "batchActions"] {
            assert_eq!(exported[key], preferences[key]);
        }
        assert!(exported.get("libraryPath").is_none());
        for (input, expected) in [
            (json!(20), 80),
            (json!(500), 300),
            (json!(125.6), 126),
            (json!("invalid"), 100),
        ] {
            let mut value = preferences.clone();
            value["uiScale"] = input;
            assert_eq!(portable_record("preferences", &value)["uiScale"], expected);
        }
        assert!(portable_record("preferences", &json!({"id":"app"}))
            .get("uiScale")
            .is_none());
        let mut records = Map::new();
        for kind in PORTABLE {
            records.insert((*kind).into(), json!([]));
        }
        let mut imported = preferences.clone();
        imported["uiScale"] = json!(450);
        records.insert("preferences".into(), json!([imported]));
        let manifest = json!({"format":"NekoNekoAsset","version":1,"complete":true,"records":records,"media":[]});
        let restored = validate_manifest(&manifest)?;
        let prefs = &restored
            .iter()
            .find(|(kind, _, _)| kind == "preferences")
            .unwrap()
            .2;
        assert_eq!(prefs["uiScale"], 300);
        assert_eq!(prefs["avatarSettings"], preferences["avatarSettings"]);
        assert_eq!(
            prefs["avatarAutomaticEnabled"],
            preferences["avatarAutomaticEnabled"]
        );
        assert_eq!(prefs["avatarAliases"], preferences["avatarAliases"]);
        assert_eq!(prefs["downloadRateBytesPerSecond"], 23 * 1024 * 1024);
        assert_eq!(prefs["cardColumns"], json!(["price"]));
        assert_eq!(prefs["listColumns"], json!(["shop", "files"]));
        for key in ["aiFeaturesEnabled", "sensitiveMode", "batchActions"] {
            assert_eq!(prefs[key], preferences[key]);
        }
        assert!(prefs.get("libraryPath").is_none());
        Ok(())
    }
}
