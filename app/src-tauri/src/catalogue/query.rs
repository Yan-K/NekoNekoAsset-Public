use super::*;
use rusqlite::types::Value as Sql;
use std::collections::{hash_map::DefaultHasher, BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

/// List facts reused until the trigger-maintained products revision changes.
/// Every list column follows the file-heavy body, so filters, sorts, search,
/// counts and list summaries read a compact per-connection TEMP copy instead of
/// each stored row; the library schema is unchanged. TEMP triggers record the
/// rows this connection changes, so a refresh rereads only those rows. Summary
/// fingerprints let a client skip summaries it already holds.
#[derive(Default)]
pub(super) struct ListSnapshot {
    revision: Option<i64>,
    total: i64,
    counts: Value,
    summaries: HashMap<String, u64>,
}

// Keep the effective values of the former summary predicates and ordering.
const LIST_COLUMNS: &str = "id,category,status,sensitive,downloaded,title,acquired,price,search,in_all,COALESCE(json_extract(summary,'$.hidden'),0),COALESCE(json_extract(summary,'$.priceCurrency'),'JPY'),COALESCE(json_extract(summary,'$.provider'),'')='local',json_extract(summary,'$.accountIds'),json_extract(summary,'$.ownedAvatarEvidence'),json_extract(summary,'$.provenance'),json_extract(summary,'$.ownedCurrentPrices'),json_extract(summary,'$.newlyDiscovered'),summary";
// The UPDATE condition matches products_changed_update, so each recorded event is one revision.
// Trigger steps inherit an upsert's conflict policy, so the change log has no key and may repeat ids.
const LIST_SCHEMA: &str = "CREATE TEMP TABLE IF NOT EXISTS list_rows(id TEXT PRIMARY KEY,category TEXT NOT NULL,status TEXT NOT NULL,sensitive INTEGER NOT NULL,downloaded INTEGER NOT NULL,title TEXT NOT NULL,acquired TEXT,price REAL,search TEXT NOT NULL,in_all INTEGER NOT NULL,hidden,currency,local INTEGER NOT NULL,accounts,evidence,provenance,prices,newly,summary TEXT NOT NULL);
 CREATE TEMP TABLE IF NOT EXISTS list_changes(id);
 CREATE TEMP TABLE IF NOT EXISTS list_events(n INTEGER NOT NULL);
 INSERT INTO temp.list_events SELECT 0 WHERE NOT EXISTS(SELECT 1 FROM temp.list_events);
 CREATE TEMP TRIGGER IF NOT EXISTS list_insert AFTER INSERT ON main.products BEGIN INSERT INTO list_changes VALUES(NEW.id); UPDATE list_events SET n=n+1; END;
 CREATE TEMP TRIGGER IF NOT EXISTS list_update AFTER UPDATE ON main.products WHEN NEW.summary<>OLD.summary OR NEW.search<>OLD.search BEGIN INSERT INTO list_changes VALUES(OLD.id),(NEW.id); UPDATE list_events SET n=n+1; END;
 CREATE TEMP TRIGGER IF NOT EXISTS list_delete AFTER DELETE ON main.products BEGIN INSERT INTO list_changes VALUES(OLD.id); UPDATE list_events SET n=n+1; END;";

fn snapshot<'a>(conn: &Connection, cache: &'a mut ListSnapshot) -> Result<&'a ListSnapshot> {
    let revision: i64 = conn.query_row(
        "SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='products_revision'),0)",
        [],
        |row| row.get(0),
    )?;
    if cache.revision == Some(revision) {
        return Ok(cache);
    }
    // An enclosing transaction could roll the copy back behind this cache.
    ensure!(conn.is_autocommit(), "catalogue.list_snapshot_transaction");
    let previous = cache.revision.take();
    conn.execute_batch(LIST_SCHEMA)?;
    conn.execute_batch("SAVEPOINT list_rows;")?;
    let filled = (|| -> Result<ListSnapshot> {
        // Reread only the recorded rows when they account for every revision since the copy.
        let events: i64 = conn.query_row("SELECT n FROM temp.list_events", [], |row| row.get(0))?;
        let partial = previous.is_some_and(|old| revision - old == events);
        let mut next = ListSnapshot {
            revision: Some(revision),
            summaries: if partial {
                std::mem::take(&mut cache.summaries)
            } else {
                HashMap::new()
            },
            ..Default::default()
        };
        let changed = if partial {
            let mut stmt = conn.prepare("SELECT DISTINCT id FROM temp.list_changes")?;
            for id in stmt.query_map([], |row| row.get::<_, String>(0))? {
                next.summaries.remove(&id?);
            }
            "WHERE id IN(SELECT id FROM temp.list_changes)"
        } else {
            ""
        };
        conn.execute(&format!("DELETE FROM temp.list_rows {changed}"), [])?;
        conn.execute(
            &format!("INSERT INTO temp.list_rows SELECT {LIST_COLUMNS} FROM products {changed}"),
            [],
        )?;
        let mut stmt = conn.prepare(&format!("SELECT id,summary FROM temp.list_rows {changed}"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut hasher = DefaultHasher::new();
            row.get_ref(1)?.as_str()?.hash(&mut hasher);
            next.summaries.insert(row.get(0)?, hasher.finish());
        }
        conn.execute_batch("DELETE FROM temp.list_changes; UPDATE temp.list_events SET n=0;")?;
        let (mut categories, mut statuses) = (BTreeMap::<String, i64>::new(), BTreeMap::<String, i64>::new());
        let (mut all, mut downloaded, mut hidden) = (0i64, 0i64, 0i64);
        let mut stmt = conn.prepare("SELECT category,status,in_all=1,downloaded=1,hidden=0,hidden=1,COUNT(*) FROM temp.list_rows GROUP BY 1,2,3,4,5,6")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let count: i64 = row.get(6)?;
            next.total += count;
            if row.get(4)? {
                *categories.entry(row.get(0)?).or_default() += count;
                *statuses.entry(row.get(1)?).or_default() += count;
                all += count * i64::from(row.get::<_, bool>(2)?);
                downloaded += count * i64::from(row.get::<_, bool>(3)?);
            }
            hidden += count * i64::from(row.get::<_, bool>(5)?);
        }
        let attention = statuses.get("attention").copied().unwrap_or(0);
        next.counts = json!({"all":all,"downloaded":downloaded,"attention":attention,"hidden":hidden,"categories":categories,"statuses":statuses});
        Ok(next)
    })();
    match filled {
        Ok(next) => {
            conn.execute_batch("RELEASE list_rows;")?;
            *cache = next;
            Ok(cache)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK TO list_rows; RELEASE list_rows;");
            Err(error)
        }
    }
}

pub(super) fn query(conn: &Connection, args: &Value, cache: &mut ListSnapshot) -> Result<Value> {
    let list = snapshot(conn, cache)?;
    let mut conditions = vec!["1=1".to_owned()];
    let hidden_only = args["hiddenOnly"].as_bool().unwrap_or(false);
    conditions.push(format!("p.hidden={}", i32::from(hidden_only)));
    let mut binds: Vec<Sql> = vec![];
    let mut bind = |value: Sql| {
        binds.push(value);
        format!("?{}", binds.len())
    };
    let category = args["category"].as_str().unwrap_or("all");
    let quick = args["quick"].as_str().unwrap_or("all");
    let collection = args["collectionId"]
        .as_str()
        .or(args["collection"].as_str())
        .unwrap_or("");
    if category != "all" && !category.is_empty() {
        conditions.push(format!(
            "p.category={}",
            bind(Sql::Text(category.to_owned()))
        ));
    } else if quick == "all" && collection.is_empty() && !hidden_only {
        conditions.push("p.in_all=1".to_owned());
    }
    if quick == "downloaded" {
        conditions.push("p.downloaded=1".to_owned());
    }
    if quick == "attention" {
        conditions.push("p.status='attention'".to_owned());
    }
    if quick == "new" {
        conditions.push("p.newly=1".to_owned());
    }
    if !collection.is_empty() {
        // Read membership once instead of reparsing the collection for each product.
        conditions.push(format!("p.id IN(SELECT m.value FROM entities c,json_each(c.body,'$.productIds') m WHERE c.kind='collection' AND c.id={})",bind(Sql::Text(collection.to_owned()))));
    }
    let sort = args["sort"].as_str().unwrap_or("acquired");
    let status = text(args, "status");
    if !status.is_empty() && status != "all" {
        conditions.push(format!("p.status={}", bind(Sql::Text(status.to_owned()))));
    }
    let sensitive = args["sensitive"]
        .as_str()
        .or(args["sensitiveMode"].as_str())
        .or(args["r18"].as_str())
        .unwrap_or("show");
    match sensitive {
        "only" => conditions.push("p.sensitive=1".into()),
        "hide" => conditions.push("p.sensitive=0".into()),
        _ => {}
    }
    let account_ids = args["accountIds"].as_array();
    let include_offline = args["includeOffline"]
        .as_bool()
        .unwrap_or(account_ids.is_none());
    let local_product = "p.local=1";
    if !include_offline {
        conditions.push(format!("NOT ({local_product})"));
    }
    let mut account_placeholders = vec![];
    if let Some(accounts) = account_ids {
        account_placeholders = accounts
            .iter()
            .filter_map(Value::as_str)
            .map(|s| bind(Sql::Text(s.to_owned())))
            .collect::<Vec<_>>();
        let account_scope = if account_placeholders.is_empty() {
            "0=1".to_owned()
        } else {
            format!(
                "EXISTS(SELECT 1 FROM json_each(p.accounts) a WHERE a.value IN({}))",
                account_placeholders.join(",")
            )
        };
        conditions.push(if include_offline {
            format!("({local_product} OR {account_scope})")
        } else {
            account_scope
        });
    }
    let avatar = text(args, "avatar");
    let mut pinned = vec![];
    if !avatar.is_empty() && avatar != "all" {
        let preferences = raw(conn, "preferences", "app")?.unwrap_or(json!({}));
        let registry = avatars::registry_from(
            &projection::bases_in(conn, "temp.list_rows")?,
            &preferences,
        )?;
        let avatar_id = registry.resolve(avatar);
        // Base pins override ordinary filters, but never the user's explicit
        // hidden/visible or R-18 visibility boundaries.
        let mut visible_base = conn.prepare_cached(
            "SELECT hidden=?2 AND (?3 NOT IN('hide','only') OR sensitive=CASE ?3 WHEN 'only' THEN 1 ELSE 0 END) FROM temp.list_rows WHERE id=?1",
        )?;
        for id in registry.bases(&avatar_id) {
            if visible_base.query_row(params![id, hidden_only, sensitive], |row| row.get::<_, bool>(0))? {
                pinned.push(id);
            }
        }
        let avatar_binds = registry
            .terms(&avatar_id)
            .into_iter()
            .map(|v| bind(Sql::Text(v)))
            .collect::<Vec<_>>()
            .join(",");
        let scope = if account_ids.is_some() {
            let account_scope = if account_placeholders.is_empty() {
                "0=1".to_owned()
            } else {
                format!("(json_extract(e.value,'$.source') IN('user','sourceTag','userTag') OR json_extract(e.value,'$.accountId') IN({}))",account_placeholders.join(","))
            };
            if include_offline {
                format!("({local_product} OR {account_scope})")
            } else {
                account_scope
            }
        } else {
            "1=1".into()
        };
        conditions.push(format!("EXISTS(SELECT 1 FROM json_each(p.evidence) e WHERE (json_extract(e.value,'$.avatarId') IN({avatar_binds}) OR (COALESCE(json_extract(e.value,'$.avatarId'),'') NOT LIKE 'booth:%' AND COALESCE(json_extract(e.value,'$.avatarId'),'') NOT LIKE 'product:%' AND COALESCE(json_extract(e.value,'$.avatarId'),'') NOT LIKE 'avatar:%' AND json_extract(e.value,'$.avatar') IN({avatar_binds}))) AND {scope})"));
    }
    let search = normalized(
        args["search"]
            .as_str()
            .or(args["query"].as_str())
            .unwrap_or(""),
    );
    for term in search.split_whitespace() {
        let chars: Vec<_> = term.chars().collect();
        let gram: String = chars.iter().take(3).collect();
        conditions.push(format!(
            "p.id IN(SELECT product_id FROM search_grams WHERE gram={})",
            bind(Sql::Text(gram))
        ));
        conditions.push(format!(
            "instr(p.search,{})>0",
            bind(Sql::Text(term.to_owned()))
        ));
    }
    let acquired_order = if account_ids.is_none() {
        "p.acquired".to_owned()
    } else {
        let account_order = if account_placeholders.is_empty() {
            "NULL".to_owned()
        } else {
            format!("(SELECT MIN(json_extract(a.value,'$.acquiredSortKey')) FROM json_each(p.provenance) a WHERE json_extract(a.value,'$.accountId') IN({}))",account_placeholders.join(","))
        };
        format!("CASE WHEN {local_product} THEN p.acquired ELSE {account_order} END")
    };
    let price_order = if account_ids.is_none() {
        "p.price".to_owned()
    } else {
        let scope = if account_placeholders.is_empty() {
            "0=1".to_owned()
        } else {
            format!("a.value IN({})", account_placeholders.join(","))
        };
        format!("CASE WHEN {local_product} THEN p.price WHEN json_type(p.prices)='array' THEN (SELECT CASE WHEN COUNT(*)=1 THEN MAX(json_extract(v.value,'$.price')) END FROM json_each(p.prices) v WHERE EXISTS(SELECT 1 FROM json_each(v.value,'$.accountIds') a WHERE {scope})) ELSE p.price END")
    };
    let order = match sort {
        "title" => "p.title",
        "price" => &price_order,
        "status" => "NULLIF(p.status,'unknown')",
        _ => &acquired_order,
    };
    let dir =
        if args["direction"]
            .as_str()
            .unwrap_or(if sort == "acquired" { "desc" } else { "asc" })
            == "desc"
        {
            "DESC"
        } else {
            "ASC"
        };
    let predicate = conditions.join(" AND ");
    // Group original currencies; values in different currencies are not comparable.
    // Existing BOOTH projections predate priceCurrency and remain JPY.
    let currency_order = if sort == "price" {
        "p.currency ASC,"
    } else {
        ""
    };
    let sql=format!("SELECT p.id,p.downloaded FROM temp.list_rows p WHERE {predicate} ORDER BY ({order} IS NULL OR {order}='') ASC,{currency_order}{order} {dir},p.id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let normal_ids = stmt
        .query_map(rusqlite::params_from_iter(binds.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The explicit avatar Base exception overrides every filter and sort. Apply
    // before count/paging so pinned records occur exactly once on every page.
    let pinned_set: BTreeSet<_> = pinned.iter().cloned().collect();
    let mut all_downloaded = normal_ids.iter().all(|(_, downloaded)| *downloaded);
    for id in &pinned {
        all_downloaded &= conn.query_row(
            "SELECT downloaded=1 FROM temp.list_rows WHERE id=?1",
            [id],
            |row| row.get::<_, bool>(0),
        )?;
    }
    let mut ids = pinned;
    ids.extend(
        normal_ids
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !pinned_set.contains(id)),
    );
    all_downloaded &= !ids.is_empty();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = if args["allSummaries"] == true
        && args["includeFiles"] == false
        && args["presentation"] == "list"
    {
        ids.len()
    } else {
        args["limit"].as_u64().unwrap_or(100).min(10000) as usize
    };
    // A list client sends the fingerprints of summaries it already holds. Only
    // new or changed summaries cross IPC; fingerprints include the account scope.
    let known = if args["includeFiles"] == false {
        args["knownSummaries"].as_object()
    } else {
        None
    };
    let scope = {
        let mut hasher = DefaultHasher::new();
        (args["accountIds"].to_string(), args["presentation"].to_string()).hash(&mut hasher);
        hasher.finish()
    };
    let mut fingerprints = Map::new();
    let mut products = vec![];
    let mut product_stmt = conn.prepare_cached(if args["includeFiles"] == false {
        "SELECT summary FROM temp.list_rows WHERE id=?1"
    } else {
        "SELECT body FROM products WHERE id=?1"
    })?;
    for id in ids.iter().skip(offset).take(limit) {
        if let Some(known) = known {
            let mut hasher = DefaultHasher::new();
            (list.summaries.get(id), scope).hash(&mut hasher);
            let fingerprint = format!("{:016x}", hasher.finish());
            if known.get(id).and_then(Value::as_str) == Some(&fingerprint) {
                continue;
            }
            fingerprints.insert(id.clone(), json!(fingerprint));
        }
        let body: String = product_stmt.query_row([id], |r| r.get(0))?;
        let mut product = serde_json::from_str::<Value>(&body)?;
        acquisition::apply_scope(&mut product, account_ids);
        // Compact transport only after native filtering and scoped financial
        // calculations. Keep the facts used by cached list/editor consumers.
        if args["includeFiles"] == false && args["presentation"] == "list" {
            if let Some(fields) = product.as_object_mut() {
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
                    fields.remove(field);
                }
                for (field, keep) in [
                    ("tagSuggestions", &["label"][..]),
                    (
                        "ownedAvatarEvidence",
                        &["avatar", "source", "accountId"][..],
                    ),
                ] {
                    if let Some(rows) = fields.get_mut(field).and_then(Value::as_array_mut) {
                        for row in rows {
                            if let Some(row) = row.as_object_mut() {
                                row.retain(|key, _| keep.contains(&key.as_str()));
                            }
                        }
                    }
                }
            }
        }
        products.push(product);
    }
    let mut result = json!({"products":products,"total":list.total,"matchedIds":ids,"matched":ids.len(),"counts":list.counts,"allDownloaded":all_downloaded});
    if known.is_some() {
        result["fingerprints"] = Value::Object(fingerprints);
    }
    Ok(result)
}

/// Visible account totals are independent of query filters and visited pages.
pub(super) fn account_counts(conn: &Connection) -> Result<Value> {
    let mut accounts = Map::new();
    let mut known = conn.prepare("SELECT id FROM entities WHERE kind='account'")?;
    for id in known.query_map([], |row| row.get::<_, String>(0))? {
        accounts.insert(id?, json!(0));
    }
    let mut statement = conn.prepare("SELECT a.value,COUNT(DISTINCT p.id) FROM products p,json_each(p.summary,'$.accountIds') a JOIN entities e ON e.kind='account' AND e.id=a.value WHERE COALESCE(json_extract(p.summary,'$.hidden'),0)=0 GROUP BY a.value")?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (account, count) = row?;
        accounts.insert(account, json!(count));
    }
    let unassigned: i64 = conn.query_row("SELECT COUNT(*) FROM products p WHERE COALESCE(json_extract(p.summary,'$.hidden'),0)=0 AND COALESCE(json_extract(p.summary,'$.custom'),0)<>1 AND NOT EXISTS(SELECT 1 FROM json_each(p.summary,'$.accountIds') a JOIN entities e ON e.kind='account' AND e.id=a.value)", [], |row| row.get(0))?;
    Ok(json!({"accounts":accounts,"unassigned":unassigned}))
}

pub(super) fn counts(conn: &Connection, cache: &mut ListSnapshot) -> Result<(i64, Value)> {
    let list = snapshot(conn, cache)?;
    Ok((list.total, list.counts.clone()))
}
