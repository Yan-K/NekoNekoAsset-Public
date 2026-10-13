//! Canonical avatar identities, independent from entitlement and checkbox state.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Registry {
    pub entries: BTreeMap<String, Value>,
    aliases: BTreeMap<String, String>,
    alias_keys: BTreeMap<String, String>,
}
impl Registry {
    pub fn resolve(&self, value: &str) -> String {
        if self.entries.contains_key(value) {
            return value.to_owned();
        }
        let direct = normalized(value);
        if let Some(id) = self.aliases.get(&direct) {
            return id.clone();
        }
        let key = self
            .alias_keys
            .get(value)
            .cloned()
            .unwrap_or_else(|| normalized(&crate::taxonomy::clean_avatar_name(value)));
        self.aliases
            .get(&key)
            .cloned()
            .unwrap_or_else(|| crate::taxonomy::canonical_tag(value))
    }
    pub fn terms(&self, id: &str) -> BTreeSet<String> {
        let mut terms = BTreeSet::from([id.to_owned()]);
        let identity = |value: &str| {
            ["booth:", "product:", "avatar:"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        };
        if let Some(entry) = self.entries.get(id) {
            // Exact product identity and explicitly linked Base products are
            // valid identity spellings; a coincident name is not.
            for product in strings(&entry["baseProductIds"])
                .into_iter()
                .chain(entry["productId"].as_str().map(str::to_owned))
            {
                terms.insert(format!("product:{product}"));
            }
        }
        for (alias, target) in &self.aliases {
            if target == id {
                if !identity(alias) {
                    terms.insert(alias.clone());
                }
                let canonical = crate::taxonomy::canonical_tag(alias);
                if !identity(&canonical) || canonical == id {
                    terms.insert(canonical);
                }
            }
        }
        terms
    }
    pub fn bases(&self, id: &str) -> Vec<String> {
        self.entries
            .get(id)
            .map(|v| strings(&v["baseProductIds"]))
            .unwrap_or_default()
    }
}
fn names(entry: &Value) -> Vec<String> {
    strings(&entry["aliases"])
        .into_iter()
        .chain(
            ["id", "officialName", "englishName", "displayName"]
                .into_iter()
                .filter_map(|k| entry[k].as_str().map(str::to_owned)),
        )
        .filter(|v| !v.is_empty())
        .collect()
}
fn add_alias(registry: &mut Registry, alias: &str, id: &str, overwrite: bool) {
    let key = registry
        .alias_keys
        .entry(alias.to_owned())
        .or_insert_with(|| normalized(&crate::taxonomy::clean_avatar_name(alias)))
        .clone();
    if !key.is_empty() {
        if overwrite {
            registry.aliases.insert(key, id.to_owned());
        } else {
            registry.aliases.entry(key).or_insert_with(|| id.to_owned());
        }
    }
}
fn decorate(entry: &mut Value, settings: &Value) {
    if let Some(name) = settings["originalName"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
    {
        entry["officialName"] = json!(name.trim());
    }
    if let Some(name) = settings["englishName"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
    {
        entry["englishName"] = json!(name.trim());
        entry["englishNameStatus"] = json!("user");
    }
    entry["displayName"] = json!(crate::taxonomy::avatar_display_name(entry));
    entry["labelStatus"] = entry["englishNameStatus"].clone();
    let aliases: BTreeSet<_> = names(entry)
        .into_iter()
        .chain(strings(&settings["aliases"]))
        .collect();
    entry["aliases"] = json!(aliases);
    if let Some(enabled) = settings["enabled"].as_bool() {
        entry["explicitEnabled"] = json!(enabled);
    }
}
pub(super) fn registry(conn: &Connection, preferences: &Value) -> Result<Registry> {
    registry_from_bases(&projection::bases(conn)?, preferences, None)
}
pub(super) fn tag_resolver(conn: &Connection) -> Result<crate::taxonomy::AvatarTagResolver> {
    let bases = projection::bases(conn)?;
    let before = raw(conn, "preferences", "app")?.unwrap_or(json!({"id":"app"}));
    let mut preferences = before.clone();
    seed_source_aliases(&bases, &mut preferences);
    if preferences != before {
        set_raw(conn, "preferences", "app", &preferences)?;
        Catalogue::bump_revision(conn)?;
    }
    let options: Vec<_> = registry_from_bases(&bases, &preferences, None)?
        .entries
        .into_values()
        .collect();
    Ok(crate::taxonomy::AvatarTagResolver::from_options(&options))
}
/// Historical names derive solely from source metadata of this exact Base.
/// Never harvest a decorated registry entry, user settings or product nickname.
pub(super) fn source_aliases(product: &Value) -> BTreeSet<String> {
    let title = text(product, "sourceTitle");
    if text(product, "category") != "Base"
        || text(product, "id").is_empty()
        || !crate::taxonomy::avatar_label_is_unambiguous(title)
    {
        return BTreeSet::new();
    }
    let source = json!({"id":product["id"],"category":"Base","sourceTitle":title,"title":title});
    let mut names = BTreeSet::from([title.to_owned()]);
    for entry in crate::taxonomy::owned_base_options(&[source]) {
        for field in ["officialName", "englishName", "displayName"] {
            if let Some(name) = entry[field].as_str().filter(|name| !name.is_empty()) {
                names.insert(name.to_owned());
            }
        }
    }
    names
}
fn merge_source_aliases(
    preferences: &mut Value,
    observations: &BTreeMap<String, BTreeSet<String>>,
) {
    let mut history = preferences["avatarSourceAliases"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (product, names) in observations {
        if names.is_empty() {
            continue;
        }
        let mut combined: BTreeSet<_> =
            strings(&history.get(product).cloned().unwrap_or(Value::Null))
                .into_iter()
                .collect();
        combined.extend(names.iter().cloned());
        history.insert(product.clone(), json!(combined));
    }
    preferences["avatarSourceAliases"] = Value::Object(history);
}
fn seed_source_aliases(bases: &[Value], preferences: &mut Value) {
    let mut observations = BTreeMap::new();
    for base in bases {
        let id = text(base, "id");
        if strings(&preferences["avatarSourceAliases"][id])
            .iter()
            .any(|name| name == text(base, "sourceTitle"))
        {
            continue;
        }
        let aliases = source_aliases(base);
        if !aliases.is_empty() {
            observations.insert(id.to_owned(), aliases);
        }
    }
    merge_source_aliases(preferences, &observations);
}
pub(super) fn remember_source_aliases(
    conn: &Connection,
    observations: &BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    if observations.is_empty() {
        return Ok(());
    }
    let before = raw(conn, "preferences", "app")?.unwrap_or(json!({"id":"app"}));
    let mut after = before.clone();
    merge_source_aliases(&mut after, observations);
    if after != before {
        set_raw(conn, "preferences", "app", &after)?;
        Catalogue::bump_revision(conn)?;
    }
    Ok(())
}
fn registry_from_bases(
    bases: &[Value],
    preferences: &Value,
    prepared_options: Option<&[Value]>,
) -> Result<Registry> {
    let dictionary = crate::taxonomy::dictionary();
    let mut registry = Registry {
        entries: BTreeMap::new(),
        aliases: BTreeMap::new(),
        alias_keys: BTreeMap::new(),
    };
    for entry in dictionary["avatars"].as_array().into_iter().flatten() {
        let mut entry = entry.clone();
        let id = text(&entry, "id").to_owned();
        // A newly curated identity replaces the previous exact-product option
        // without losing its explicitly saved enable/name preferences.
        let mut aliases = strings(&entry["aliases"]);
        if !text(&entry, "productId").is_empty() {
            aliases.push(format!("product:{}", text(&entry, "productId")));
        }
        entry["aliases"] = json!(aliases);
        entry["baseProductIds"] = json!([]);
        entry["shopName"] = entry["creator"].clone();
        entry["shopId"] = json!(url::Url::parse(text(&entry, "sourceUrl"))
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| text(&entry, "creator").to_owned()));
        registry.entries.insert(id, entry);
    }
    // A saved identity name is stronger evidence than an ambiguous source
    // title. Use it only for option construction; retain the original source
    // records below for aliases, shops and exact Base product identity.
    let mut named_bases = bases.to_vec();
    for base in &mut named_bases {
        let identity = format!("product:{}", text(base, "id"));
        let human_name = text(&preferences["avatarSettings"][&identity], "originalName");
        let source_name = base["sourceTitle"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .or(base["title"].as_str())
            .unwrap_or("");
        if !crate::taxonomy::avatar_label_is_unambiguous(source_name)
            && crate::taxonomy::avatar_label_is_unambiguous(human_name)
        {
            base["sourceTitle"] = json!(human_name);
        }
    }
    let bases_by_id: BTreeMap<_, _> = bases.iter().map(|base| (text(base, "id"), base)).collect();
    let owned = if named_bases == bases {
        prepared_options
            .map(<[Value]>::to_vec)
            .unwrap_or_else(|| crate::taxonomy::owned_base_options(&named_bases))
    } else {
        crate::taxonomy::owned_base_options(&named_bases)
    };
    for mut entry in owned {
        let original_id = text(&entry, "id").to_owned();
        let product_id = text(&entry, "productId").to_owned();
        let base = bases_by_id[product_id.as_str()];
        // An explicit human compatibility correction can link another Base to
        // one known identity. Ambiguous/multiple links and name-only matches do
        // not merge Base identities or shops.
        let manual: BTreeSet<_> = base["ownedAvatarEvidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| text(e, "source") == "user")
            .map(|e| crate::taxonomy::canonical_tag(text(e, "avatar")))
            .filter(|id| registry.entries.contains_key(id) && id.starts_with("booth:"))
            .collect();
        let id = if original_id.starts_with("product:") && manual.len() == 1 {
            manual.into_iter().next().unwrap()
        } else {
            original_id.clone()
        };
        if let Some(known) = registry.entries.get(&id) {
            entry = known.clone();
        }
        let mut base_ids: BTreeSet<_> = strings(&entry["baseProductIds"]).into_iter().collect();
        base_ids.insert(product_id);
        entry["baseProductIds"] = json!(base_ids);
        if text(&entry, "shopName").is_empty() && !text(base, "shop").is_empty() {
            entry["shopName"] = base["shop"].clone();
        }
        if text(&entry, "shopId").is_empty() {
            if let Some(shop) = base["sourceShop"]["url"]
                .as_str()
                .and_then(|s| url::Url::parse(s).ok())
                .and_then(|u| u.host_str().map(str::to_owned))
            {
                entry["shopId"] = json!(shop);
            }
        }
        if text(&entry, "shopId").is_empty() {
            entry["shopId"] = json!(text(&entry, "shopName"));
        }
        let mut aliases = names(&entry);
        aliases.push(original_id);
        aliases.extend([
            text(base, "title").to_owned(),
            text(base, "sourceTitle").to_owned(),
        ]);
        entry["aliases"] = json!(aliases);
        registry.entries.insert(id, entry);
    }
    // Source history belongs to exact currently active Base products. It must
    // not resurrect deleted Base identities or removed human settings aliases.
    for entry in registry.entries.values_mut() {
        let mut aliases: BTreeSet<_> = strings(&entry["aliases"]).into_iter().collect();
        for product in strings(&entry["baseProductIds"]) {
            aliases.extend(strings(&preferences["avatarSourceAliases"][&product]));
        }
        entry["aliases"] = json!(aliases);
    }
    // Persisted aliases survive future source renames. Official aliases take
    // priority over ambiguous inferred Base labels; explicit human aliases win.
    let entries: Vec<_> = registry.entries.values().cloned().collect();
    for entry in entries
        .iter()
        .filter(|e| text(e, "id").starts_with("booth:"))
    {
        for name in names(entry) {
            add_alias(&mut registry, &name, text(entry, "id"), false);
        }
    }
    for entry in &entries {
        for name in names(entry) {
            add_alias(&mut registry, &name, text(entry, "id"), false);
        }
    }
    for (alias, id) in preferences["avatarAliases"]
        .as_object()
        .into_iter()
        .flatten()
    {
        if let Some(id) = id.as_str().filter(|id| {
            id.starts_with("booth:") || id.starts_with("product:") || id.starts_with("avatar:")
        }) {
            add_alias(&mut registry, alias, id, false);
        }
    }
    for (id, settings) in preferences["avatarSettings"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let canonical = registry.resolve(id);
        let display = if let Some(entry) = registry.entries.get_mut(&canonical) {
            decorate(entry, settings);
            Some(text(entry, "displayName").to_owned())
        } else {
            None
        };
        if let Some(display) = display {
            add_alias(&mut registry, &display, &canonical, true);
        }
        for name in strings(&settings["aliases"]).into_iter().chain(
            ["originalName", "englishName"]
                .into_iter()
                .filter_map(|k| settings[k].as_str().map(str::to_owned)),
        ) {
            add_alias(&mut registry, &name, &canonical, true);
        }
    }
    for entry in registry.entries.values_mut() {
        let id = text(entry, "id").to_owned();
        decorate(entry, &preferences["avatarSettings"][&id]);
    }
    Ok(registry)
}
pub(super) fn normalize_preferences(conn: &Connection, preferences: &mut Value) -> Result<()> {
    let registry = registry(conn, preferences)?;
    normalize_with_registry(&registry, preferences);
    Ok(())
}
fn normalize_with_registry(registry: &Registry, preferences: &mut Value) {
    let mut settings = Map::new();
    // Settings are written with canonical identity keys by the current UI.
    for (key, value) in preferences["avatarSettings"]
        .as_object()
        .into_iter()
        .flatten()
    {
        if !value.is_object() {
            continue;
        }
        settings.insert(key.clone(), value.clone());
    }
    preferences["avatarSettings"] = Value::Object(settings);
    let mut aliases = preferences["avatarAliases"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (alias, id) in &registry.aliases {
        aliases.insert(alias.clone(), json!(id));
    }
    preferences["avatarAliases"] = Value::Object(aliases);
}
pub(super) fn options(
    conn: &Connection,
    bases: &[Value],
    prepared_options: &[Value],
) -> Result<Vec<Value>> {
    let before = raw(conn, "preferences", "app")?.unwrap_or(json!({"id":"app"}));
    let mut prefs = before.clone();
    seed_source_aliases(bases, &mut prefs);
    let mut registry = registry_from_bases(bases, &prefs, Some(prepared_options))?;
    normalize_with_registry(&registry, &mut prefs);
    let mut memberships = BTreeMap::<String, BTreeSet<String>>::new();
    for (id, entry) in &registry.entries {
        memberships
            .entry(id.clone())
            .or_default()
            .extend(strings(&entry["baseProductIds"]));
    }
    let mut stmt=conn.prepare("SELECT id,json_extract(summary,'$.ownedAvatarEvidence') FROM products WHERE json_array_length(summary,'$.ownedAvatarEvidence')>0")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (product, evidence) = row?;
        let evidence: Value = serde_json::from_str(&evidence)?;
        for item in evidence.as_array().into_iter().flatten() {
            let name = text(item, "avatar");
            let key = if text(item, "avatarId").is_empty() {
                name
            } else {
                text(item, "avatarId")
            };
            let mut id = registry.resolve(key);
            let (original, spelling) =
                if !registry.entries.contains_key(&id) || id.starts_with("avatar:") {
                    crate::taxonomy::avatar_label_parts(name)
                } else {
                    (String::new(), None)
                };
            if !registry.entries.contains_key(&id)
                && !crate::taxonomy::avatar_label_is_unambiguous(name)
            {
                // Retain ambiguous source/overlay text; it is not evidence of
                // one named identity. Explicit saved identity names may resolve it.
                let human_name = text(&prefs["avatarSettings"][&id], "originalName");
                if !crate::taxonomy::avatar_label_is_unambiguous(human_name) {
                    continue;
                }
            }
            if !registry.entries.contains_key(&id) && !id.starts_with("avatar:") {
                id = format!(
                    "avatar:{:x}",
                    Sha256::digest(normalized(&original).as_bytes())
                );
            }
            if !registry.entries.contains_key(&id) {
                let english = spelling
                    .clone()
                    .unwrap_or_else(|| crate::taxonomy::romanized_name(&original));
                let status = if english.is_empty() {
                    "unresolvedReading"
                } else if spelling.is_some() {
                    "sourceTitleSpelling"
                } else {
                    "romanizationFallback"
                };
                let mut entry = json!({"id":id,"officialName":original,"englishName":english,"englishNameStatus":status,"shopName":"","shopId":"","aliases":[name,key],"baseProductIds":[]});
                decorate(&mut entry, &prefs["avatarSettings"][&id]);
                let display = text(&entry, "displayName").to_owned();
                registry.entries.insert(id.clone(), entry);
                add_alias(&mut registry, &display, &id, true);
            }
            if id.starts_with("avatar:") {
                let entry = registry.entries.get_mut(&id).unwrap();
                if matches!(
                    text(entry, "englishNameStatus"),
                    "romanizationFallback" | "unresolvedReading"
                ) {
                    if let Some(english) = &spelling {
                        entry["englishName"] = json!(english);
                        entry["englishNameStatus"] = json!("sourceTitleSpelling");
                        decorate(entry, &prefs["avatarSettings"][&id]);
                    }
                }
                let aliases = names(entry);
                for alias in aliases {
                    add_alias(&mut registry, &alias, &id, false);
                }
            }
            add_alias(&mut registry, name, &id, true);
            add_alias(&mut registry, key, &id, true);
            memberships.entry(id).or_default().insert(product.clone());
        }
    }
    for (alias, id) in &registry.aliases {
        prefs["avatarAliases"][alias] = json!(id);
    }
    if prefs != before {
        set_raw(conn, "preferences", "app", &prefs)?;
        Catalogue::bump_revision(conn)?;
    }
    for (id, entry) in &mut registry.entries {
        let count = memberships.get(id).map_or(0, BTreeSet::len);
        entry["matchingProductCount"] = json!(count);
        entry["enabled"] = json!(entry["explicitEnabled"]
            .as_bool()
            .unwrap_or(prefs["avatarAutomaticEnabled"][id].as_bool().unwrap_or(false)));
    }
    Ok(registry.entries.into_values().collect())
}

impl Catalogue {
    /// Update automatic defaults at source-sync closure, independently of human overrides.
    /// Catalogue reads only consume this snapshot and never toggle an avatar mid-sync.
    pub fn refresh_avatar_defaults(&self) -> Result<()> {
        self.mutate(|tx, projections| {
            if rows(tx, "job")?.iter().any(|job| {
                matches!(text(job, "state"), "queued" | "running")
                    && matches!(text(job, "type"), "boothSync" | "boothUpdate" | "gumroadSync" | "gumroadUpdate" | "jinxxySync" | "jinxxyUpdate")
            }) {
                return Ok(());
            }
            let bases = projection::bases(tx)?;
            let owned = crate::taxonomy::owned_base_options(&bases);
            let entries = options(tx, &bases, &owned)?;
            let mut automatic = Map::new();
            for entry in entries {
                let count = entry["matchingProductCount"].as_u64().unwrap_or(0);
                if count > 5 && !strings(&entry["baseProductIds"]).is_empty() {
                    automatic.insert(text(&entry, "id").to_owned(), json!(true));
                }
            }
            let mut preferences = raw(tx, "preferences", "app")?.unwrap_or(json!({"id":"app"}));
            preferences["avatarAutomaticEnabled"] = Value::Object(automatic);
            Self::write(tx, projections, "preferences", "app", Some(preferences))
        })
    }
}
