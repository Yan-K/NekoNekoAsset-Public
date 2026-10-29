use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct ProjectionChanges {
    /// Product IDs whose exact tag inputs need the transaction's resolver.
    pub tags: BTreeSet<String>,
    pub identities_changed: bool,
    /// Source-only names observed before and after a Base identity changes.
    pub source_aliases: BTreeMap<String, BTreeSet<String>>,
    /// Optional transaction-local snapshot already needed to interpret tag edits.
    pub prepared_resolver: Option<crate::taxonomy::AvatarTagResolver>,
    /// Batch-import membership is the only job relation not indexed by product.
    /// Read it once per mutation, rather than scanning all history per product.
    pub local_import_jobs: Option<Vec<Value>>,
}
impl ProjectionChanges {
    fn prepare_jobs(&mut self, conn: &Connection) -> Result<()> {
        if self.local_import_jobs.is_none() {
            self.local_import_jobs = Some(local_import_jobs(conn)?);
        }
        Ok(())
    }

    pub fn refresh(&mut self, conn: &Connection, id: &str) -> Result<()> {
        self.prepare_jobs(conn)?;
        let changes = refresh_with_jobs(conn, id, self.local_import_jobs.as_deref().unwrap())?;
        self.merge(changes);
        Ok(())
    }

    pub fn refresh_local(
        &mut self,
        conn: &Connection,
        id: &str,
        files: &BTreeSet<String>,
        roots_changed: bool,
    ) -> Result<()> {
        self.prepare_jobs(conn)?;
        refresh_local(
            conn,
            id,
            files,
            roots_changed,
            self.local_import_jobs.as_deref().unwrap(),
        )
    }

    pub fn merge(&mut self, other: Self) {
        self.tags.extend(other.tags);
        self.identities_changed |= other.identities_changed;
        for (id, aliases) in other.source_aliases {
            self.source_aliases.entry(id).or_default().extend(aliases);
        }
    }

    fn record_source_aliases(&mut self, product: &Value) {
        if text(product, "category") != "Base" {
            return;
        }
        let aliases = avatars::source_aliases(product);
        if !aliases.is_empty() {
            self.source_aliases
                .entry(text(product, "id").to_owned())
                .or_default()
                .extend(aliases);
        }
    }
}

fn identity_inputs(product: &Value) -> Value {
    if text(product, "category") != "Base" {
        return Value::Null;
    }
    let manual: Vec<_> = product["ownedAvatarEvidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| text(e, "source") == "user")
        .collect();
    json!({"id":product["id"],"category":product["category"],
        "title":product["title"],"titleOverride":product["titleOverride"] == true || product["titleOverride"] == 1,
        "sourceTitle":product["sourceTitle"],"shop":product["shop"],
        "sourceShop":product["sourceShop"],"manual":manual})
}

fn compatibility(
    product: &mut Value,
    resolve: impl Fn(&str) -> Option<Value>,
    canonical: impl Fn(&str) -> String,
    resolve_manual: impl Fn(&str) -> Option<Value>,
) {
    let inputs = product["_tagInputs"].clone();
    let rejected: BTreeSet<_> = strings(&inputs["rejectedTags"])
        .iter()
        .map(|t| canonical(t))
        .collect();
    let mut evidence: Vec<_> = product["ownedAvatarEvidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| !matches!(text(e, "source"), "sourceTag" | "userTag" | "user"))
        .cloned()
        .collect();
    for (tags, basis) in [
        (strings(&inputs["sourceTags"]), "sourceTag"),
        (strings(&inputs["tags"]), "userTag"),
    ] {
        for tag in tags {
            if rejected.contains(&canonical(&tag)) {
                continue;
            }
            if let Some(avatar) = resolve(&tag) {
                evidence.push(json!({"avatar":avatar["displayName"].as_str().or(avatar["officialName"].as_str()).unwrap_or(""),
                    "avatarId":avatar["id"],"source":basis,"tag":tag,"entitlementEstablished":false}));
            }
        }
    }
    if inputs.get("ownedAvatars").is_some() {
        evidence = strings(&inputs["ownedAvatars"])
            .into_iter()
            .map(|name| {
                if let Some(avatar) = resolve_manual(&name) {
                    let display = if name == text(&avatar, "id") {
                        avatar["displayName"].as_str().or(avatar["officialName"].as_str()).unwrap_or(&name)
                    } else {
                        &name
                    };
                    json!({"avatarId":avatar["id"],"avatar":display,"source":"user"})
                } else {
                    json!({"avatarId":crate::taxonomy::canonical_tag(&name),"avatar":name,"source":"user"})
                }
            })
            .collect();
    }
    set_evidence(product, evidence, &canonical);
    let offered: BTreeSet<_> = strings(&product["listingAvatars"])
        .iter()
        .map(|t| canonical(t))
        .collect();
    let owned: BTreeSet<_> = strings(&product["ownedAvatars"])
        .iter()
        .map(|t| canonical(t))
        .collect();
    let manual = inputs.get("tags").is_some();
    let mut tags = strings(if manual {
        &inputs["tags"]
    } else {
        &inputs["sourceTags"]
    });
    if !manual {
        for tag in &mut tags {
            if let Some(alias) = inputs["organizationAliases"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|alias| canonical(text(alias, "from")) == canonical(tag))
            {
                if !rejected.contains(&canonical(tag)) {
                    *tag = text(alias, "to").to_owned();
                }
            }
        }
    }
    let ai_tags: BTreeSet<_> = strings(&inputs["aiTags"])
        .iter()
        .map(|tag| canonical(tag))
        .collect();
    if !manual {
        tags.extend(strings(&inputs["aiTags"]));
    }
    // Resolver equivalence, rather than dictionary-only canonicalization, is
    // necessary for dynamic aliases and dictionary/name collisions.
    let mut seen = BTreeSet::new();
    product["tags"] = json!(tags
        .into_iter()
        .filter(|tag| {
            let key = canonical(tag);
            !tag.trim().is_empty()
                && !rejected.contains(&key)
                && (manual
                    || ai_tags.contains(&key)
                    || !offered.contains(&key)
                    || owned.contains(&key))
                && seen.insert(key)
        })
        .collect::<Vec<_>>());
}

fn set_evidence(product: &mut Value, evidence: Vec<Value>, canonical: &impl Fn(&str) -> String) {
    let mut names = BTreeMap::new();
    for entry in &evidence {
        let name = text(entry, "avatar");
        if !name.is_empty() {
            let key = if text(entry, "avatarId").is_empty() {
                canonical(name)
            } else {
                text(entry, "avatarId").to_owned()
            };
            names.entry(key).or_insert_with(|| name.to_owned());
        }
    }
    product["ownedAvatars"] = json!(names.into_values().collect::<Vec<_>>());
    product["ownedAvatarEvidence"] = json!(evidence);
}

pub(super) fn all(conn: &Connection) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare("SELECT body FROM products ORDER BY id")?;
    let data = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    data.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .collect()
}
pub(super) fn bases(conn: &Connection) -> Result<Vec<Value>> {
    let mut stmt=conn.prepare("SELECT json_object('id',id,'category',category,'title',json_extract(summary,'$.title'),'titleOverride',json_extract(summary,'$.titleOverride'),'sourceTitle',json_extract(summary,'$.sourceTitle'),'shop',json_extract(summary,'$.shop'),'sourceShop',json_extract(summary,'$.sourceShop'),'ownedAvatarEvidence',json_extract(summary,'$.ownedAvatarEvidence')) FROM products WHERE category='Base'")?;
    let data = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    data.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .collect()
}
pub(super) fn rebuild(conn: &Connection) -> Result<()> {
    conn.execute_batch("DELETE FROM products; DELETE FROM search_grams;")?;
    let jobs = local_import_jobs(conn)?;
    for value in rows(conn, "product")? {
        refresh_with_jobs(conn, text(&value, "id"), &jobs)?;
    }
    Ok(())
}
/// Refresh only stale download badges from already materialized file facts.
/// This does not alter source records, file membership, or the schema.
pub(super) fn reconcile_ready_badges(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id,body,summary FROM products WHERE downloaded=0 AND json_extract(body,'$.downloaded')>0 AND json_array_length(body,'$.files')>0")?;
    let candidates = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let tx = conn.unchecked_transaction()?;
    let local_import_jobs = local_import_jobs(&tx)?;
    for (id, body, summary) in candidates {
        let mut product: Value = serde_json::from_str(&body)?;
        let Some(files) = product["files"].as_array() else {
            continue;
        };
        if files.is_empty() || !files.iter().all(|file| text(file, "status") == "ready") {
            continue;
        }
        let jobs = product_jobs(&tx, &id, &local_import_jobs)?;
        update_status(&mut product, &jobs);
        if product["status"] != "ready" {
            continue;
        }
        let mut compact: Value = serde_json::from_str(&summary)?;
        for field in ["status", "downloaded", "fullyDownloaded"] {
            compact[field] = product[field].clone();
        }
        tx.execute(
            "UPDATE products SET body=?2,summary=?3,status='ready',downloaded=1 WHERE id=?1",
            params![
                id,
                serde_json::to_string(&product)?,
                serde_json::to_string(&compact)?
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}
fn unique(values: impl Iterator<Item = String>) -> Vec<String> {
    values.collect::<BTreeSet<_>>().into_iter().collect()
}
fn byte_complete(v: &Value) -> bool {
    matches!(
        text(v, "status"),
        "ready" | "downloaded" | "extracting" | "extractionFailed"
    )
}
fn artifact_needs_attention(artifact: &Value) -> bool {
    let status = text(artifact, "status");
    let error = text(artifact, "errorCode");
    if matches!(status, "missing" | "corrupt") {
        return true;
    }
    if status == "cancelled" || error == "cancelled" {
        return false;
    }
    matches!(
        status,
        "failed" | "needsAction" | "extractionFailed" | "attention"
    ) || (!error.is_empty()
        && !matches!(
            status,
            "ready" | "queued" | "downloading" | "extracting" | "processing"
        ))
}

fn file_job(job: &Value) -> bool {
    text(job, "type").contains("download") || text(job, "type") == "storage:import_local"
}

fn local_import_jobs(conn: &Connection) -> Result<Vec<Value>> {
    // Keep large import plans out of the transaction-local status snapshot.
    let mut statement = conn.prepare("SELECT json_object('id',id,'productId',product_id,'productIds',json_extract(body,'$.productIds'),'completedProductIds',json_extract(body,'$.completedProductIds'),'type',json_extract(body,'$.type'),'state',json_extract(body,'$.state'),'stage',json_extract(body,'$.stage'),'updatedAt',json_extract(body,'$.updatedAt'),'errorCode',json_extract(body,'$.errorCode')) FROM entities WHERE kind='job' AND json_extract(body,'$.type')='storage:import_local'")?;
    let bodies = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    bodies
        .into_iter()
        .map(|body| serde_json::from_str(&body).map_err(Into::into))
        .collect()
}

fn product_jobs(conn: &Connection, id: &str, local_import_jobs: &[Value]) -> Result<Vec<Value>> {
    let mut statement = conn.prepare("SELECT body FROM entities WHERE kind='job' AND product_id=?1 AND (json_extract(body,'$.type')<>'storage:import_local' OR NOT EXISTS(SELECT 1 FROM json_each(body,'$.completedProductIds') WHERE value=?1))")?;
    let bodies = statement
        .query_map([id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut jobs: Vec<Value> = bodies
        .into_iter()
        .map(|body| serde_json::from_str(&body).map_err(Into::into))
        .collect::<Result<_>>()?;
    for job in local_import_jobs {
        if strings(&job["productIds"])
            .iter()
            .any(|product| product == id)
            && !strings(&job["completedProductIds"])
                .iter()
                .any(|product| product == id)
            && !jobs.iter().any(|direct| direct["id"] == job["id"])
        {
            jobs.push(job.clone());
        }
    }
    Ok(jobs)
}

pub(super) fn job_changes_product(before: Option<&Value>, after: Option<&Value>) -> bool {
    if !before.is_some_and(file_job) && !after.is_some_and(file_job) {
        return false;
    }
    match (before, after) {
        (Some(before), Some(after))
            if [
                "productId",
                "productIds",
                "completedProductIds",
                "type",
                "state",
                "errorCode",
            ]
            .iter()
            .all(|field| before[*field] == after[*field])
                && matches!(text(after, "state"), "queued" | "running") =>
        {
            false
        }
        _ => true,
    }
}

fn update_status(product: &mut Value, jobs: &[Value]) {
    let empty = vec![];
    let files = product["files"].as_array().unwrap_or(&empty);
    let latest = jobs
        .iter()
        .filter(|job| file_job(job))
        .max_by_key(|job| text(job, "updatedAt"));
    let downloaded = files.iter().filter(|file| byte_complete(file)).count();
    let all_ready = !files.is_empty() && files.iter().all(|file| text(file, "status") == "ready");
    let status = if jobs.iter().any(|job| {
        matches!(text(job, "state"), "queued" | "running")
            && file_job(job)
            && text(job, "stage") != "authRequired"
    }) || files.iter().any(|file| {
        matches!(
            text(file, "status"),
            "queued" | "downloading" | "extracting" | "processing"
        )
    }) {
        "processing"
    } else if files.iter().any(artifact_needs_attention) {
        "attention"
    } else if all_ready {
        "ready"
    } else if latest.is_some_and(|job| {
        matches!(text(job, "state"), "failed" | "needsAction")
            && !matches!(text(job, "errorCode"), "cancelled" | "manifest_incomplete")
    }) {
        "attention"
    } else if downloaded > 0 {
        "partial"
    } else {
        "notDownloaded"
    };
    // Downloaded describes the files currently attached to this product. Store
    // discovery may still be running; a later observed file makes it Partial.
    let fully_downloaded = !files.is_empty() && downloaded == files.len();
    product["status"] = json!(status);
    product["downloaded"] = json!(downloaded);
    product["fullyDownloaded"] = json!(fully_downloaded);
}

/// Local state never changes taxonomy, filename search or compatibility evidence.
/// Keep their materialized values and update only affected file facts and badges.
fn refresh_local(
    conn: &Connection,
    id: &str,
    changed_files: &BTreeSet<String>,
    roots_changed: bool,
    local_import_jobs: &[Value],
) -> Result<()> {
    let body: Option<String> = conn
        .query_row("SELECT body FROM products WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(body) = body else {
        return Ok(());
    };
    let mut product: Value = serde_json::from_str(&body)?;
    if !changed_files.is_empty() {
        let mut artifacts = vec![];
        let mut remote = vec![];
        let mut statement = conn.prepare("SELECT body FROM entities WHERE kind='artifact' AND product_id=?1 AND json_extract(body,'$.fileId')=?2 ORDER BY id")?;
        for file in changed_files {
            for body in statement.query_map(params![id, file], |row| row.get::<_, String>(0))? {
                artifacts.push(serde_json::from_str::<Value>(&body?)?);
            }
            if let Some(file) = raw(conn, "remote_file", file)? {
                remote.push(file);
            }
        }
        let mut by_revision = BTreeMap::new();
        for artifact in &artifacts {
            let entry = by_revision
                .entry((text(artifact, "fileId"), text(artifact, "revision")))
                .or_insert(artifact);
            if text(artifact, "updatedAt") >= text(entry, "updatedAt") {
                *entry = artifact;
            }
        }
        let remote: BTreeMap<_, _> = remote.iter().map(|file| (text(file, "id"), file)).collect();
        let mut roots = BTreeMap::new();
        for root in artifacts
            .iter()
            .filter_map(|artifact| artifact["rootId"].as_str())
            .collect::<BTreeSet<_>>()
        {
            roots.insert(root, raw(conn, "root", root)?.unwrap_or(Value::Null));
        }
        for file in product["files"].as_array_mut().into_iter().flatten() {
            if !changed_files.contains(text(file, "id")) {
                continue;
            }
            let revision = text(file, "revision");
            let fid = text(file, "id");
            let source = remote.get(fid);
            let artifact = if revision.is_empty() || revision == "unknown" {
                None
            } else {
                by_revision.get(&(fid, revision)).copied()
            };
            let failed = artifact.is_some_and(artifact_needs_attention);
            let state =
                artifact
                    .map(|artifact| text(artifact, "status"))
                    .unwrap_or(if source.is_some() {
                        "notDownloaded"
                    } else {
                        "unknown"
                    });
            let status = if source
                .is_some_and(|source| source["requiresContentVerification"] == true)
                && !failed
                && !matches!(
                    state,
                    "queued" | "downloading" | "extracting" | "processing"
                ) {
                "unknown"
            } else if artifact.is_some_and(|artifact| text(artifact, "errorCode") == "cancelled") {
                match state {
                    "extractionFailed" => "downloaded",
                    "failed" | "needsAction" | "attention" | "partial" => "notDownloaded",
                    state => state,
                }
            } else {
                state
            };
            file["status"] = json!(status);
            for field in [
                "rootId",
                "relativePath",
                "originalAvailable",
                "extractedPath",
                "extractedInventory",
                "errorCode",
                "verifiedHash",
                "rootStatus",
            ] {
                file.as_object_mut().unwrap().remove(field);
            }
            file["originalAvailable"] = json!(false);
            if let Some(artifact) = artifact {
                for field in [
                    "rootId",
                    "relativePath",
                    "originalAvailable",
                    "extractedPath",
                    "extractedInventory",
                    "errorCode",
                ] {
                    if let Some(value) = artifact.get(field) {
                        file[field] = value.clone();
                    }
                }
                file["verifiedHash"] = artifact["hash"].clone();
                if let Some(root) = artifact["rootId"].as_str().and_then(|root| roots.get(root)) {
                    file["rootStatus"] = root["status"].clone();
                }
            }
        }
    }
    if roots_changed {
        let mut roots = BTreeMap::new();
        for root in product["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| file["rootId"].as_str())
            .collect::<BTreeSet<_>>()
        {
            roots.insert(
                root.to_owned(),
                raw(conn, "root", root)?.unwrap_or(Value::Null),
            );
        }
        for file in product["files"].as_array_mut().into_iter().flatten() {
            if let Some(root) = file["rootId"].as_str().and_then(|root| roots.get(root)) {
                file["rootStatus"] = root["status"].clone();
            }
        }
    }
    update_status(&mut product, &product_jobs(conn, id, local_import_jobs)?);
    let updated = serde_json::to_string(&product)?;
    if updated != body {
        conn.execute(
            "UPDATE products SET body=?1,summary=?2,status=?3,downloaded=?4 WHERE id=?5",
            params![
                updated,
                serde_json::to_string(&summary(&product))?,
                text(&product, "status"),
                product["fullyDownloaded"] == true,
                id
            ],
        )?;
    }
    Ok(())
}
fn category(source: &Value) -> Value {
    crate::taxonomy::infer_product_category(source)
}
fn source_rows(
    conn: &Connection,
    source: &Value,
    acquisitions: &[Value],
    entitlements: &[Value],
    remote: &[Value],
) -> Result<Vec<Value>> {
    let mut result = BTreeMap::new();
    for item in acquisitions
        .iter()
        .chain(entitlements)
        .chain(remote)
        .chain(std::iter::once(source))
    {
        let account_id = text(item, "accountId");
        let account = if account_id.is_empty() {
            Value::Null
        } else {
            raw(conn, "account", account_id)?.unwrap_or(Value::Null)
        };
        let raw_provider = [
            text(item, "provider"),
            text(item, "store"),
            text(&account, "provider"),
            text(&account, "store"),
            text(source, "provider"),
            text(source, "store"),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("");
        let provider = match normalized(raw_provider).as_str() {
            "booth" => "BOOTH",
            "gumroad" => "Gumroad",
            "jinxxy" => "Jinxxy",
            "local" | "local files" | "offline" => "Local Files",
            _ => raw_provider,
        };
        if provider.is_empty() {
            continue;
        }
        let name = [
            text(&account, "name"),
            text(&account, "label"),
            text(item, "accountName"),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("");
        let label = if name.is_empty() {
            provider.to_owned()
        } else {
            format!("{provider} – {name}")
        };
        result.insert((provider.to_owned(),account_id.to_owned()),json!({"provider":provider,"accountId":if account_id.is_empty(){Value::Null}else{json!(account_id)},"accountName":if name.is_empty(){Value::Null}else{json!(name)},"label":label}));
    }
    let account_providers: BTreeSet<_> = result
        .keys()
        .filter(|(_, a)| !a.is_empty())
        .map(|(p, _)| p.clone())
        .collect();
    result.retain(|(p, a), _| !a.is_empty() || !account_providers.contains(p));
    Ok(result.into_values().collect())
}
#[cfg(test)]
pub(super) fn effective(conn: &Connection, id: &str) -> Result<Option<Value>> {
    effective_with_jobs(conn, id, &local_import_jobs(conn)?)
}

fn effective_with_jobs(
    conn: &Connection,
    id: &str,
    local_import_jobs: &[Value],
) -> Result<Option<Value>> {
    let Some(source) = raw(conn, "product", id)? else {
        return Ok(None);
    };
    let overlay = raw(conn, "overlay", id)?.unwrap_or(json!({}));
    let acquisitions = related(conn, "acquisition", id)?;
    let entitlements = effective_entitlements(conn, id)?;
    let remote = related(conn, "remote_file", id)?;
    let artifacts = related(conn, "artifact", id)?;
    let mut artifact_map = BTreeMap::new();
    for artifact in &artifacts {
        let key = (text(artifact, "fileId"), text(artifact, "revision"));
        let entry = artifact_map.entry(key).or_insert(artifact);
        if text(artifact, "updatedAt") >= text(entry, "updatedAt") {
            *entry = artifact;
        }
    }
    let mut roots = BTreeMap::new();
    for root in artifacts
        .iter()
        .filter_map(|artifact| artifact["rootId"].as_str())
        .collect::<BTreeSet<_>>()
    {
        roots.insert(root, raw(conn, "root", root)?.unwrap_or(Value::Null));
    }
    let jobs = product_jobs(conn, id, local_import_jobs)?;
    let inference = category(&source);
    let mut p = source.clone();
    let provider = match normalized(if text(&source, "provider").is_empty() {
        text(&source, "store")
    } else {
        text(&source, "provider")
    })
    .as_str()
    {
        "booth" => "booth",
        "gumroad" => "gumroad",
        "jinxxy" => "jinxxy",
        "local" | "local files" => "local",
        _ => "",
    };
    if !provider.is_empty() {
        p["provider"] = json!(provider);
    }
    p["title"] = source
        .get("sourceTitle")
        .filter(|v| v.is_string())
        .or(source.get("title"))
        .or(source.get("name"))
        .cloned()
        .unwrap_or(json!(""));
    p["shop"] = source["shop"]
        .as_str()
        .or(source["shop"]["name"].as_str())
        .or(source["author"].as_str())
        .map(|v| json!(v))
        .unwrap_or(json!(""));
    p["sourceTitle"] = p["title"].clone();
    p["sourceTags"] = source.get("sourceTags").unwrap_or(&source["tags"]).clone();
    p["categoryInference"] = inference.clone();
    p["category"] = inference["category"].clone();
    let ai = overlay.get("ai").map(validate_ai_overlay).transpose()?;
    let organization = overlay
        .get("aiTagOrganization")
        .map(validate_ai_organization)
        .transpose()?;
    let mut ai_current = match &ai {
        Some(ai) => match &ai.evidence_fingerprint {
            Some(expected) => {
                ai_evidence_fingerprint(
                    &source,
                    &overlay,
                    &remote,
                    &artifacts,
                    &ai.evidence_refs,
                    &ai.content_evidence_refs,
                )? == *expected
            }
            None => {
                ai_fingerprint(&source, &overlay, &remote, &artifacts)? == ai.catalogue_fingerprint
            }
        },
        None => false,
    };
    if let Some(expected) = ai.as_ref().and_then(|ai| ai.preview_fingerprint.as_ref()) {
        ai_current &= ai_preview_fingerprint(&source, &overlay)? == *expected;
    }
    let organization_current = match &organization {
        Some(organization) => match &organization.evidence_fingerprint {
            Some(expected) => {
                ai_evidence_fingerprint(&source, &overlay, &[], &[], &["sourceTags".into()], &[])?
                    == *expected
            }
            None => {
                ai_fingerprint(&source, &overlay, &remote, &artifacts)?
                    == organization.catalogue_fingerprint
            }
        },
        None => false,
    };
    p["aiCurrent"] = json!(ai_current);
    p["aiCategoryApplied"] = json!(false);
    if ai_current && overlay.get("category").is_none() {
        if let Some(category) = ai.as_ref().and_then(|ai| ai.analysis.category.as_ref()) {
            p["category"] = json!(category.category);
            p["aiCategoryApplied"] = json!(true);
        }
    }
    for (k, v) in overlay.as_object().into_iter().flatten() {
        if k != "id" {
            p[k] = v.clone();
        }
    }
    // Visibility is a user choice, independent from source metadata and AI.
    p["hidden"] = json!(overlay["hidden"] == true);
    let dismissed: BTreeSet<_> = strings(&overlay["aiDismissedSourceUrls"])
        .into_iter()
        .collect();
    p["aiSourceUrlCandidates"] = json!(if ai_current {
        ai.as_ref().into_iter().flat_map(|ai| ai.analysis.source_url_candidates.iter().filter(|candidate| !dismissed.contains(&candidate.url)).map(|candidate| json!({
            "url":candidate.url,"confidence":candidate.confidence,"basis":candidate.basis,"evidenceRefs":candidate.evidence_refs,
            "status":if ai.observed_source_urls.contains(&candidate.url){"supported"}else{"uncertain"}
        }))).collect::<Vec<_>>()
    } else {
        vec![]
    });
    p["titleOverride"] = json!(overlay.get("title").is_some());
    p["categoryOverride"] = json!(overlay.get("category").is_some());
    p["sourceOriginUrl"] = source["sourceUrl"].clone();
    p["sourceUrlOverride"] = json!(overlay.get("sourceUrl").is_some());
    let page_availability = crate::booth::effective_public_page_availability(&source);
    p["publicPageAvailability"] = json!(page_availability);
    let source_url = text(&p, "sourceUrl");
    let overridden_url = overlay.get("sourceUrl").is_some()
        && text(&overlay, "sourceUrl") != text(&source, "sourceUrl");
    p["sourceUrlAvailable"] = json!(
        !source_url.is_empty()
            && validate_source_url(source_url).is_ok()
            && (overridden_url || page_availability != "unavailable")
    );
    p["sourcePreviewUrl"] = overlay
        .get("sourceThumbnailUrl")
        .unwrap_or(&source["previewUrl"])
        .clone();
    p["previewUrl"] = p["sourcePreviewUrl"].clone();
    if overlay["thumbnailHidden"] == true {
        p.as_object_mut().unwrap().remove("previewUrl");
    } else if let Some(path) = overlay["thumbnailPath"].as_str() {
        let db: String = conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        if let Some(dir) = Path::new(&db).parent() {
            p["previewUrl"] = json!(dir.join(path));
        }
    }
    for f in ["title", "shop", "note", "acquired", "avatar"] {
        if !p[f].is_string() {
            p[f] = json!("");
        }
    }
    if provider == "local" {
        p["price"] = Value::Null;
        p["priceCurrency"] = Value::Null;
        p["currentPriceText"] = Value::Null;
    } else if matches!(provider, "gumroad" | "jinxxy") {
        let currency = text(&source, "priceCurrency");
        let amount = acquisition::money_amount(&source["price"], currency);
        p["price"] = json!(amount);
        let formatted = source["priceFormatted"]
            .as_str()
            .filter(|value| !value.trim().is_empty());
        let source_currency = source["priceSourceCurrency"].as_str().filter(|value| {
            value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_uppercase())
        });
        p["priceCurrency"] = json!(amount
            .map(|_| currency)
            .or(source_currency.filter(|_| formatted.is_some())));
        p["currentPriceText"] = json!(amount
            .map(|amount| format!("{currency} {amount}"))
            .or_else(|| formatted.map(str::to_owned)));
    } else {
        // Read the BOOTH listing evidence; owned variant pricing is applied below.
        p["listingStartingPrice"] = json!(crate::booth::listing_starting_price(
            &source["listingPriceText"]
        )
        .or_else(|| crate::booth::listing_starting_price(&p["price"])));
        p["price"] = json!(crate::booth::parse_jpy_price(&p["price"])
            .or_else(|| crate::booth::parse_jpy_price(&source["listingPriceText"])));
        p["currentPriceText"] = json!(crate::booth::listing_price_display(&p["price"])
            .or_else(|| crate::booth::listing_price_display(&source["listingPriceText"])));
        p["priceCurrency"] = json!(p["currentPriceText"].is_string().then_some("JPY"));
    }
    if !p["sensitive"].is_boolean() {
        p["sensitive"] = json!(false);
    }
    // No procedural mock art/kind is assigned to real records.
    p.as_object_mut().unwrap().remove("kind");
    let accounts = unique(
        acquisitions
            .iter()
            .chain(entitlements.iter())
            .chain(remote.iter())
            .filter_map(|v| v["accountId"].as_str())
            .map(str::to_owned),
    );
    p["accountIds"] = json!(accounts);
    p["provenance"] = json!(acquisition::normalize_provenance(&acquisitions));
    p["sources"] = json!(source_rows(
        conn,
        &source,
        &acquisitions,
        &entitlements,
        &remote
    )?);
    let source_acquired = if provider == "local" {
        String::new()
    } else {
        text(&p, "acquired").to_owned()
    };
    acquisition::apply_default(&mut p, &source_acquired);
    acquisition::owned_prices(&mut p, &source, &acquisitions, &entitlements, &remote);
    let required: BTreeSet<_> = if provider == "local" {
        strings(&source["localManifest"]["fileIds"])
            .into_iter()
            .collect()
    } else {
        entitlements
            .iter()
            .flat_map(|e| strings(&e["fileIds"]))
            .collect()
    };
    let known_local_files: BTreeSet<_> = remote
        .iter()
        .filter(|file| file["provider"] == "local")
        .map(|file| text(file, "id"))
        .collect();
    let known_file_ids: BTreeSet<_> = remote.iter().map(|file| text(file, "id")).collect();
    let manifest_complete = !required.is_empty()
        && required
            .iter()
            .all(|id| known_file_ids.contains(id.as_str()))
        && if provider == "local" {
            source["localManifest"]["complete"] == true
                && required
                    .iter()
                    .all(|id| known_local_files.contains(id.as_str()))
        } else {
            !entitlements.is_empty() && entitlements.iter().all(|e| e["manifestComplete"] == true)
        };
    p["manifestComplete"] = json!(manifest_complete);
    let mut remote_map: BTreeMap<String, Value> = remote
        .into_iter()
        .map(|v| (text(&v, "id").to_owned(), v))
        .collect();
    let file_ids: BTreeSet<_> = if provider != "local" && entitlements.is_empty() {
        remote_map.keys().cloned().collect()
    } else {
        required
    };
    let mut files = vec![];
    let mut evidence = vec![];
    let mut known_file_count = 0;
    for fid in file_ids {
        let known = remote_map.contains_key(&fid);
        known_file_count += usize::from(known);
        let f = remote_map
            .remove(&fid)
            .unwrap_or(json!({"id":fid,"productId":id}));
        let revision = text(&f, "revision");
        // Revision evidence is mandatory: a previously ready version does not satisfy a changed source file.
        let artifact = if revision.is_empty() || revision == "unknown" {
            None
        } else {
            artifact_map.get(&(fid.as_str(), revision)).copied()
        };
        let failed = artifact.is_some_and(artifact_needs_attention);
        let artifact_status = artifact.map(|a| text(a, "status")).unwrap_or(if known {
            "notDownloaded"
        } else {
            "unknown"
        });
        let status = if f["requiresContentVerification"] == true
            && !failed
            && !matches!(
                artifact_status,
                "queued" | "downloading" | "extracting" | "processing"
            ) {
            "unknown"
        } else if artifact.is_some_and(|a| text(a, "errorCode") == "cancelled") {
            match artifact_status {
                "extractionFailed" => "downloaded",
                "failed" | "needsAction" | "attention" | "partial" => "notDownloaded",
                status => status,
            }
        } else {
            artifact_status
        };
        let mut file = json!({"id":fid,"name":text(&f,"name"),"type":text(&f,"name").rsplit('.').next().unwrap_or("").to_uppercase(),"size":f["sizeBytes"].as_u64().map(|n|n.to_string()).unwrap_or_default(),"status":status,"revision":f["revision"],"provenance":[{"accountId":f["accountId"],"sourceFileId":f["sourceFileId"],"variantId":f["variantId"],"variantName":f["variantName"]}],"originalAvailable":false});
        if let Some(size) = f.get("sizeBytes") {
            file["sizeBytes"] = size.clone();
        }
        if let Some(a) = artifact {
            for key in [
                "rootId",
                "relativePath",
                "originalAvailable",
                "extractedPath",
                "extractedInventory",
                "errorCode",
            ] {
                if let Some(value) = a.get(key) {
                    file[key] = value.clone();
                }
            }
            file["verifiedHash"] = a["hash"].clone();
            if let Some(root) = a["rootId"].as_str().and_then(|id| roots.get(id)) {
                file["rootStatus"] = root["status"].clone();
            }
        }
        if known && !text(&f, "accountId").is_empty() {
            for avatar in crate::taxonomy::account_variant_avatars(
                &source,
                f["variantId"].as_str(),
                text(&f, "variantName"),
            ) {
                evidence.push(json!({"avatar":avatar["displayName"].as_str().or(avatar["officialName"].as_str()).unwrap_or(""),"avatarId":avatar["id"],"source":"accountVariant","accountId":f["accountId"],"fileId":fid}));
            }
        }
        for avatar in crate::taxonomy::match_avatars(text(&f, "name")) {
            evidence.push(json!({"avatar":avatar["displayName"].as_str().or(avatar["officialName"].as_str()).unwrap_or(""),"avatarId":avatar["id"],"source":"accountFilename","accountId":f["accountId"],"fileId":fid}));
        }
        files.push(file);
    }
    p["total"] = json!(files.len());
    p["knownFileCount"] = json!(known_file_count);
    p["files"] = json!(files);
    update_status(&mut p, &jobs);
    p["ownedAvatarEvidence"] = json!(evidence);
    let source_tags = strings(&p["sourceTags"]);
    let listing = unique(
        crate::taxonomy::listing_avatar_evidence(&source)
            .into_iter()
            .filter_map(|v| {
                v["displayName"]
                    .as_str()
                    .or(v["officialName"].as_str())
                    .map(str::to_owned)
            }),
    );
    p["listingAvatars"] = json!(listing);
    p["_tagInputs"] = json!({"sourceTags":source_tags,"listingAvatars":listing,
        "rejectedTags":strings(&p["rejectedTags"])});
    if ai_current {
        p["_tagInputs"]["aiTags"] = json!(ai
            .as_ref()
            .unwrap()
            .analysis
            .tags
            .iter()
            .map(|tag| tag.label.clone())
            .collect::<Vec<_>>());
    }
    if organization_current {
        p["_tagInputs"]["organizationAliases"] =
            serde_json::to_value(&organization.as_ref().unwrap().aliases)?;
    }
    for field in ["tags", "ownedAvatars"] {
        if let Some(value) = overlay.get(field) {
            p["_tagInputs"][field] = value.clone();
        }
    }
    compatibility(
        &mut p,
        crate::taxonomy::exact_avatar_tag,
        crate::taxonomy::canonical_tag,
        crate::taxonomy::exact_avatar_tag,
    );
    p["tagSuggestions"] = json!(crate::taxonomy::tag_suggestions(
        &format!(
            "{} {}",
            text(&p, "sourceTitle"),
            strings(&p["sourceTags"]).join(" ")
        ),
        &strings(&p["rejectedTags"])
    ));
    Ok(Some(p))
}

/// Recompute release-check components using the same read-only source and resolver.
#[cfg(test)]
pub(crate) fn verification_expected(conn: &Connection, id: &str) -> Result<Option<Value>> {
    let Some(mut product) = effective(conn, id)? else {
        return Ok(None);
    };
    let preferences = raw(conn, "preferences", "app")?.unwrap_or(json!({}));
    let registry = avatars::registry(conn, &preferences)?;
    let resolver = registry.resolver();
    compatibility(
        &mut product,
        |tag| resolver.resolve_tag(tag),
        |tag| resolver_tag_key(&resolver, tag),
        |name| resolver.resolve_manual_avatar(name),
    );
    Ok(Some(product))
}
// Keep routine comparisons and whole-catalogue reconciliation independent of
// the potentially large materialized file/provenance arrays.
const COMPACT: &str = "json_object('id',id,'category',category,'title',json_extract(body,'$.title'),'titleOverride',json_extract(body,'$.titleOverride'),'sourceTitle',json_extract(body,'$.sourceTitle'),'shop',json_extract(body,'$.shop'),'sourceShop',json_extract(body,'$.sourceShop'),'_tagInputs',json_extract(body,'$._tagInputs'),'ownedAvatarEvidence',json_extract(body,'$.ownedAvatarEvidence'),'ownedAvatars',json_extract(body,'$.ownedAvatars'),'tags',json_extract(body,'$.tags'),'listingAvatars',json_extract(body,'$.listingAvatars'))";

fn compact(conn: &Connection, id: &str) -> Result<Option<Value>> {
    let body: Option<String> = conn
        .query_row(
            &format!("SELECT {COMPACT} FROM products WHERE id=?1"),
            [id],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body).map_err(Into::into))
        .transpose()
}

pub(super) fn resolver_tag_key(resolver: &crate::taxonomy::AvatarTagResolver, tag: &str) -> String {
    let key = resolver.canonical_tag(tag);
    if key != crate::taxonomy::normalize(tag) {
        return key;
    }
    let generic = crate::taxonomy::canonical_tag(tag);
    if generic.starts_with("style:")
        || generic.starts_with("use:")
        || generic.starts_with("rating:")
    {
        generic
    } else {
        // In particular, never accept a dictionary avatar here: the global
        // resolver may have rejected that name as an identity collision.
        key
    }
}

#[cfg(test)]
pub(super) fn refresh(conn: &Connection, id: &str) -> Result<ProjectionChanges> {
    refresh_with_jobs(conn, id, &local_import_jobs(conn)?)
}

fn refresh_with_jobs(
    conn: &Connection,
    id: &str,
    local_import_jobs: &[Value],
) -> Result<ProjectionChanges> {
    let previous = compact(conn, id)?;
    let mut changes = ProjectionChanges::default();
    let Some(mut p) = effective_with_jobs(conn, id, local_import_jobs)? else {
        if let Some(previous) = previous {
            changes.identities_changed = !identity_inputs(&previous).is_null();
            if changes.identities_changed {
                changes.record_source_aliases(&previous);
            }
            changes.tags.insert(id.to_owned());
        }
        conn.execute("DELETE FROM search_grams WHERE product_id=?1", [id])?;
        conn.execute("DELETE FROM products WHERE id=?1", [id])?;
        return Ok(changes);
    };
    if previous
        .as_ref()
        .is_none_or(|old| old["_tagInputs"] != p["_tagInputs"])
    {
        changes.tags.insert(id.to_owned());
    } else if let Some(previous) = previous.as_ref() {
        // Fresh file/variant evidence replaces its old counterpart. Registry
        // tags and resolved manual choices survive unchanged-input refreshes.
        let mut evidence: Vec<Value> = p["ownedAvatarEvidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| !matches!(text(e, "source"), "sourceTag" | "userTag" | "user"))
            .cloned()
            .collect();
        evidence.extend(
            previous["ownedAvatarEvidence"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| matches!(text(e, "source"), "sourceTag" | "userTag" | "user"))
                .cloned(),
        );
        set_evidence(&mut p, evidence, &crate::taxonomy::canonical_tag);
        p["tags"] = previous["tags"].clone();
    }
    changes.identities_changed = previous
        .as_ref()
        .map(identity_inputs)
        .unwrap_or(Value::Null)
        != identity_inputs(&p);
    if changes.identities_changed {
        if let Some(previous) = previous.as_ref() {
            changes.record_source_aliases(previous);
        }
        changes.record_source_aliases(&p);
    }
    store_projection(conn, &p)?;
    Ok(changes)
}

/// Reconcile a transaction's immutable resolver snapshot without reading raw
/// files, artifacts or acquisitions. Full scans use bounded keyset batches.
pub(super) fn reconcile_tags(
    conn: &Connection,
    resolver: &crate::taxonomy::AvatarTagResolver,
    ids: Option<&BTreeSet<String>>,
) -> Result<()> {
    fn reconcile(
        conn: &Connection,
        resolver: &crate::taxonomy::AvatarTagResolver,
        mut product: Value,
    ) -> Result<()> {
        let previous = product.clone();
        compatibility(
            &mut product,
            |tag| resolver.resolve_tag(tag),
            |tag| resolver_tag_key(resolver, tag),
            |name| resolver.resolve_manual_avatar(name),
        );
        const FIELDS: [&str; 3] = ["ownedAvatars", "ownedAvatarEvidence", "tags"];
        if FIELDS
            .iter()
            .all(|field| product[*field] == previous[*field])
        {
            return Ok(());
        }
        let body: String = conn.query_row(
            "SELECT body FROM products WHERE id=?1",
            [text(&product, "id")],
            |r| r.get(0),
        )?;
        let mut full: Value = serde_json::from_str(&body)?;
        for field in FIELDS {
            full[field] = product[field].clone();
        }
        store_projection(conn, &full)
    }
    if let Some(ids) = ids {
        for id in ids {
            if let Some(product) = compact(conn, id)? {
                reconcile(conn, resolver, product)?;
            }
        }
    } else {
        let mut after = String::new();
        loop {
            let batch = {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {COMPACT} FROM products WHERE id>?1 ORDER BY id LIMIT 256"
                ))?;
                let data = stmt
                    .query_map([&after], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                data
            };
            if batch.is_empty() {
                break;
            }
            for body in batch {
                let product: Value = serde_json::from_str(&body)?;
                after = text(&product, "id").to_owned();
                reconcile(conn, resolver, product)?;
            }
        }
    }
    Ok(())
}

fn store_projection(conn: &Connection, p: &Value) -> Result<()> {
    let id = text(p, "id");
    let previous: Option<(String, String)> = conn
        .query_row(
            "SELECT body,search FROM products WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let body = serde_json::to_string(p)?;
    if previous.as_ref().is_some_and(|(old, _)| *old == body) {
        return Ok(());
    }
    let mut terms = vec![
        text(&p, "id").to_owned(),
        text(&p, "title").to_owned(),
        text(&p, "sourceTitle").to_owned(),
        text(&p, "shop").to_owned(),
        text(&p, "note").to_owned(),
    ];
    terms.extend(strings(&p["tags"]));
    terms.extend(strings(&p["ownedAvatars"]));
    for f in p["files"].as_array().into_iter().flatten() {
        terms.push(text(f, "name").to_owned());
    }
    let mut expanded = terms.clone();
    for t in terms {
        expanded.extend(crate::taxonomy::aliases(&t));
    }
    let search = normalized(&expanded.join(" "));
    let scope = text(&p["categoryInference"], "scope");
    let in_all = if p["categoryOverride"] == true || p["aiCategoryApplied"] == true {
        text(&p, "category") != "NonVRChat"
    } else {
        scope != "other" && (text(&p, "category") != "NonVRChat" || scope == "vrchat")
    };
    conn.execute("INSERT INTO products(id,body,summary,category,status,sensitive,downloaded,title,acquired,price,search,in_all) VALUES(?1,?2,?12,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(id) DO UPDATE SET body=excluded.body,summary=excluded.summary,category=excluded.category,status=excluded.status,sensitive=excluded.sensitive,downloaded=excluded.downloaded,title=excluded.title,acquired=excluded.acquired,price=excluded.price,search=excluded.search,in_all=excluded.in_all",params![id,body,text(&p,"category"),text(&p,"status"),p["sensitive"]==true,p["fullyDownloaded"]==true,normalized(text(&p,"title")),p["acquiredSortKey"].as_str(),p["price"].as_f64(),search,in_all,serde_json::to_string(&summary(p))?])?;
    if previous.as_ref().is_some_and(|(_, old)| *old == search) {
        return Ok(());
    }
    conn.execute("DELETE FROM search_grams WHERE product_id=?1", [id])?;
    let chars: Vec<_> = search.chars().collect();
    let mut grams = BTreeSet::new();
    for size in 1..=3 {
        for window in chars.windows(size) {
            if !window.iter().any(|c| c.is_whitespace()) {
                grams.insert(window.iter().collect::<String>());
            }
        }
    }
    let mut stmt =
        conn.prepare_cached("INSERT OR IGNORE INTO search_grams(gram,product_id) VALUES(?1,?2)")?;
    for gram in grams {
        stmt.execute(params![gram, id])?;
    }
    Ok(())
}

/// List projection is a deliberate DTO, not the source record minus files.
/// Compact account facts retain exact paid/date and compatibility semantics.
pub(super) fn summary(product: &Value) -> Value {
    let mut result = Map::new();
    for field in [
        "id",
        "title",
        "sourceTitle",
        "titleOverride",
        "shop",
        "sourceShop",
        "category",
        "categoryOverride",
        "aiCategoryApplied",
        "categoryInference",
        "sourceCategory",
        "tags",
        "sourceTags",
        "rejectedTags",
        "tagSuggestions",
        "avatar",
        "listingAvatars",
        "ownedAvatars",
        "status",
        "downloaded",
        "total",
        "price",
        "priceCurrency",
        "currentPriceText",
        "listingStartingPrice",
        "ownedCurrentPrices",
        "ownedPaidPrices",
        "paidPrice",
        "paidCurrency",
        "acquired",
        "acquiredSortKey",
        "note",
        "sensitive",
        "accountIds",
        "newlyDiscovered",
        "manifestComplete",
        "knownFileCount",
        "fullyDownloaded",
        "previewUrl",
        "sourcePreviewUrl",
        "thumbnailPath",
        "thumbnailHidden",
        "hidden",
        "sourceThumbnailUrl",
        "sourceUrl",
        "sourceUrlAvailable",
        "publicPageAvailability",
        "sourceOriginUrl",
        "sourceUrlOverride",
        "sources",
        "store",
        "provider",
        "custom",
        "sourceKind",
    ] {
        if let Some(value) = product.get(field) {
            result.insert(field.to_owned(), value.clone());
        }
    }
    fn compact_rows(product: &Value, field: &str, fields: &[&str]) -> Value {
        let mut seen = BTreeSet::new();
        let rows: Vec<Value> = product[field]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| {
                Value::Object(
                    fields
                        .iter()
                        .filter_map(|field| {
                            row.get(*field)
                                .map(|value| ((*field).to_owned(), value.clone()))
                        })
                        .collect(),
                )
            })
            .filter(|row| seen.insert(row.to_string()))
            .collect();
        json!(rows)
    }
    result.insert(
        "provenance".into(),
        compact_rows(
            product,
            "provenance",
            &[
                "id",
                "rowId",
                "accountId",
                "source",
                "acquiredDisplay",
                "acquiredSortKey",
                "acquiredDateText",
                "acquisitionKind",
                "paidPrice",
                "paidCurrency",
                "paidPriceBasis",
            ],
        ),
    );
    result.insert(
        "ownedAvatarEvidence".into(),
        compact_rows(
            product,
            "ownedAvatarEvidence",
            &[
                "avatar",
                "avatarId",
                "source",
                "accountId",
                "entitlementEstablished",
            ],
        ),
    );
    result.insert("files".into(), json!([]));
    result.insert("filesLoaded".into(), json!(false));
    Value::Object(result)
}
