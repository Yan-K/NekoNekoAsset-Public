use super::*;

const ACCOUNT_SOURCE_KINDS: [&str; 3] = ["acquisition", "entitlement", "remote_file"];

fn ensure_account_idle(conn: &Connection, account: &str) -> Result<()> {
    let busy: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM entities AS job
            WHERE job.kind='job'
              AND json_extract(job.body,'$.state') IN ('queued','running')
              AND (
                json_extract(job.body,'$.accountId')=?1
                OR (
                  (json_extract(job.body,'$.type')='download' OR substr(json_extract(job.body,'$.type'),1,8)='storage:')
                  AND EXISTS(
                    SELECT 1 FROM entities AS source
                    WHERE source.kind IN ('acquisition','entitlement','remote_file')
                      AND json_extract(source.body,'$.accountId')=?1
                      AND source.product_id<>''
                      AND (source.product_id=job.product_id OR EXISTS(
                        SELECT 1 FROM json_each(job.body,'$.productIds') WHERE value=source.product_id
                      ))
                  )
                )
              )
        )",
        [account],
        |row| row.get(0),
    )?;
    ensure!(!busy, "ACCOUNT_BUSY");
    Ok(())
}

fn account_sources(conn: &Connection, account: &str) -> Result<Vec<(String, String, Value)>> {
    let mut statement = conn.prepare(
        "SELECT kind,id,body FROM entities WHERE kind IN ('acquisition','entitlement','remote_file') AND json_extract(body,'$.accountId')=?1 ORDER BY kind,id",
    )?;
    let records = statement
        .query_map([account], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records
        .into_iter()
        .map(|(kind, id, body)| Ok((kind, id, serde_json::from_str(&body)?)))
        .collect()
}

impl Catalogue {
    /// Provider preflight before clearing its device session. Removal repeats
    /// this same check inside its record transaction.
    pub fn require_account_idle(&self, id: &str) -> Result<()> {
        ensure!(!id.is_empty(), "catalogue.invalid_id");
        let conn = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("catalogue.lock"))?;
        ensure_account_idle(&conn, id)
    }

    /// The caller owns the provider's idle/session guard. This changes records
    /// only: downloaded files and retained products' user overlays remain.
    pub fn remove_account(&self, id: &str, remove_products: bool) -> Result<Value> {
        ensure!(!id.is_empty(), "catalogue.invalid_id");
        self.mutate(|tx, projections| {
            if raw(tx, "account", id)?.is_none() {
                return Ok(json!({"accountRemoved":0,"productRemoved":0,"retainedShared":0}));
            }
            ensure_account_idle(tx, id)?;
            let mut removed = BTreeSet::new();
            let mut retained_shared = 0;
            if remove_products {
                let sources = account_sources(tx, id)?;
                let candidates: BTreeSet<_> = sources.iter().filter_map(|(_, _, value)| {
                    value["productId"].as_str().filter(|id| !id.is_empty()).map(str::to_owned)
                }).collect();
                let removed_files: BTreeSet<_> = sources.iter().filter(|(kind, _, _)| kind == "remote_file").map(|(_, id, _)| id.clone()).collect();
                for product_id in &candidates {
                    let Some(product) = raw(tx, "product", &product_id)? else { continue };
                    let mut shared = product["provider"] == "local" || product["custom"] == true;
                    for kind in ACCOUNT_SOURCE_KINDS {
                        shared |= related(tx, kind, &product_id)?.iter().any(|record| {
                            // Accountless/manual source facts must not be lost either.
                            record["accountId"].as_str() != Some(id)
                        });
                    }
                    if shared {
                        retained_shared += 1;
                    } else {
                        removed.insert(product_id.clone());
                    }
                }
                // Remove the source first so child removals cannot repeatedly
                // reconstruct a product that is being deleted.
                for product_id in &removed {
                    Self::write(tx, projections, "product", product_id, None)?;
                    Self::write(tx, projections, "overlay", product_id, None)?;
                    for kind in ["artifact", "download_intent"] {
                        for record in related(tx, kind, product_id)? {
                            Self::write(tx, projections, kind, text(&record, "id"), None)?;
                        }
                    }
                    // Download intents use the product ID as their entity ID.
                    Self::write(tx, projections, "download_intent", product_id, None)?;
                }
                if !removed.is_empty() {
                    for mut collection in rows(tx, "collection")? {
                        let members = strings(&collection["productIds"]);
                        let retained: Vec<_> = members.iter().filter(|id| !removed.contains(*id)).cloned().collect();
                        if retained.len() != members.len() {
                            collection["productIds"] = json!(retained);
                            let collection_id = text(&collection, "id").to_owned();
                            Self::write(tx, projections, "collection", &collection_id, Some(collection))?;
                        }
                    }
                }
                for product_id in candidates.difference(&removed) {
                    for artifact in related(tx, "artifact", product_id)? {
                        if removed_files.contains(text(&artifact, "fileId")) {
                            Self::write(tx, projections, "artifact", text(&artifact, "id"), None)?;
                        }
                    }
                }
                for (kind, source_id, _) in sources {
                    Self::write(tx, projections, &kind, &source_id, None)?;
                }
                // Surviving descriptors retain their device artifacts. Removed
                // descriptor facts must not leave orphan backup references;
                // their physical bytes are still untouched.
            }
            Self::write(tx, projections, "account", id, None)?;
            Ok(json!({"accountRemoved":1,"productRemoved":removed.len(),"retainedShared":retained_shared}))
        })
    }

    /// Failed first login may discard a draft account, never an established one.
    /// Terminal job history is deliberately retained to display the failure.
    pub fn discard_unverified_account(&self, id: &str) -> Result<bool> {
        ensure!(!id.is_empty(), "catalogue.invalid_id");
        self.mutate(|tx, projections| {
            let Some(account) = raw(tx, "account", id)? else { return Ok(false) };
            let authenticated = &account["authenticatedAt"];
            if account["identityVerified"] == true
                || (!authenticated.is_null() && authenticated.as_str() != Some(""))
                || !text(&account, "identityHash").is_empty()
            {
                return Ok(false);
            }
            let has_history: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM entities WHERE kind IN ('acquisition','entitlement','remote_file') AND json_extract(body,'$.accountId')=?1)",
                [id], |row| row.get(0),
            )?;
            if has_history { return Ok(false) }
            let active_job: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM entities WHERE kind='job' AND json_extract(body,'$.accountId')=?1 AND json_extract(body,'$.state') IN ('queued','running'))",
                [id], |row| row.get(0),
            )?;
            if active_job { return Ok(false) }
            Self::write(tx, projections, "account", id, None)?;
            Ok(true)
        })
    }
}
