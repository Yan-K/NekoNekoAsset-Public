//! Portable metadata CRDT. SQLite owns the durable document and projection transaction.
//! The shared folder contains immutable Automerge changes, never a shared database.
use anyhow::{bail, ensure, Context, Result};
use automerge::{transaction::Transactable, ActorId, AutoCommit, Change, ReadDoc, ROOT};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

const DELETED: &str = "@deleted";
const MEMBER: &str = "@member:";
const RECOVER: &str = "@recover"; // Projection hint only; never stored in Automerge.
const MAX_CHANGE: u64 = 32 * 1024 * 1024;
const KINDS: &[&str] = &[
    "product",
    "overlay",
    "acquisition",
    "entitlement",
    "remote_file",
    "collection",
    "saved_search",
    "custom_category",
    "source_coverage",
];

pub fn new_doc(actor: &str) -> Result<AutoCommit> {
    ensure!(!actor.is_empty(), "sync_actor_required");
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(Sha256::digest(actor.as_bytes()).to_vec()));
    Ok(doc)
}

pub fn load_doc(bytes: &[u8], actor: &str) -> Result<AutoCommit> {
    let mut doc = load_trusted_doc(bytes, actor)?;
    validate_doc(&mut doc)?;
    Ok(doc)
}

// Only checksummed SQLite checkpoints/deltas previously validated by this version.
pub(super) fn load_trusted_doc(bytes: &[u8], actor: &str) -> Result<AutoCommit> {
    let mut doc = if bytes.is_empty() {
        new_doc(actor)?
    } else {
        AutoCommit::load(bytes).map_err(|_| anyhow::anyhow!("sync_document_invalid"))?
    };
    ensure!(!actor.is_empty(), "sync_actor_required");
    doc.set_actor(ActorId::from(Sha256::digest(actor.as_bytes()).to_vec()));
    ensure!(
        doc.get_missing_deps(&[]).is_empty(),
        "sync_document_dependencies_missing"
    );
    Ok(doc)
}

fn check_identity(kind: &str, id: &str) -> Result<()> {
    ensure!(KINDS.contains(&kind), "sync_kind_not_portable");
    ensure!(!id.is_empty() && id.len() <= 4096, "sync_identity_invalid");
    Ok(())
}

fn key(kind: &str, id: &str, field: &str) -> String {
    json!([kind, id, field]).to_string()
}

fn parse_key(key: &str) -> Result<(String, String, String)> {
    let parts: Vec<String> = serde_json::from_str(key).context("sync_key_invalid")?;
    ensure!(parts.len() == 3, "sync_key_invalid");
    check_identity(&parts[0], &parts[1])?;
    Ok((parts[0].clone(), parts[1].clone(), parts[2].clone()))
}

fn check_value(value: &Value, depth: usize) -> Result<()> {
    ensure!(depth <= 32, "sync_value_too_deep");
    match value {
        Value::Object(map) => {
            for (field, value) in map {
                let normalized: String = field
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                ensure!(
                    ![
                        "cookie",
                        "cookies",
                        "token",
                        "accesstoken",
                        "refreshtoken",
                        "csrftoken",
                        "apikey",
                        "password",
                        "credentials",
                        "secret",
                        "secrets",
                        "session",
                        "sessionid",
                        "authorization",
                        "headers",
                        "rawbody",
                        "httpbody",
                        "rawresponse",
                        "rawhtml",
                        "privatebody",
                        "downloadurl",
                        "signeddownloadurl",
                        "signedurl",
                        "localpath",
                        "absolutepath",
                        "rootpath",
                        "profilepath",
                        "path",
                        "extractedpath",
                        "binary",
                        "imagedata",
                        "thumbnaildata"
                    ]
                    .contains(&normalized.as_str()),
                    "sync_private_field"
                );
                if normalized.ends_with("path") {
                    if let Some(path) = value.as_str() {
                        ensure!(
                            !path.starts_with('/')
                                && !path.starts_with('\\')
                                && !path.contains(':')
                                && !path.split(['/', '\\']).any(|p| p == ".."),
                            "sync_device_path"
                        );
                    }
                }
                check_value(value, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                check_value(value, depth + 1)?;
            }
        }
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            ensure!(
                !lower.starts_with("data:")
                    && !lower.starts_with("blob:")
                    && !lower.starts_with("file:"),
                "sync_binary_or_local_url"
            );
            if lower.starts_with("https://") || lower.starts_with("http://") {
                ensure!(
                    ![
                        "x-amz-",
                        "signature=",
                        "token=",
                        "expires=",
                        "policy=",
                        "key-pair-id="
                    ]
                    .iter()
                    .any(|part| lower.contains(part)),
                    "sync_signed_url"
                );
            }
        }
        _ => (),
    }
    Ok(())
}

// A missing field is a register value, not an Automerge delete: concurrent cleared
// fields must remain visible in get_all alongside the other device's value.
fn present(value: &Value) -> Value {
    json!({"present":true,"value":value})
}
fn absent() -> Value {
    json!({"present":false})
}

fn check_entity_value(kind: &str, value: &Map<String, Value>) -> Result<()> {
    let mut portable = value.clone();
    if kind == "overlay" {
        if let Some(url) = portable.remove("sourceUrl") {
            super::validate_source_url(url.as_str().context("sync_source_url_invalid")?)?;
        }
        if let Some(url) = portable.remove("sourceThumbnailUrl") {
            super::validate_source_thumbnail_url(url.as_str().context("sync_source_url_invalid")?)?;
        }
    }
    check_value(&Value::Object(portable), 0)
}

fn validate_register(kind: &str, field: &str, value: &Value) -> Result<()> {
    if field == DELETED {
        ensure!(value.is_boolean(), "sync_tombstone_invalid");
    } else if let Some(member) = field.strip_prefix(MEMBER) {
        ensure!(kind == "collection", "sync_member_invalid");
        let pair: Vec<String> = serde_json::from_str(member).context("sync_member_invalid")?;
        ensure!(
            pair.len() == 2 && pair[0] == "productIds" && !pair[1].is_empty() && value.is_boolean(),
            "sync_member_invalid"
        );
    } else {
        ensure!(
            !field.is_empty() && !field.starts_with('@'),
            "sync_field_invalid"
        );
        ensure!(
            value.get("present").and_then(Value::as_bool).is_some(),
            "sync_register_invalid"
        );
        ensure!(
            value.as_object().map(|v| v.len())
                == Some(if value["present"] == true { 2 } else { 1 }),
            "sync_register_invalid"
        );
        if value["present"] == true {
            ensure!(value.get("value").is_some(), "sync_register_invalid");
            if kind == "overlay" && field == "sourceUrl" {
                super::validate_source_url(
                    value["value"].as_str().context("sync_source_url_invalid")?,
                )?;
            } else if kind == "overlay" && field == "sourceThumbnailUrl" {
                super::validate_source_thumbnail_url(
                    value["value"].as_str().context("sync_source_url_invalid")?,
                )?;
            } else {
                check_value(&json!({field:value["value"]}), 0)?;
            }
            if kind == "overlay" && field == "thumbnailPath" {
                let path = value["value"].as_str().context("sync_device_path")?;
                ensure!(
                    !path.is_empty()
                        && !path.contains([':', '\0'])
                        && !path.starts_with(['/', '\\'])
                        && !path
                            .split(['/', '\\'])
                            .any(|part| part.is_empty() || part == "." || part == ".."),
                    "sync_device_path"
                );
            }
            if kind == "collection" && field == "productIds" {
                ensure!(value["value"] == json!([]), "sync_member_invalid");
            }
        }
    }
    Ok(())
}

fn register_values(doc: &AutoCommit, key: &str) -> Result<Vec<(String, Value)>> {
    doc.get_all(ROOT, key)?
        .into_iter()
        .map(|(value, op)| {
            let text = value.to_str().context("sync_scalar_invalid")?;
            Ok((
                op.to_string(),
                serde_json::from_str(text).context("sync_scalar_invalid")?,
            ))
        })
        .collect()
}

fn effective(doc: &AutoCommit, key: &str) -> Result<Option<Value>> {
    doc.get(ROOT, key)?
        .map(|(value, _)| {
            serde_json::from_str(value.to_str().context("sync_scalar_invalid")?)
                .context("sync_scalar_invalid")
        })
        .transpose()
}

/// Applies only fields changed between before/after. Parent must provide the current
/// portable source entity, never the effective source+overlay+local-files projection.
pub fn write_entity(
    doc: &mut AutoCommit,
    kind: &str,
    id: &str,
    before: Option<&Value>,
    after: Option<&Value>,
) -> Result<()> {
    check_identity(kind, id)?;
    let empty = Map::new();
    // Reusing an explicitly deleted identity replaces its old fields/members.
    // Historical values still exist in Automerge, but must not leak into a newly
    // created entity whose caller legitimately has no live `before` record.
    let retained = if before.is_none()
        && after.is_some()
        && doc.get(ROOT, key(kind, id, DELETED))?.is_some()
    {
        project(doc, true)?
            .into_iter()
            .find(|(k, i, _)| k == kind && i == id)
            .map(|(_, _, v)| v)
    } else {
        None
    };
    let recreating = retained.is_some();
    let previous = before
        .or(retained.as_ref())
        .map(|v| v.as_object().context("sync_entity_invalid"))
        .transpose()?
        .unwrap_or(&empty);
    let next = after
        .map(|v| v.as_object().context("sync_entity_invalid"))
        .transpose()?;
    if let Some(next) = next {
        check_entity_value(kind, next)?;
        ensure!(
            next.keys()
                .all(|field| !field.is_empty() && !field.starts_with('@')),
            "sync_field_invalid"
        );
        if let Some(value) = next.get("id") {
            ensure!(value.as_str() == Some(id), "sync_identity_mismatch");
        }
        if kind == "collection" {
            members(next.get("productIds"))?;
        }
    }
    let mut edits: Vec<(String, Value)> = Vec::new();
    if next.is_none() {
        edits.push((key(kind, id, DELETED), json!(true)));
    } else {
        let next = next.unwrap();
        // An ordinary edit cannot resurrect a concurrent deletion.
        if before.is_none() || effective(doc, &key(kind, id, DELETED))?.is_none() {
            edits.push((key(kind, id, DELETED), json!(false)));
        }
        for field in previous.keys().chain(next.keys()).collect::<BTreeSet<_>>() {
            if !recreating && previous.get(field) == next.get(field) {
                continue;
            }
            if kind == "collection" && field == "productIds" {
                let old = members(previous.get(field))?;
                let new = members(next.get(field))?;
                for member in old.union(&new) {
                    if recreating || old.contains(member) != new.contains(member) {
                        edits.push((
                            key(kind, id, &format!("{MEMBER}{}", json!([field, member]))),
                            json!(new.contains(member)),
                        ));
                    }
                }
                edits.push((
                    key(kind, id, field),
                    if next.contains_key(field) {
                        present(&json!([]))
                    } else {
                        absent()
                    },
                ));
            } else {
                edits.push((
                    key(kind, id, field),
                    next.get(field).map(present).unwrap_or_else(absent),
                ));
            }
        }
    }
    // All fallible input checks precede mutation; SQLite supplies transactionality.
    for (key, value) in edits {
        // Recreated rows must emit even unchanged retained fields so a peer whose
        // SQLite row was deleted can incrementally reconstruct the complete row.
        // Automerge otherwise optimizes an equal put away.
        if recreating {
            doc.delete(ROOT, key.as_str())?;
        }
        doc.put(ROOT, key, value.to_string())?;
    }
    Ok(())
}

/// The SQL foreground rejects invalid portable values without loading history.
pub(super) fn validate_entity(kind: &str, id: &str, after: Option<&Value>) -> Result<()> {
    check_identity(kind, id)?;
    if let Some(after) = after {
        let object = after.as_object().context("sync_entity_invalid")?;
        check_entity_value(kind, object)?;
        ensure!(
            object
                .keys()
                .all(|field| !field.is_empty() && !field.starts_with('@')),
            "sync_field_invalid"
        );
        ensure!(after["id"].as_str() == Some(id), "sync_identity_mismatch");
        if kind == "collection" {
            members(object.get("productIds"))?;
        }
    }
    Ok(())
}

fn members(value: Option<&Value>) -> Result<BTreeSet<String>> {
    match value {
        None => Ok(BTreeSet::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .context("sync_member_invalid")
            })
            .collect(),
        _ => bail!("sync_member_invalid"),
    }
}

/// Deleted entities are absent from this complete projection; parent removes their
/// portable SQLite rows in the same transaction that stores the document bytes.
pub fn entities(doc: &AutoCommit) -> Result<Vec<(String, String, Value)>> {
    project(doc, false)
}

/// Enumerates only operations introduced since the supplied known heads. Callers
/// group these field keys by entity and apply them to their existing SQLite rows.
pub fn changed_fields(
    doc: &mut AutoCommit,
    heads: &[automerge::ChangeHash],
) -> Result<Vec<(String, String, String)>> {
    let mut fields = BTreeSet::new();
    for change in doc.get_changes(heads) {
        let expanded = serde_json::to_value(change.decode())?;
        let ops = expanded["ops"].as_array().context("sync_change_invalid")?;
        for op in ops {
            let (kind, id, field) = validate_operation(op)?;
            if ops.len() == 1
                && field == DELETED
                && semantic_register(doc, &key(&kind, &id, &field))? == Some(json!(false))
            {
                fields.insert((kind.clone(), id.clone(), RECOVER.to_owned()));
            }
            fields.insert((kind, id, field));
        }
    }
    Ok(fields.into_iter().collect())
}

/// Projects a changed entity without enumerating the document. `base` must be the
/// row corresponding to the heads used by `changed_fields`; `fields` includes all
/// changed keys for this entity. Ordinary absent/null and member add-win semantics
/// match the full projection.
pub fn project_entity(
    doc: &AutoCommit,
    kind: &str,
    id: &str,
    base: Option<Value>,
    fields: &[String],
) -> Result<Option<Value>> {
    check_identity(kind, id)?;
    let tombstones = register_values(doc, &key(kind, id, DELETED))?;
    for (_, value) in &tombstones {
        validate_register(kind, DELETED, value)?;
    }
    if tombstones.iter().any(|(_, value)| *value == true) {
        return Ok(None);
    }
    // Explicit user recovery of a tombstone writes only this flag. This rare path
    // needs retained fields after SQLite has removed the deleted row. Ordinary
    // recreation emits every field and never takes this enumeration fallback.
    let recover_retained = base.is_none()
        && (fields.iter().any(|field| field == RECOVER)
            || (fields.len() == 1 && fields[0] == DELETED));
    let mut recovered_fields = Vec::new();
    if recover_retained {
        for field_key in doc.keys(ROOT) {
            let (record_kind, record_id, field) = parse_key(&field_key)?;
            if record_kind == kind && record_id == id {
                recovered_fields.push(field);
            }
        }
    }
    let fields = if recover_retained {
        recovered_fields.as_slice()
    } else {
        fields
    };
    let mut exists = base.is_some() || !tombstones.is_empty();
    let mut record = match base {
        Some(Value::Object(record)) => record,
        Some(_) => bail!("sync_entity_invalid"),
        None => Map::new(),
    };
    let mut member_ids = if kind == "collection" {
        members(record.get("productIds"))?
    } else {
        BTreeSet::new()
    };
    let mut has_members_field = record.contains_key("productIds");
    let mut members_changed = false;
    for field in fields.iter().collect::<BTreeSet<_>>() {
        if field == DELETED || field == RECOVER {
            continue;
        }
        let field_key = key(kind, id, field);
        let values = register_values(doc, &field_key)?;
        exists |= !values.is_empty();
        for (_, value) in &values {
            validate_register(kind, field, value)?;
        }
        if let Some(member) = field.strip_prefix(MEMBER) {
            validate_register(kind, field, &json!(false))?;
            let pair: Vec<String> = serde_json::from_str(member)?;
            if values.iter().any(|(_, value)| *value == true) {
                member_ids.insert(pair[1].clone());
            } else {
                member_ids.remove(&pair[1]);
            }
            members_changed = true;
        } else if kind == "collection" && field == "productIds" {
            // This register records presence, not the entire set. Clearing it must
            // not discard unchanged member registers or concurrent additions.
            has_members_field = effective(doc, &field_key)?
                .map(|value| value["present"] == true)
                .unwrap_or(false);
            members_changed = true;
        } else if let Some(value) = effective(doc, &field_key)? {
            if value["present"] == true {
                record.insert(field.clone(), value["value"].clone());
            } else {
                record.remove(field);
            }
        } else {
            record.remove(field);
        }
    }
    if !exists {
        return Ok(None);
    }
    if members_changed {
        if has_members_field || !member_ids.is_empty() {
            record.insert("productIds".into(), json!(member_ids));
        } else {
            record.remove("productIds");
        }
    }
    record.insert("id".into(), json!(id));
    Ok(Some(Value::Object(record)))
}

fn project(doc: &AutoCommit, include_deleted: bool) -> Result<Vec<(String, String, Value)>> {
    let mut records: BTreeMap<(String, String), Map<String, Value>> = BTreeMap::new();
    let mut deleted = BTreeSet::new();
    let mut membership: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for field_key in doc.keys(ROOT) {
        let (kind, id, field) = parse_key(&field_key)?;
        let identity = (kind.clone(), id.clone());
        let values = register_values(doc, &field_key)?;
        for (_, value) in &values {
            validate_register(&kind, &field, value)?;
        }
        if field == DELETED {
            if values.iter().any(|(_, v)| *v == true) {
                deleted.insert(identity.clone());
            }
            records.entry(identity).or_default();
        } else if let Some(member) = field.strip_prefix(MEMBER) {
            let pair: Vec<String> = serde_json::from_str(member)?;
            // Add wins against a concurrent removal; a later observed removal wins.
            if values.iter().any(|(_, v)| *v == true) {
                membership
                    .entry(identity)
                    .or_default()
                    .insert(pair[1].clone());
            }
        } else if let Some(value) = effective(doc, &field_key)? {
            if value["present"] == true {
                records
                    .entry(identity)
                    .or_default()
                    .insert(field, value["value"].clone());
            }
        }
    }
    for (identity, ids) in membership {
        records
            .entry(identity)
            .or_default()
            .insert("productIds".to_owned(), json!(ids));
    }
    Ok(records
        .into_iter()
        .filter(|(identity, _)| include_deleted || !deleted.contains(identity))
        .map(|((kind, id), mut value)| {
            value.insert("id".into(), json!(id));
            (kind, id, Value::Object(value))
        })
        .collect())
}

pub fn conflicts(doc: &AutoCommit) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    for field_key in doc.keys(ROOT) {
        if let Some(conflict) = field_conflict(doc, &field_key)? {
            results.push(conflict);
        }
    }
    Ok(results)
}

/// Refresh only previously divergent registers after a local mutation. Import,
/// merge and restore must use `conflicts` to discover newly introduced conflicts.
pub fn remaining_conflicts(doc: &AutoCommit, previous: &[Value]) -> Result<Vec<Value>> {
    let mut keys = BTreeSet::new();
    for conflict in previous {
        let kind = conflict["kind"].as_str().context("sync_kind_required")?;
        let id = conflict["id"].as_str().context("sync_id_required")?;
        let field = conflict["field"]
            .as_str()
            .filter(|f| !f.is_empty())
            .context("sync_field_required")?;
        check_identity(kind, id)?;
        keys.insert(key(kind, id, field));
    }
    let mut results = Vec::new();
    for field_key in keys {
        if let Some(conflict) = field_conflict(doc, &field_key)? {
            results.push(conflict);
        }
    }
    Ok(results)
}

/// Discover new disagreements only on changed registers while refreshing all
/// previously known disagreements. Unchanged registers cannot gain a conflict.
pub fn changed_conflicts(
    doc: &AutoCommit,
    previous: &[Value],
    fields: &[(String, String, String)],
) -> Result<Vec<Value>> {
    let mut candidates = previous.to_vec();
    candidates.extend(
        fields
            .iter()
            .filter(|(_, _, field)| field != RECOVER)
            .map(|(kind, id, field)| json!({"kind":kind,"id":id,"field":field})),
    );
    remaining_conflicts(doc, &candidates)
}

fn field_conflict(doc: &AutoCommit, field_key: &str) -> Result<Option<Value>> {
    let (kind, id, field) = parse_key(field_key)?;
    let values = register_values(doc, field_key)?;
    if values.len() <= 1 {
        return Ok(None);
    }
    for (_, value) in &values {
        validate_register(&kind, &field, value)?;
    }
    // Equal values written independently carry no user-visible disagreement.
    if values.iter().all(|(_, v)| *v == values[0].1) {
        return Ok(None);
    }
    let current = if field == DELETED || field.starts_with(MEMBER) {
        Some(json!(values.iter().any(|(_, v)| *v == true)))
    } else {
        effective(doc, field_key)?
    };
    Ok(Some(
        json!({"kind":kind,"id":id,"field":field,"values":values.into_iter().map(|(op_id,value)| json!({"opId":op_id,"value":value})).collect::<Vec<_>>(),"effectiveValue":current}),
    ))
}

/// Choose a retained opId, or explicitly set a register value. Regular field values
/// use {present:true,value:...}/{present:false}; @deleted and member fields are bool.
pub fn resolve(doc: &mut AutoCommit, args: &Value) -> Result<()> {
    let kind = args["kind"].as_str().context("sync_kind_required")?;
    let id = args["id"].as_str().context("sync_id_required")?;
    let field = args["field"].as_str().context("sync_field_required")?;
    check_identity(kind, id)?;
    let field_key = key(kind, id, field);
    let value = if let Some(op) = args["opId"].as_str() {
        register_values(doc, &field_key)?
            .into_iter()
            .find(|(id, _)| id == op)
            .map(|(_, value)| value)
            .context("sync_conflict_changed")?
    } else {
        args.get("value").cloned().context("sync_value_required")?
    };
    validate_register(kind, field, &value)?;
    doc.put(ROOT, field_key, value.to_string())?;
    Ok(())
}

// Validate EVERY historical operation, not only the current winners: otherwise an
// overwritten secret could be smuggled into an apparently safe portable document.
fn validate_change(change: &Change) -> Result<()> {
    let expanded = serde_json::to_value(change.decode())?;
    ensure!(
        expanded["message"].is_null(),
        "sync_change_message_forbidden"
    );
    ensure!(
        expanded.get("extra_bytes").is_none(),
        "sync_change_extra_bytes_forbidden"
    );
    for op in expanded["ops"].as_array().context("sync_change_invalid")? {
        validate_operation(op)?;
    }
    Ok(())
}

fn validate_operation(op: &Value) -> Result<(String, String, String)> {
    ensure!(op["obj"] == "_root", "sync_operation_invalid");
    let (kind, id, field) = parse_key(op["key"].as_str().context("sync_key_invalid")?)?;
    let object = op.as_object().context("sync_operation_invalid")?;
    ensure!(op["pred"].is_array(), "sync_operation_invalid");
    if op["action"] == "set" {
        ensure!(
            object.len() == 5
                && object
                    .keys()
                    .all(|key| ["action", "obj", "key", "pred", "value"].contains(&key.as_str())),
            "sync_operation_invalid"
        );
        let register: Value =
            serde_json::from_str(op["value"].as_str().context("sync_scalar_invalid")?)?;
        validate_register(&kind, &field, &register)?;
    } else if op["action"] == "del" {
        // Automerge 0.7.4 OpsFound::resolve_action emits a native delete of
        // losing predecessors when put selects the existing scalar winner.
        // A delete carries no value/type/insert payload; historical sets are
        // still individually validated, including values later deleted here.
        ensure!(
            object.len() == 4
                && object
                    .keys()
                    .all(|key| ["action", "obj", "key", "pred"].contains(&key.as_str())),
            "sync_operation_invalid"
        );
        ensure!(
            !op["pred"].as_array().unwrap().is_empty(),
            "sync_operation_invalid"
        );
        let probe = if field == DELETED || field.starts_with(MEMBER) {
            json!(false)
        } else if kind == "collection" && field == "productIds" {
            present(&json!([]))
        } else if kind == "overlay" && field == "thumbnailPath" {
            // A native delete has no value. Validate its field policy using a
            // valid relative-path probe; historical sets validate actual paths.
            present(&json!("media/validation.png"))
        } else if kind == "overlay" && matches!(field.as_str(), "sourceUrl" | "sourceThumbnailUrl")
        {
            // Deletions have no URL payload; historical sets retain full checks.
            present(&json!(""))
        } else {
            present(&Value::Null)
        };
        validate_register(&kind, &field, &probe)?;
    } else {
        bail!("sync_operation_invalid");
    }
    Ok((kind, id, field))
}

fn validate_doc(doc: &mut AutoCommit) -> Result<()> {
    for change in doc.get_changes(&[]) {
        validate_change(&change)?;
    }
    entities(doc)?;
    Ok(())
}

fn validated_snapshot(bytes: &[u8]) -> Result<AutoCommit> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= 256 * 1024 * 1024,
        "sync_snapshot_size_invalid"
    );
    let mut snapshot =
        AutoCommit::load(bytes).map_err(|_| anyhow::anyhow!("sync_snapshot_invalid"))?;
    ensure!(
        snapshot.get_missing_deps(&[]).is_empty(),
        "sync_snapshot_dependencies_missing"
    );
    validate_doc(&mut snapshot)?;
    Ok(snapshot)
}

/// Preflight an untrusted portable snapshot without modifying the live document.
/// The manifest owner additionally checks its version and external SHA256 envelope.
pub fn validate_snapshot(bytes: &[u8]) -> Result<()> {
    validated_snapshot(bytes)?;
    Ok(())
}

fn semantic_register(doc: &AutoCommit, field_key: &str) -> Result<Option<Value>> {
    let (_, _, field) = parse_key(field_key)?;
    if field == DELETED || field.starts_with(MEMBER) {
        let values = register_values(doc, field_key)?;
        Ok(if values.is_empty() {
            None
        } else {
            Some(json!(values.iter().any(|(_, value)| *value == true)))
        })
    } else {
        effective(doc, field_key)
    }
}

fn distinct_registers(values: &[(String, Value)]) -> BTreeMap<String, Value> {
    values
        .iter()
        .map(|(_, value)| (value.to_string(), value.clone()))
        .collect()
}

fn restore_register_conflict(
    doc: &mut AutoCommit,
    field_key: &str,
    choices: &BTreeMap<String, Value>,
    winner: &Value,
) -> Result<()> {
    let (_, _, field) = parse_key(field_key)?;
    ensure!(
        choices.len() > 1 && choices.contains_key(&winner.to_string()),
        "sync_snapshot_conflict_invalid"
    );
    let heads = doc.get_heads();
    let baseline = doc.fork_at(&heads)?;
    // Every branch starts at the same heads and writes delete+set, so the set
    // counters are equal. Automerge's greatest ActorId chooses the scalar
    // winner; fresh actors avoid sequence reuse with this or any other device.
    let mut actors: BTreeSet<Vec<u8>> = BTreeSet::new();
    while actors.len() < choices.len() {
        actors.insert(Sha256::digest(uuid::Uuid::new_v4().as_bytes()).to_vec());
    }
    let mut values: Vec<Value> = choices
        .values()
        .filter(|value| *value != winner)
        .cloned()
        .collect();
    values.push(winner.clone());
    for (actor, value) in actors.into_iter().zip(values) {
        let mut branch = baseline.clone();
        branch.set_actor(ActorId::from(actor));
        // A plain put of the baseline winner is optimized away by Automerge.
        // Removing it first guarantees a new concurrent set on EVERY branch.
        branch.delete(ROOT, field_key)?;
        branch.put(ROOT, field_key, value.to_string())?;
        branch.commit();
        doc.merge(&mut branch)?;
    }
    ensure!(
        distinct_registers(&register_values(doc, field_key)?) == *choices,
        "sync_snapshot_conflict_mismatch"
    );
    ensure!(
        semantic_register(doc, field_key)?.as_ref() == Some(winner),
        "sync_snapshot_winner_mismatch"
    );
    // Bool registers use the documented delete/add-win projection. Ordinary
    // registers additionally require Automerge's actual scalar winner to match.
    if field != DELETED && !field.starts_with(MEMBER) {
        ensure!(
            effective(doc, field_key)?.as_ref() == Some(winner),
            "sync_snapshot_winner_mismatch"
        );
    }
    Ok(())
}

/// Restore portable semantics onto existing causal history. The caller's actor
/// remains unchanged. Saved conflicts survive unless the corresponding desired
/// field intentionally differs from the saved winner (e.g. thumbnail remapping).
/// Work happens in a candidate; validation/postcondition failures leave `doc` intact.
pub fn restore_snapshot(
    doc: &mut AutoCommit,
    bytes: &[u8],
    desired: &[(String, String, Value)],
) -> Result<()> {
    let mut snapshot = validated_snapshot(bytes)?;
    let saved_live: BTreeSet<(String, String)> = entities(&snapshot)?
        .into_iter()
        .map(|(kind, id, _)| (kind, id))
        .collect();
    let mut wanted = BTreeMap::new();
    for (kind, id, value) in desired {
        check_identity(kind, id)?;
        let mut value = value.as_object().cloned().context("sync_entity_invalid")?;
        ensure!(
            value.get("id").and_then(Value::as_str) == Some(id.as_str()),
            "sync_identity_mismatch"
        );
        if kind == "collection" && value.contains_key("productIds") {
            value.insert(
                "productIds".into(),
                json!(members(value.get("productIds"))?),
            );
        }
        check_entity_value(kind, &value)?;
        ensure!(
            wanted
                .insert((kind.clone(), id.clone()), Value::Object(value))
                .is_none(),
            "sync_snapshot_duplicate_entity"
        );
    }
    let mut saved = BTreeMap::new();
    let mut saved_identities = BTreeSet::new();
    for field_key in snapshot.keys(ROOT) {
        let (kind, id, _) = parse_key(&field_key)?;
        saved_identities.insert((kind, id));
        let winner =
            semantic_register(&snapshot, &field_key)?.context("sync_snapshot_register_missing")?;
        let choices = distinct_registers(&register_values(&snapshot, &field_key)?);
        saved.insert(field_key, (winner, choices));
    }
    let mut candidate = doc.clone();
    candidate.merge(&mut snapshot)?;
    let mut writes = 0;
    // Restore all retained registers, including nonconflicted fields of deleted
    // entities. Live-only JSON records cannot represent this recovery information.
    for (field_key, (winner, choices)) in &saved {
        if semantic_register(&candidate, field_key)?.as_ref() != Some(winner)
            || distinct_registers(&register_values(&candidate, field_key)?) != *choices
        {
            candidate.put(ROOT, field_key.as_str(), winner.to_string())?;
            writes += 1;
            if writes % 128 == 0 {
                candidate.commit();
            }
        }
    }
    let candidate_keys: Vec<String> = candidate.keys(ROOT).collect();
    for field_key in candidate_keys {
        let (kind, id, field) = parse_key(&field_key)?;
        if saved_identities.contains(&(kind, id)) && !saved.contains_key(&field_key) {
            let value = if field == DELETED || field.starts_with(MEMBER) {
                json!(false)
            } else {
                absent()
            };
            candidate.put(ROOT, field_key, value.to_string())?;
            writes += 1;
            if writes % 128 == 0 {
                candidate.commit();
            }
        }
    }
    let current: BTreeMap<(String, String), Value> = entities(&candidate)?
        .into_iter()
        .map(|(kind, id, value)| ((kind, id), value))
        .collect();
    for ((kind, id), value) in &current {
        if !wanted.contains_key(&(kind.clone(), id.clone())) {
            write_entity(&mut candidate, kind, id, Some(value), None)?;
        }
    }
    for ((kind, id), value) in &wanted {
        write_entity(
            &mut candidate,
            kind,
            id,
            current.get(&(kind.clone(), id.clone())),
            Some(value),
        )?;
        writes += 1;
        if writes % 128 == 0 {
            candidate.commit();
        }
    }
    for (field_key, (winner, choices)) in &saved {
        if choices.len() <= 1 {
            continue;
        }
        let (kind, id, field) = parse_key(field_key)?;
        let identity = (kind.clone(), id.clone());
        let desired = wanted.get(&identity);
        let preserve = if field == DELETED {
            *winner == json!(desired.is_none())
        } else if let Some(record) = desired {
            if let Some(member) = field.strip_prefix(MEMBER) {
                let pair: Vec<String> = serde_json::from_str(member)?;
                *winner == json!(members(record.get("productIds"))?.contains(&pair[1]))
            } else {
                *winner == record.get(&field).map(present).unwrap_or_else(absent)
            }
        } else {
            !saved_live.contains(&identity)
        };
        if preserve
            && (semantic_register(&candidate, field_key)?.as_ref() != Some(winner)
                || distinct_registers(&register_values(&candidate, field_key)?) != *choices)
        {
            restore_register_conflict(&mut candidate, field_key, choices, winner)?;
        }
    }
    let actual: BTreeMap<(String, String), Value> = entities(&candidate)?
        .into_iter()
        .map(|(kind, id, value)| ((kind, id), value))
        .collect();
    ensure!(actual == wanted, "sync_snapshot_projection_mismatch");
    ensure!(
        candidate.get_missing_deps(&[]).is_empty(),
        "sync_snapshot_dependencies_missing"
    );
    *doc = candidate;
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn publish(folder: &Path, bytes: &[u8]) -> Result<bool> {
    let target = folder.join(format!("{}.amchange", digest(bytes)));
    if target.exists() {
        ensure!(fs::read(&target)? == bytes, "sync_change_checksum_mismatch");
        return Ok(false);
    }
    let temporary = folder.join(format!(".{}.partial", uuid::Uuid::new_v4()));
    let result = (|| -> Result<bool> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // Identical writers may race. A final file is only ever a complete immutable
        // checksum-addressed payload, so a POSIX replacement is identical content.
        match fs::rename(&temporary, &target) {
            Ok(()) => (),
            Err(_) if target.exists() && fs::read(&target)? == bytes => {
                fs::remove_file(&temporary)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        fs::File::open(folder)?.sync_all()?;
        Ok(true)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Imports into a candidate first. Caller commits returned document+projection in
/// SQLite atomically. Exported changes are replay-safe after any SQLite rollback.
pub fn exchange(doc: &mut AutoCommit, folder: &Path) -> Result<Value> {
    validate_doc(doc)?;
    let mut result = exchange_validated(doc, folder)?;
    result["conflicts"] = json!(conflicts(doc)?.len());
    Ok(result)
}

/// The catalogue calls this with its already validated durable document. Every
/// folder payload is still checksum checked; only previously validated content
/// can skip operation decoding. Unknown history is validated before any apply.
pub(super) fn exchange_validated(doc: &mut AutoCommit, folder: &Path) -> Result<Value> {
    fs::create_dir_all(folder).context("sync_folder_unavailable")?;
    let mut candidate = doc.clone();
    let known_files: BTreeSet<String> = candidate
        .get_changes(&[])
        .iter()
        .map(|change| format!("{}.amchange", digest(change.raw_bytes())))
        .collect();
    let mut incoming = Vec::new();
    let mut ignored = 0;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if entry.path().extension().and_then(|e| e.to_str()) != Some("amchange") {
            ignored += 1;
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= MAX_CHANGE,
            "sync_change_file_invalid"
        );
        let bytes = fs::read(entry.path())?;
        ensure!(
            entry.file_name().to_string_lossy() == format!("{}.amchange", digest(&bytes)),
            "sync_change_checksum_mismatch"
        );
        if known_files.contains(entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        let change =
            Change::from_bytes(bytes).map_err(|_| anyhow::anyhow!("sync_change_invalid"))?;
        validate_change(&change)?;
        if candidate.get_change_by_hash(&change.hash()).is_none() {
            incoming.push(change);
        }
    }
    let mut selected = Vec::new();
    let mut scheduled = BTreeSet::new();
    // Keep unresolved changes durably in the folder and retry next exchange. Never
    // rely on whether an Automerge save happens to include its dependency queue.
    loop {
        let mut remaining = Vec::new();
        let before = selected.len();
        for change in incoming {
            if change
                .deps()
                .iter()
                .all(|dep| scheduled.contains(dep) || candidate.get_change_by_hash(dep).is_some())
            {
                scheduled.insert(change.hash());
                selected.push(change);
            } else {
                remaining.push(change);
            }
        }
        incoming = remaining;
        if incoming.is_empty() || before == selected.len() {
            break;
        }
    }
    let imported = selected.len();
    if !selected.is_empty() {
        candidate.apply_changes(selected)?;
    }
    let mut exported = 0;
    for change in candidate.get_changes(&[]) {
        ensure!(
            change.raw_bytes().len() as u64 <= MAX_CHANGE,
            "sync_change_too_large"
        );
        if publish(folder, change.raw_bytes())? {
            exported += 1;
        }
    }
    *doc = candidate;
    Ok(
        json!({"imported":imported,"exported":exported,"pendingDependencies":incoming.len(),"ignoredFiles":ignored}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetched_source_thumbnail_register_rejects_unsafe_history_and_retains_empty() -> Result<()> {
        let safe = "https://booth.pximg.net/synthetic-fetched.png";
        for value in [safe, ""] {
            validate_register("overlay", "sourceThumbnailUrl", &present(&json!(value)))?;
        }
        for value in [
            "http://booth.pximg.net/a",
            "https://example.org/a",
            "https://booth.pximg.net/a?token=synthetic",
            "https://booth.pximg.net:443/a",
            "https://u:p@booth.pximg.net/a",
        ] {
            assert!(
                validate_register("overlay", "sourceThumbnailUrl", &present(&json!(value)))
                    .is_err()
            );
            let mut history = new_doc("synthetic-thumbnail-history")?;
            history.put(
                ROOT,
                key("overlay", "p", "sourceThumbnailUrl"),
                present(&json!(value)).to_string(),
            )?;
            history.commit();
            history.put(
                ROOT,
                key("overlay", "p", "sourceThumbnailUrl"),
                present(&json!(safe)).to_string(),
            )?;
            assert!(validate_snapshot(&history.save()).is_err());
        }
        let mut doc = new_doc("synthetic-thumbnail-device")?;
        let value = json!({"id":"p","sourceThumbnailUrl":""});
        write_entity(&mut doc, "overlay", "p", None, Some(&value))?;
        doc = load_doc(&doc.save(), "synthetic-thumbnail-device")?;
        assert_eq!(entity(&doc, "overlay", "p").unwrap(), value);
        doc.delete(ROOT, key("overlay", "p", "sourceThumbnailUrl"))?;
        validate_snapshot(&doc.save())?;
        Ok(())
    }

    #[test]
    fn overlay_source_urls_roundtrip_and_unsafe_historical_values_are_rejected() -> Result<()> {
        let safe = "https://example.org/custom-assets/42?variant=blue#files";
        for url in [
            "",
            "http://example.org/asset",
            safe,
            "https://example.org/assets/token=public",
        ] {
            validate_register("overlay", "sourceUrl", &present(&json!(url)))?;
        }
        for url in [
            "javascript:alert(1)",
            "file:///synthetic.asset",
            "ftp://example.org/asset",
            "https://synthetic-user:synthetic-password@example.org/asset",
            "/relative/asset",
            "https://example.org/asset?token=synthetic",
            "https://example.org/asset?signature=synthetic",
            "https://example.org/asset?X-Amz-Signature=synthetic",
        ] {
            assert!(validate_register("overlay", "sourceUrl", &present(&json!(url))).is_err());
            let mut history = new_doc("unsafe-url-history")?;
            history.put(
                ROOT,
                key("overlay", "p", "sourceUrl"),
                present(&json!(url)).to_string(),
            )?;
            history.commit();
            history.put(
                ROOT,
                key("overlay", "p", "sourceUrl"),
                present(&json!(safe)).to_string(),
            )?;
            assert!(validate_snapshot(&history.save()).is_err());
        }
        let mut doc = new_doc("source-url-device")?;
        let before = json!({"id":"p","sourceUrl":safe});
        write_entity(&mut doc, "overlay", "p", None, Some(&before))?;
        doc = load_doc(&doc.save(), "source-url-device")?;
        assert_eq!(entity(&doc, "overlay", "p").unwrap(), before);
        let cleared = json!({"id":"p","sourceUrl":""});
        write_entity(&mut doc, "overlay", "p", Some(&before), Some(&cleared))?;
        doc = load_doc(&doc.save(), "source-url-device")?;
        assert_eq!(entity(&doc, "overlay", "p").unwrap(), cleared);
        doc.delete(ROOT, key("overlay", "p", "sourceUrl"))?;
        validate_snapshot(&doc.save())?;
        Ok(())
    }

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/catalogue-synthetic")
                .join(format!("nna-sync-synthetic-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn entity(doc: &AutoCommit, kind: &str, id: &str) -> Option<Value> {
        entities(doc)
            .unwrap()
            .into_iter()
            .find(|(k, i, _)| k == kind && i == id)
            .map(|(_, _, v)| v)
    }

    #[test]
    fn two_devices_offline_fields_conflicts_restart_and_resolution() -> Result<()> {
        let folder = Scratch::new();
        let mut a = new_doc("synthetic-device-a")?;
        let baseline =
            json!({"id":"p1","name":"Original","notes":"Initial","rejectedTags":["rejected"]});
        write_entity(&mut a, "overlay", "p1", None, Some(&baseline))?;
        exchange(&mut a, &folder.0)?;
        let mut b = new_doc("synthetic-device-b")?;
        exchange(&mut b, &folder.0)?;
        let av = json!({"id":"p1","name":"A name","notes":"A note","rejectedTags":["rejected"]});
        let bv = json!({"id":"p1","name":"B name","notes":"Initial","rejectedTags":["rejected","another"]});
        write_entity(&mut a, "overlay", "p1", Some(&baseline), Some(&av))?;
        write_entity(&mut b, "overlay", "p1", Some(&baseline), Some(&bv))?;
        a = load_doc(&a.save(), "synthetic-device-a")?;
        b = load_doc(&b.save(), "synthetic-device-b")?;
        exchange(&mut a, &folder.0)?;
        exchange(&mut b, &folder.0)?;
        exchange(&mut a, &folder.0)?;
        assert_eq!(entities(&a)?, entities(&b)?);
        let merged = entity(&a, "overlay", "p1").unwrap();
        assert_eq!(merged["notes"], "A note");
        assert_eq!(merged["rejectedTags"], bv["rejectedTags"]);
        let conflicts = conflicts(&a)?;
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0]["field"], "name");
        let op = conflicts[0]["values"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["value"]["value"] == "A name")
            .unwrap()["opId"]
            .clone();
        resolve(
            &mut a,
            &json!({"kind":"overlay","id":"p1","field":"name","opId":op}),
        )?;
        exchange(&mut a, &folder.0)?;
        exchange(&mut b, &folder.0)?;
        assert_eq!(entity(&b, "overlay", "p1").unwrap()["name"], "A name");
        assert!(super::conflicts(&b)?.is_empty());
        let repeat = exchange(&mut b, &folder.0)?;
        assert_eq!(repeat["imported"], 0);
        assert_eq!(repeat["exported"], 0);
        Ok(())
    }

    #[test]
    fn collections_independent_additions_and_delete_vs_edit_survive_restart() -> Result<()> {
        let mut a = new_doc("a")?;
        let initial = json!({"id":"c","name":"Collection","productIds":[]});
        write_entity(&mut a, "collection", "c", None, Some(&initial))?;
        let mut b = load_doc(&a.save(), "b")?;
        let av = json!({"id":"c","name":"Collection","productIds":["one"]});
        let bv = json!({"id":"c","name":"Collection","productIds":["two"]});
        write_entity(&mut a, "collection", "c", Some(&initial), Some(&av))?;
        write_entity(&mut b, "collection", "c", Some(&initial), Some(&bv))?;
        a.merge(&mut b)?;
        b.merge(&mut a)?;
        assert_eq!(
            entity(&a, "collection", "c").unwrap()["productIds"],
            json!(["one", "two"])
        );
        let before = entity(&a, "collection", "c").unwrap();
        write_entity(&mut a, "collection", "c", Some(&before), None)?;
        let mut edited = before.clone();
        edited["name"] = json!("Offline edit");
        write_entity(&mut b, "collection", "c", Some(&before), Some(&edited))?;
        a.merge(&mut b)?;
        a = load_doc(&a.save(), "a")?;
        assert!(entity(&a, "collection", "c").is_none());
        resolve(
            &mut a,
            &json!({"kind":"collection","id":"c","field":DELETED,"value":false}),
        )?;
        assert_eq!(
            entity(&a, "collection", "c").unwrap()["name"],
            "Offline edit"
        );
        let restored = entity(&a, "collection", "c").unwrap();
        write_entity(&mut a, "collection", "c", Some(&restored), None)?;
        write_entity(
            &mut a,
            "collection",
            "c",
            None,
            Some(&json!({"id":"c","name":"Fresh","productIds":[]})),
        )?;
        assert_eq!(
            entity(&a, "collection", "c").unwrap()["productIds"],
            json!([])
        );
        Ok(())
    }

    #[test]
    fn out_of_order_partial_checksum_and_private_history() -> Result<()> {
        let folder = Scratch::new();
        let mut a = new_doc("a")?;
        let first = json!({"id":"p","name":"First"});
        write_entity(&mut a, "product", "p", None, Some(&first))?;
        a.commit();
        write_entity(
            &mut a,
            "product",
            "p",
            Some(&first),
            Some(&json!({"id":"p","name":"Second"})),
        )?;
        a.commit();
        let changes = a.get_changes(&[]);
        publish(&folder.0, changes[1].raw_bytes())?;
        fs::write(folder.0.join("interrupted.partial"), b"unfinished")?;
        let mut b = new_doc("b")?;
        assert_eq!(exchange(&mut b, &folder.0)?["pendingDependencies"], 1);
        b = load_doc(&b.save(), "b")?;
        publish(&folder.0, changes[0].raw_bytes())?;
        exchange(&mut b, &folder.0)?;
        assert_eq!(entity(&b, "product", "p").unwrap()["name"], "Second");
        let before = b.save();
        fs::write(
            folder.0.join(format!("{}.amchange", "0".repeat(64))),
            b"bad",
        )?;
        assert!(exchange(&mut b, &folder.0).is_err());
        assert_eq!(b.save(), before);
        assert!(write_entity(
            &mut b,
            "account",
            "secret",
            None,
            Some(&json!({"id":"secret"}))
        )
        .is_err());
        assert!(write_entity(
            &mut b,
            "product",
            "p",
            None,
            Some(&json!({"id":"p","cookies":"private"}))
        )
        .is_err());
        let mut poison = new_doc("malformed")?;
        poison.put(
            ROOT,
            key("product", "p", "cookies"),
            present(&json!("private")).to_string(),
        )?;
        poison.commit();
        poison.put(ROOT, key("product", "p", "cookies"), absent().to_string())?;
        assert!(load_doc(&poison.save(), "b").is_err());
        Ok(())
    }

    #[test]
    fn concurrent_field_clear_is_retained_and_can_be_selected() -> Result<()> {
        let mut a = new_doc("a")?;
        let first = json!({"id":"p","notes":"Before"});
        write_entity(&mut a, "overlay", "p", None, Some(&first))?;
        let mut b = load_doc(&a.save(), "b")?;
        write_entity(
            &mut a,
            "overlay",
            "p",
            Some(&first),
            Some(&json!({"id":"p"})),
        )?;
        write_entity(
            &mut b,
            "overlay",
            "p",
            Some(&first),
            Some(&json!({"id":"p","notes":null})),
        )?;
        a.merge(&mut b)?;
        let conflicts = conflicts(&a)?;
        assert_eq!(conflicts.len(), 1);
        let missing = conflicts[0]["values"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["value"]["present"] == false)
            .unwrap()["opId"]
            .clone();
        resolve(
            &mut a,
            &json!({"kind":"overlay","id":"p","field":"notes","opId":missing}),
        )?;
        assert!(entity(&a, "overlay", "p").unwrap().get("notes").is_none());
        Ok(())
    }

    #[test]
    fn cached_conflicts_refresh_only_previous_keys_and_local_write_resolves() -> Result<()> {
        let mut a = new_doc("a")?;
        let first = json!({"id":"p","name":"Before","notes":"Before"});
        write_entity(&mut a, "overlay", "p", None, Some(&first))?;
        let mut b = load_doc(&a.save(), "b")?;
        write_entity(
            &mut a,
            "overlay",
            "p",
            Some(&first),
            Some(&json!({"id":"p","name":"A","notes":"A"})),
        )?;
        write_entity(
            &mut b,
            "overlay",
            "p",
            Some(&first),
            Some(&json!({"id":"p","name":"B","notes":"B"})),
        )?;
        a.merge(&mut b)?;
        let previous = conflicts(&a)?;
        assert_eq!(previous.len(), 2);
        assert_eq!(remaining_conflicts(&a, &previous)?, previous);
        let notes = previous
            .iter()
            .find(|c| c["field"] == "notes")
            .unwrap()
            .clone();
        assert_eq!(
            remaining_conflicts(&a, &[notes.clone()])?,
            vec![notes.clone()]
        );
        let before = entity(&a, "overlay", "p").unwrap();
        let mut after = before.clone();
        after["notes"] = json!("Resolved locally");
        write_entity(&mut a, "overlay", "p", Some(&before), Some(&after))?;
        assert!(remaining_conflicts(&a, &[notes])?.is_empty());
        let remaining = remaining_conflicts(&a, &previous)?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["field"], "name");
        assert_eq!(remaining, conflicts(&a)?);
        assert!(remaining_conflicts(&a, &[])?.is_empty());
        Ok(())
    }

    #[test]
    fn existing_winner_resolution_native_delete_survives_validation_and_restart() -> Result<()> {
        let mut a = new_doc("winner-a")?;
        let initial = json!({"id":"p","name":"Initial"});
        write_entity(&mut a, "overlay", "p", None, Some(&initial))?;
        let mut b = load_doc(&a.save(), "winner-b")?;
        write_entity(
            &mut a,
            "overlay",
            "p",
            Some(&initial),
            Some(&json!({"id":"p","name":"A"})),
        )?;
        write_entity(
            &mut b,
            "overlay",
            "p",
            Some(&initial),
            Some(&json!({"id":"p","name":"B"})),
        )?;
        a.merge(&mut b)?;
        let conflict = conflicts(&a)?.remove(0);
        let winner = conflict["values"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["value"] == conflict["effectiveValue"])
            .unwrap();
        let heads = a.get_heads();
        resolve(
            &mut a,
            &json!({"kind":"overlay","id":"p","field":"name","opId":winner["opId"]}),
        )?;
        let changes = a.get_changes(&heads);
        assert!(changes.iter().any(
            |change| serde_json::to_value(change.decode()).unwrap()["ops"]
                .as_array()
                .unwrap()
                .iter()
                .any(|op| op["action"] == "del")
        ));
        for change in &changes {
            validate_change(change)?;
        }
        assert_eq!(
            changed_fields(&mut a, &heads)?,
            vec![("overlay".into(), "p".into(), "name".into())]
        );
        let a = load_doc(&a.save(), "winner-a")?;
        assert!(conflicts(&a)?.is_empty());
        assert_eq!(
            entity(&a, "overlay", "p").unwrap()["name"],
            conflict["effectiveValue"]["value"]
        );
        let delete =
            json!({"action":"del","obj":"_root","key":key("overlay","p","name"),"pred":["1@aa"]});
        validate_operation(&delete)?;
        let mut payload = delete.clone();
        payload["value"] = json!("unexpected");
        assert!(validate_operation(&payload).is_err());
        let mut private = delete.clone();
        private["key"] = json!(key("overlay", "p", "cookies"));
        assert!(validate_operation(&private).is_err());
        let mut poison = new_doc("private-history")?;
        poison.put(
            ROOT,
            key("product", "p", "cookies"),
            present(&json!("private")).to_string(),
        )?;
        poison.commit();
        poison.delete(ROOT, key("product", "p", "cookies"))?;
        assert!(validate_snapshot(&poison.save()).is_err());
        for path in [
            "",
            "X:\\synthetic\\thumbnail.png",
            "/synthetic.png",
            "\\synthetic.png",
            "../synthetic.png",
            "thumbs/./synthetic.png",
            "thumbs//synthetic.png",
            "thumbs/synthetic\0.png",
        ] {
            assert!(validate_register("overlay", "thumbnailPath", &present(&json!(path))).is_err());
        }
        let mut path_history = new_doc("synthetic-path-history")?;
        path_history.put(
            ROOT,
            key("overlay", "p", "thumbnailPath"),
            present(&json!("X:\\synthetic\\thumbnail.png")).to_string(),
        )?;
        path_history.commit();
        path_history.put(
            ROOT,
            key("overlay", "p", "thumbnailPath"),
            present(&json!("thumbs/safe.png")).to_string(),
        )?;
        assert!(validate_snapshot(&path_history.save()).is_err());
        Ok(())
    }

    #[test]
    fn incremental_projection_matches_full_merge_delete_and_collection_changes() -> Result<()> {
        fn full(doc: &AutoCommit) -> Result<BTreeMap<(String, String), Value>> {
            Ok(entities(doc)?
                .into_iter()
                .map(|(kind, id, value)| ((kind, id), value))
                .collect())
        }
        fn apply(
            doc: &mut AutoCommit,
            heads: &[automerge::ChangeHash],
            rows: &mut BTreeMap<(String, String), Value>,
        ) -> Result<()> {
            let mut grouped: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
            for (kind, id, field) in changed_fields(doc, heads)? {
                grouped.entry((kind, id)).or_default().push(field);
            }
            for ((kind, id), fields) in grouped {
                let identity = (kind.clone(), id.clone());
                let next = project_entity(doc, &kind, &id, rows.get(&identity).cloned(), &fields)?;
                if let Some(next) = next {
                    rows.insert(identity, next);
                } else {
                    rows.remove(&identity);
                }
            }
            assert_eq!(*rows, full(doc)?);
            Ok(())
        }
        let mut a = new_doc("a")?;
        let overlay = json!({"id":"p","name":"Before","notes":"Before"});
        let collection = json!({"id":"c","name":"Collection","productIds":["keep"]});
        write_entity(&mut a, "overlay", "p", None, Some(&overlay))?;
        write_entity(&mut a, "collection", "c", None, Some(&collection))?;
        let mut b = load_doc(&a.save(), "b")?;
        write_entity(
            &mut a,
            "overlay",
            "p",
            Some(&overlay),
            Some(&json!({"id":"p","name":"A","notes":"Before"})),
        )?;
        write_entity(
            &mut a,
            "collection",
            "c",
            Some(&collection),
            Some(&json!({"id":"c","name":"Collection","productIds":["keep","a"]})),
        )?;
        let mut rows = full(&a)?;
        let heads = a.get_heads();
        write_entity(
            &mut b,
            "overlay",
            "p",
            Some(&overlay),
            Some(&json!({"id":"p","name":"Before","notes":null})),
        )?;
        write_entity(
            &mut b,
            "collection",
            "c",
            Some(&collection),
            Some(&json!({"id":"c","name":"Collection","productIds":["keep","b"]})),
        )?;
        a.merge(&mut b)?;
        apply(&mut a, &heads, &mut rows)?;
        assert_eq!(
            rows[&("collection".into(), "c".into())]["productIds"],
            json!(["a", "b", "keep"])
        );
        let heads = a.get_heads();
        let before = entity(&a, "collection", "c").unwrap();
        let mut after = before.clone();
        after["productIds"] = json!(["keep"]);
        write_entity(&mut a, "collection", "c", Some(&before), Some(&after))?;
        apply(&mut a, &heads, &mut rows)?;
        let heads = a.get_heads();
        let before = entity(&a, "overlay", "p").unwrap();
        write_entity(&mut a, "overlay", "p", Some(&before), None)?;
        apply(&mut a, &heads, &mut rows)?;
        assert!(project_entity(&a, "overlay", "p", None, &[])?.is_none());
        let heads = a.get_heads();
        let mut recreated = before.clone();
        recreated["notes"] = json!("Recreated note");
        write_entity(&mut a, "overlay", "p", None, Some(&recreated))?;
        apply(&mut a, &heads, &mut rows)?;
        let heads = a.get_heads();
        let before = entity(&a, "collection", "c").unwrap();
        write_entity(&mut a, "collection", "c", Some(&before), None)?;
        apply(&mut a, &heads, &mut rows)?;
        let heads = a.get_heads();
        resolve(
            &mut a,
            &json!({"kind":"collection","id":"c","field":DELETED,"value":false}),
        )?;
        a.commit();
        let before = entity(&a, "collection", "c").unwrap();
        let mut after = before.clone();
        after["name"] = json!("Edited after recovery");
        write_entity(&mut a, "collection", "c", Some(&before), Some(&after))?;
        apply(&mut a, &heads, &mut rows)?;
        let heads = a.get_heads();
        assert!(changed_fields(&mut a, &heads)?.is_empty());
        Ok(())
    }

    #[test]
    fn snapshot_restores_conflicts_fresh_and_causally_preserves_modified_field_intent() -> Result<()>
    {
        let mut a = new_doc("backup-a")?;
        let original = json!({"id":"p","name":"Initial","notes":"Retained","thumbnailPath":"thumbs/initial.png"});
        let hidden = json!({"id":"hidden","name":"Deleted collection","productIds":["p"]});
        write_entity(&mut a, "overlay", "p", None, Some(&original))?;
        write_entity(&mut a, "collection", "hidden", None, Some(&hidden))?;
        let mut b = load_doc(&a.save(), "backup-b")?;
        write_entity(
            &mut a,
            "overlay",
            "p",
            Some(&original),
            Some(&json!({"id":"p","name":"A","notes":"Retained","thumbnailPath":"thumbs/a.png"})),
        )?;
        write_entity(
            &mut b,
            "overlay",
            "p",
            Some(&original),
            Some(&json!({"id":"p","name":"B","notes":"Retained","thumbnailPath":"thumbs/b.png"})),
        )?;
        a.merge(&mut b)?;
        write_entity(&mut a, "collection", "hidden", Some(&hidden), None)?;
        let desired = entities(&a)?;
        let saved_conflicts = conflicts(&a)?;
        assert_eq!(saved_conflicts.len(), 2);
        let bytes = a.save();
        validate_snapshot(&bytes)?;
        let mut fresh = new_doc("fresh-device")?;
        restore_snapshot(&mut fresh, &bytes, &desired)?;
        assert_eq!(entities(&fresh)?, desired);
        assert_eq!(conflicts(&fresh)?, saved_conflicts);
        fresh = load_doc(&fresh.save(), "fresh-device")?;
        assert_eq!(conflicts(&fresh)?, saved_conflicts);
        let name_conflict = saved_conflicts
            .iter()
            .find(|c| c["field"] == "name")
            .unwrap();
        let alternative = name_conflict["values"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["value"] != name_conflict["effectiveValue"])
            .unwrap();
        resolve(
            &mut fresh,
            &json!({"kind":"overlay","id":"p","field":"name","opId":alternative["opId"]}),
        )?;
        assert_eq!(
            entity(&fresh, "overlay", "p").unwrap()["name"],
            alternative["value"]["value"]
        );

        let mut existing = load_doc(&bytes, "existing-device")?;
        let before = entity(&existing, "overlay", "p").unwrap();
        let mut later = before.clone();
        later["name"] = json!("Later edit");
        later["thumbnailPath"] = json!("thumbs/later.png");
        write_entity(&mut existing, "overlay", "p", Some(&before), Some(&later))?;
        write_entity(
            &mut existing,
            "product",
            "local",
            None,
            Some(&json!({"id":"local","name":"Local extra"})),
        )?;
        let actor = existing.get_actor().clone();
        let mut remapped = desired.clone();
        remapped
            .iter_mut()
            .find(|(k, i, _)| k == "overlay" && i == "p")
            .unwrap()
            .2["thumbnailPath"] = json!("thumbs/restored.png");
        restore_snapshot(&mut existing, &bytes, &remapped)?;
        assert_eq!(*existing.get_actor(), actor);
        assert_eq!(entities(&existing)?, remapped);
        let restored = conflicts(&existing)?;
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0]["field"], "name");
        assert_eq!(
            restored[0]["effectiveValue"],
            name_conflict["effectiveValue"]
        );
        let old_choices: BTreeSet<_> = name_conflict["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].to_string())
            .collect();
        let new_choices: BTreeSet<_> = restored[0]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].to_string())
            .collect();
        assert_eq!(old_choices, new_choices);
        resolve(
            &mut existing,
            &json!({"kind":"collection","id":"hidden","field":DELETED,"value":false}),
        )?;
        assert_eq!(entity(&existing, "collection", "hidden").unwrap(), hidden);
        let before = existing.save();
        assert!(validate_snapshot(b"invalid snapshot").is_err());
        assert!(restore_snapshot(&mut existing, b"invalid snapshot", &remapped).is_err());
        assert_eq!(existing.save(), before);
        Ok(())
    }
}
