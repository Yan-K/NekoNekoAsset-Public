//! Pure exact compatibility lookup over a caller-supplied canonical snapshot.
//! The catalogue owns snapshot lifetime, overlays and re-projection. No DB here.
use super::{names, normalize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct AvatarTagResolver {
    entries: BTreeMap<String, Value>,
    keys: BTreeMap<String, BTreeSet<String>>,
}

fn usable_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().any(char::is_alphabetic)
        && !["booth:", "product:", "avatar:"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        && !name.contains("://")
}

fn name_key(name: &str) -> String {
    normalize(&names::cleaned_title(name))
}

impl AvatarTagResolver {
    /// Supply canonical dictionary + effective owned Base entries after explicit
    /// identity links and human name/alias overrides. Distinct IDs are never
    /// merged by spelling. Duplicate entries of one ID contribute their aliases.
    pub fn from_options(options: &[Value]) -> Self {
        let mut resolver = Self::default();
        for entry in options {
            let Some(id) = entry["id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            resolver.entries.insert(id.to_owned(), entry.clone());
            let names: BTreeSet<_> = ["officialName", "englishName", "displayName"]
                .into_iter()
                .filter_map(|field| entry[field].as_str())
                .chain(
                    entry["aliases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
                .map(name_key)
                .filter(|name| usable_name(name))
                .collect();
            for name in &names {
                resolver.insert(name, id);
            }
            // Generate complete same-identity pairs, never split an input tag
            // and accept only its recognizable half. This keeps qualifiers and
            // mixed-avatar labels from turning into an automatic association.
            let simple: Vec<_> = names
                .iter()
                .filter(|name| !name.contains(['/', '(', ')', '「', '」', '『', '』', '"']))
                .collect();
            for left in &simple {
                for right in &simple {
                    for pair in [
                        format!("{left}/{right}"),
                        format!("{left} / {right}"),
                        format!("{left} {right}"),
                        format!("{left}-{right}"),
                        format!("{left} -{right}-"),
                        format!("{left} - {right}"),
                        format!("{left}({right})"),
                        format!("{left} ({right})"),
                        format!("{left}「{right}」"),
                        format!("{left} 「{right}」"),
                        format!("{left}『{right}』"),
                        format!("「{left}」 {right}"),
                    ] {
                        resolver.insert(&pair, id);
                    }
                }
            }
        }
        resolver
    }

    fn insert(&mut self, key: &str, id: &str) {
        self.keys
            .entry(key.to_owned())
            .or_default()
            .insert(id.to_owned());
    }

    fn tag_candidates<'a>(&'a self, key: &str, candidates: &mut BTreeSet<&'a String>) {
        if let Some(ids) = self.keys.get(key) {
            candidates.extend(ids.iter());
        }
        // Accept exactly one conventional honorific after a complete known
        // name. Keep its full spelling as a candidate too: an actual name
        // ending in ちゃん must not be silently assigned to another identity.
        if let Some(name) = key.strip_suffix("ちゃん").filter(|name| {
            !name.is_empty()
                && !name.ends_with("ちゃん")
                && !name.chars().next_back().is_some_and(char::is_whitespace)
        }) {
            if let Some(ids) = self.keys.get(name) {
                candidates.extend(ids.iter());
            }
        }
    }

    fn identity(&self, tag: &str) -> Option<&str> {
        let full = name_key(tag);
        if !usable_name(&full) {
            return None;
        }
        let mut candidates = BTreeSet::new();
        self.tag_candidates(&full, &mut candidates);
        for suffix in [
            "対応", "専用", "用", "對應", "对应", "向け", "전용", "대응", "용",
        ] {
            if let Some(bare) = full.strip_suffix(suffix) {
                self.tag_candidates(&name_key(bare.trim()), &mut candidates);
            }
        }
        if candidates.len() == 1 {
            candidates.into_iter().next().map(String::as_str)
        } else {
            None
        }
    }

    /// Exact compatible-avatar evidence only. Includes no purchase, entitlement,
    /// ownership or file-access inference. Ambiguous shared names return None.
    pub fn resolve_tag(&self, tag: &str) -> Option<Value> {
        let id = self.identity(tag)?;
        let mut candidate = self.entries.get(id)?.clone();
        candidate["basis"] = json!("exactTag");
        candidate["matchedAliases"] = json!([tag]);
        candidate["entitlementEstablished"] = json!(false);
        Some(candidate)
    }

    /// Explicit human choices may use a canonical ID even when names collide.
    /// Automatic tag matching continues to reject ID-shaped text.
    pub fn resolve_manual_avatar(&self, name: &str) -> Option<Value> {
        self.entries
            .get(name)
            .cloned()
            .or_else(|| self.resolve_tag(name))
    }

    /// Use for rejection equivalence within this same snapshot. A unique name
    /// returns its ID; ambiguous/unrecognized text remains plain normalized
    /// text, with no dictionary fallback that could conceal a collision.
    pub fn canonical_tag(&self, tag: &str) -> String {
        self.identity(tag)
            .map(str::to_owned)
            .unwrap_or_else(|| normalize(tag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(id: &str, original: &str, english: &str) -> Value {
        json!({"id":id,"officialName":original,"englishName":english,"displayName":format!("{original} / {english}"),"aliases":[format!("product:{id}"),"123456"]})
    }

    #[test]
    fn uncurated_owned_names_resolve_complete_exact_tags_only() {
        let resolver = AvatarTagResolver::from_options(&[base(
            "product:synthetic-cat",
            "ソラネコ",
            "Soraneko",
        )]);
        for tag in [
            "ソラネコ",
            "ＳＯＲＡＮＥＫＯ",
            "Soraneko / ソラネコ",
            "ソラネコ(Soraneko)",
            "ソラネコ -Soraneko-",
            "【ソラネコ】対応",
            "オリジナル3Dモデル「ソラネコ」",
            "Soraneko전용",
        ] {
            let candidate = resolver.resolve_tag(tag).unwrap();
            assert_eq!(candidate["id"], "product:synthetic-cat", "{tag}");
            assert_eq!(candidate["entitlementEstablished"], false);
            assert_eq!(resolver.canonical_tag(tag), "product:synthetic-cat");
        }
        for tag in [
            "ソラネコ用コート",
            "Soraneko outfit",
            "Soraneko - coat",
            "ソラネコ / Coat",
            "ソラネコ / 海ネコ",
            "Soranekoish",
            "123456",
            "product:synthetic-cat",
            "booth:123456",
            "https://example.test/Soraneko",
        ] {
            assert!(resolver.resolve_tag(tag).is_none(), "{tag}");
        }
    }

    #[test]
    fn honorific_tags_and_rejections_resolve_all_complete_known_names() {
        let mut sky = base("product:sky", "ソラネコ", "Soraneko");
        sky["aliases"] = json!(["Star Cat"]);
        let sea = base("product:sea", "ウミネコ", "Umineko");
        let resolver = AvatarTagResolver::from_options(&[sky, sea]);
        for (id, names) in [
            ("product:sky", vec!["ソラネコ", "Soraneko", "Star Cat"]),
            ("product:sea", vec!["ウミネコ", "Umineko"]),
        ] {
            let rejected = BTreeSet::from([resolver.canonical_tag(names[0])]);
            for name in names {
                for suffix in ["", "対応", "用", "専用"] {
                    let tag = format!("{name}ちゃん{suffix}");
                    let candidate = resolver.resolve_tag(&tag).unwrap();
                    assert_eq!(candidate["id"], id, "{tag}");
                    assert_eq!(candidate["basis"], "exactTag");
                    assert_eq!(candidate["entitlementEstablished"], false);
                    assert!(rejected.contains(&resolver.canonical_tag(&tag)), "{tag}");
                    assert_eq!(resolver.canonical_tag(&tag), resolver.canonical_tag(name));
                }
            }
        }
        for tag in [
            "ソラネコちゃんちゃん",
            "ソラネコちゃんちゃん対応",
            "ソラネコちゃん対応用",
            "ソラネコちゃん用コート",
            "ソラネコちゃん衣装",
            "ソラネコちゃんが好き",
            "ソラネコ ちゃん",
            "Soranekoishちゃん",
            "未登録ちゃん",
            "ちゃん",
            "123456ちゃん",
            "product:skyちゃん",
            "ソラネコちゃん / ウミネコちゃん",
        ] {
            assert!(resolver.resolve_tag(tag).is_none(), "{tag}");
        }
    }

    #[test]
    fn honorific_full_name_and_stripped_name_collisions_are_never_chosen() {
        let ordinary = base("booth:synthetic-curated", "ソラネコ", "Soraneko");
        let honorific = base("product:synthetic-owned", "ソラネコちゃん", "Little Sky");
        for options in [
            vec![ordinary.clone(), honorific.clone()],
            vec![honorific, ordinary.clone()],
            vec![
                ordinary,
                base("product:other-shop", "ソラネコ", "Other Sky"),
            ],
        ] {
            let resolver = AvatarTagResolver::from_options(&options);
            for tag in [
                "ソラネコちゃん",
                "ソラネコちゃん対応",
                "ソラネコちゃん用",
                "ソラネコちゃん専用",
            ] {
                assert!(resolver.resolve_tag(tag).is_none(), "{tag}");
                assert_eq!(resolver.canonical_tag(tag), normalize(tag));
            }
        }
    }

    #[test]
    fn shared_names_are_ambiguous_but_distinct_bilingual_pairs_can_resolve() {
        let a = base("product:shop-a", "ソラネコ", "Starcat");
        let b = base("product:shop-b", "ソラネコ", "Skycat");
        for options in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
            let resolver = AvatarTagResolver::from_options(&options);
            assert!(resolver.resolve_tag("ソラネコ").is_none());
            assert!(resolver.resolve_tag("【ソラネコ】対応").is_none());
            assert_eq!(resolver.canonical_tag("ソラネコ"), "ソラネコ");
            assert_eq!(
                resolver.resolve_tag("ソラネコ / Starcat").unwrap()["id"],
                "product:shop-a"
            );
            assert_eq!(
                resolver.resolve_tag("ソラネコ / Skycat").unwrap()["id"],
                "product:shop-b"
            );
            assert!(resolver.resolve_tag("Starcat / Skycat").is_none());
        }
        let resolver = AvatarTagResolver::from_options(&[a.clone(), a]);
        assert!(resolver.resolve_tag("ソラネコ").is_some());
    }

    #[test]
    fn aliases_and_rejections_share_identity_without_overriding_collisions() {
        let mut a = base("product:synthetic-cat", "ソラネコ", "Soraneko");
        a["aliases"] = json!(["Star Cat", "Old Cat"]);
        let resolver = AvatarTagResolver::from_options(&[a.clone()]);
        let rejected = BTreeSet::from([resolver.canonical_tag("Old Cat")]);
        assert!(rejected.contains(&resolver.canonical_tag("ソラネコ対応")));
        assert!(rejected.contains(&resolver.canonical_tag("Star Cat")));
        let mut b = base("product:other-cat", "ウミネコ", "Umineko");
        b["aliases"] = json!(["Old Cat"]);
        let collision = AvatarTagResolver::from_options(&[a, b]);
        assert!(collision.resolve_tag("Old Cat").is_none());
        assert_eq!(collision.canonical_tag("Old Cat"), "old cat");
    }

    #[test]
    fn curated_and_owned_collisions_do_not_choose_dictionary_order() {
        let known = base("booth:synthetic-known", "真冬", "Mafuyu");
        let owned = base("product:synthetic-other", "真冬", "Mafuyu");
        let resolver = AvatarTagResolver::from_options(&[known, owned]);
        for tag in ["真冬", "Mafuyu", "真冬 / Mafuyu"] {
            assert!(resolver.resolve_tag(tag).is_none());
            assert_eq!(resolver.canonical_tag(tag), normalize(tag));
        }
    }

    #[test]
    fn manual_ids_resolve_exact_identity_without_enabling_automatic_id_tags() {
        let a = base("product:avatar-a", "ソラネコ", "Soraneko");
        let b = base("product:avatar-b", "ソラネコ", "Soraneko");
        let resolver = AvatarTagResolver::from_options(&[a, b]);
        assert!(resolver.resolve_tag("product:avatar-b").is_none());
        let selected = resolver.resolve_manual_avatar("product:avatar-b").unwrap();
        assert_eq!(selected["id"], "product:avatar-b");
        assert_eq!(selected["displayName"], "ソラネコ / Soraneko");
        assert!(resolver.resolve_manual_avatar("ソラネコ").is_none());
        assert!(resolver.resolve_manual_avatar("product:absent").is_none());
    }
}
