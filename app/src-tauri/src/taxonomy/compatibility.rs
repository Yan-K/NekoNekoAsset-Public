//! Positive public support and account-bound variant compatibility.
//! No public offer by itself establishes account accessibility.
use super::{avatar_display_name, exact_avatar_tag, match_avatars, normalize, romanized_name};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn negative(text: &str) -> bool {
    [
        "非対応",
        "未対応",
        "対応予定",
        "not supported",
        "unsupported",
        "incompatible",
        "不支援",
        "不支持",
        "미지원",
    ]
    .iter()
    .any(|word| normalize(text).contains(word))
}
fn label(candidate: &Value) -> &str {
    candidate["displayName"]
        .as_str()
        .or(candidate["officialName"].as_str())
        .unwrap_or("")
}
fn insert(map: &mut BTreeMap<String, Value>, mut avatar: Value, basis: &str) {
    if label(&avatar).is_empty() {
        return;
    }
    avatar["basis"] = json!(basis);
    avatar["entitlementEstablished"] = json!(false);
    let key = avatar["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| normalize(label(&avatar)));
    map.entry(key).or_insert(avatar);
}
/// Explicit base-specific offer names also work before that base is curated.
/// A named garment, platform, generic audience or mixed label is not a base.
pub fn variant_avatars(name: &str) -> Vec<Value> {
    if negative(name) || is_all_inclusive_variant(name) {
        return vec![];
    }
    let mut avatars = BTreeMap::new();
    if let Some(avatar) = exact_avatar_tag(name) {
        insert(&mut avatars, avatar, "publicVariant");
        return avatars.into_values().collect();
    }
    for avatar in match_avatars(name) {
        insert(&mut avatars, avatar, "publicVariant");
    }
    if !avatars.is_empty() {
        return avatars.into_values().collect();
    }
    let name = name.trim();
    let bare = ["専用", "対応", "對應", "对应", "전용", "用"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .map(str::trim);
    let Some(bare) = bare else {
        return vec![];
    };
    let cleaned = super::clean_avatar_name(bare);
    let key = normalize(&cleaned);
    if cleaned.is_empty()
        || cleaned.chars().count() > 96
        || !cleaned.chars().any(char::is_alphabetic)
        || cleaned.contains(['/', '\\', ':', '\n', '\r', '+', '・', '、', ','])
        || [
            "vrchat",
            "vrc",
            "unity",
            "pc",
            "quest",
            "fbx",
            "vrm",
            "男性",
            "女性",
            "共通",
            "汎用",
            "通常",
            "カラー",
            "色",
            "all",
        ]
        .contains(&key.as_str())
    {
        return vec![];
    }
    let english = if cleaned.is_ascii() {
        cleaned.clone()
    } else {
        romanized_name(&cleaned)
    };
    let mut avatar = json!({"officialName":cleaned,"englishName":english,"basis":"publicVariant","entitlementEstablished":false});
    avatar["displayName"] = json!(avatar_display_name(&avatar));
    vec![avatar]
}
/// Only explicit whole-product/all-avatar bundle names; constrained packs are excluded.
pub fn is_all_inclusive_variant(name: &str) -> bool {
    if negative(name)
        || ["のみ", "除く", "除外", "except", "excluding", "only"]
            .iter()
            .any(|w| normalize(name).contains(w))
    {
        return false;
    }
    let key: String = normalize(name)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    [
        "全部入り",
        "全アバター",
        "全素体",
        "全対応アバター",
        "allavatars",
        "allbases",
        "allversions",
        "allvariants",
    ]
    .iter()
    .any(|marker| key.contains(marker))
        || [
            "fullpack",
            "fullset",
            "フルパック",
            "フルセット",
            "全部",
            "allinone",
        ]
        .contains(&key.as_str())
}
/// Public listing support, independent from accounts. Dedicated support sections
/// take precedence as evidence; unrelated dependency/credit mentions are ignored.
pub fn listing_avatar_evidence(source: &Value) -> Vec<Value> {
    let mut avatars = BTreeMap::new();
    for avatar in source["publicSupportAvatars"]
        .as_array()
        .into_iter()
        .flatten()
    {
        insert(&mut avatars, avatar.clone(), "publicSupportSection");
    }
    for variant in source["publicVariants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["type"] == "digital")
    {
        for avatar in variant_avatars(variant["name"].as_str().unwrap_or("")) {
            insert(&mut avatars, avatar, "publicVariant");
        }
    }
    for tag in source["sourceTags"]
        .as_array()
        .or(source["tags"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !negative(tag) {
            for avatar in match_avatars(tag) {
                insert(&mut avatars, avatar, "publicTag");
            }
        }
    }
    let title = source["sourceTitle"]
        .as_str()
        .or(source["title"].as_str())
        .or(source["name"].as_str())
        .unwrap_or("");
    if !negative(title) {
        for avatar in match_avatars(title) {
            insert(&mut avatars, avatar, "publicTitle");
        }
    }
    for name in source["listingAvatars"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(avatar) = exact_avatar_tag(name) {
            insert(&mut avatars, avatar, "retainedListing");
        } else if !name.trim().is_empty() {
            insert(
                &mut avatars,
                json!({"officialName":name,"displayName":name}),
                "retainedListing",
            );
        }
    }
    avatars.into_values().collect()
}
/// Account evidence is supplied only from currently accessible file records.
/// A whole bundle expands to the public support set only when its exact offer is
/// bound by variant ID, or by one unambiguous exact offer name. Public inventory
/// does not create remote files, acquisitions or entitlements.
pub fn account_variant_avatars(
    source: &Value,
    variant_id: Option<&str>,
    variant_name: &str,
) -> Vec<Value> {
    let variants: Vec<_> = source["publicVariants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["type"] == "digital")
        .collect();
    let matched: Vec<_> = if let Some(id) = variant_id.filter(|id| !id.is_empty()) {
        variants
            .iter()
            .copied()
            .filter(|v| v["id"].as_str() == Some(id))
            .collect()
    } else {
        variants
            .iter()
            .copied()
            .filter(|v| normalize(v["name"].as_str().unwrap_or("")) == normalize(variant_name))
            .collect()
    };
    if matched.len() == 1 {
        let public_name = matched[0]["name"].as_str().unwrap_or("");
        if is_all_inclusive_variant(public_name) {
            return listing_avatar_evidence(source);
        }
        // The bound current offer also resolves a single-base variant when
        // the account label is missing, opaque or stale.
        return variant_avatars(public_name);
    }
    if matched.len() > 1 {
        return vec![];
    }
    // An observed account label is itself evidence for that accessible variant,
    // but a qualified pack never expands to other publicly offered bases.
    variant_avatars(variant_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_offers_cover_all_named_bases_without_avatar_tags() {
        let source = json!({"sourceTitle":"Authored VRChat clothing","sourceTags":["VRChat"],"publicVariants":[
            {"id":"900001","type":"digital","name":"全部入りフルパック"},
            {"id":"900002","type":"digital","name":"しなの専用"},
            {"id":"900003","type":"digital","name":"マヌカ専用"},
            {"id":"900004","type":"digital","name":"Lapwing専用"},
            {"id":"900005","type":"digital","name":"マリシア専用"},
            {"id":"900006","type":"physical","name":"真冬専用"}]});
        let public = listing_avatar_evidence(&source);
        assert_eq!(public.len(), 4);
        assert!(public.iter().all(|v| v["entitlementEstablished"] == false));
        assert_eq!(
            account_variant_avatars(&source, Some("900001"), "Opaque file label").len(),
            4
        );
        assert_eq!(
            account_variant_avatars(&source, None, "全部入りフルパック").len(),
            4
        );
        assert!(account_variant_avatars(&source, Some("unknown"), "全部入りフルパック").is_empty());
        assert_eq!(
            account_variant_avatars(&source, Some("900002"), "しなの専用").len(),
            1
        );
        let single =
            account_variant_avatars(&source, Some("900002"), "Opaque or stale Manuka label");
        assert_eq!(single.len(), 1);
        assert_eq!(single[0]["id"], "booth:6106863");
        assert_eq!(
            account_variant_avatars(&source, None, "FullPack (Manuka only)").len(),
            1
        );
    }
    #[test]
    fn uncurated_explicit_variant_names_and_negative_support_are_bounded() {
        for name in ["そらねこ専用", "Astercat専用", "シロネコ対応"] {
            assert_eq!(variant_avatars(name).len(), 1, "{name}");
        }
        for name in [
            "Quest専用",
            "カラー専用",
            "男性専用",
            "Astercat coat",
            "Astercat/Othercat専用",
            "マヌカ非対応",
            "Manuka not supported",
        ] {
            assert!(variant_avatars(name).is_empty(), "{name}");
        }
        let source = json!({"publicVariants":[{"id":"900001","type":"digital","name":"FullPack"},{"id":"900002","type":"digital","name":"FullPack"},{"id":"900003","type":"digital","name":"Astercat専用"}],"description":"Credits: Manuka"});
        assert_eq!(listing_avatar_evidence(&source).len(), 1);
        assert!(account_variant_avatars(&source, None, "FullPack").is_empty());
    }
}
