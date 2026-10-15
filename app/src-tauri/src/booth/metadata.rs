use super::safety;
use anyhow::{bail, Result};
use scraper::{ElementRef, Html, Selector};
use serde_json::{json, Value};
use unicode_normalization::UnicodeNormalization;

const MAX_SAFE_JPY: u64 = 9_007_199_254_740_991;
fn normalized_price_text(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if raw.chars().count() > 128 || raw.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return None;
    }
    let normalized: String = raw.nfkc().collect();
    Some(normalized.split_whitespace().collect::<Vec<_>>().join(" "))
}
fn jpy_amount(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = text
        .strip_prefix('¥')
        .or_else(|| text.strip_suffix('円'))
        .or_else(|| text.strip_suffix("JPY"))
        .unwrap_or(text)
        .trim();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == ',') {
        return None;
    }
    if digits.contains(',') {
        let groups: Vec<_> = digits.split(',').collect();
        if !(1..=3).contains(&groups[0].len())
            || groups[0].starts_with('0')
            || groups.iter().skip(1).any(|group| group.len() != 3)
        {
            return None;
        }
    }
    digits
        .replace(',', "")
        .parse::<u64>()
        .ok()
        .filter(|v| *v <= MAX_SAFE_JPY)
}
/// Exact integer JPY only. A displayed range is never substituted with its lower bound.
pub(crate) fn parse_jpy_price(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_number() {
        if let Some(integer) = number.as_u64() {
            return (integer <= MAX_SAFE_JPY).then_some(integer as f64);
        }
        let amount = number.as_f64()?;
        return (amount.is_finite()
            && !amount.is_sign_negative()
            && amount.fract() == 0.0
            && amount <= MAX_SAFE_JPY as f64)
            .then_some(amount);
    }
    jpy_amount(&normalized_price_text(value)?).map(|v| v as f64)
}
/// Bounded recognized price text; no HTML stripping or currency/amount guessing.
pub(crate) fn listing_price_display(value: &Value) -> Option<String> {
    if value.is_number() {
        return parse_jpy_price(value).map(|v| format!("¥ {}", v as u64));
    }
    let text = normalized_price_text(value)?;
    if jpy_amount(&text).is_some() {
        return Some(text);
    }
    let mut parts = text.split(['~', '〜']);
    let lower = jpy_amount(parts.next()?)?;
    let upper = parts.next()?.trim();
    if parts.next().is_some() || (!upper.is_empty() && jpy_amount(upper)? < lower) {
        return None;
    }
    Some(text)
}
/// The lowest advertised BOOTH price is a display fallback, not an owned quote.
pub(crate) fn listing_starting_price(value: &Value) -> Option<f64> {
    let display = listing_price_display(value)?;
    let lower = display.split(['~', '〜']).next()?.trim();
    jpy_amount(lower).map(|amount| amount as f64)
}

fn string(v: &Value, key: &str) -> Value {
    v.get(key)
        .and_then(Value::as_str)
        .map(|s| Value::String(s.chars().take(40000).collect()))
        .unwrap_or(Value::Null)
}
fn scalar(v: &Value, key: &str) -> Value {
    v.get(key)
        .filter(|v| v.is_string() || v.is_number())
        .cloned()
        .unwrap_or(Value::Null)
}
fn plain_text(value: &str, limit: usize) -> String {
    let bounded: String = value.chars().take(limit.saturating_mul(4)).collect();
    let fragment = Html::parse_fragment(&bounded);
    let mut text = String::new();
    for node in fragment.root_element().descendants() {
        if node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|e| matches!(e.value().name(), "script" | "style"))
        {
            continue;
        }
        if let Some(part) = node.value().as_text() {
            text.push(' ');
            text.extend(
                part.chars()
                    .filter(|c| !c.is_control() || c.is_whitespace()),
            );
        }
    }
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}
fn plain_string(v: &Value, key: &str, limit: usize) -> Value {
    v.get(key)
        .and_then(Value::as_str)
        .map(|s| Value::String(plain_text(s, limit)))
        .unwrap_or(Value::Null)
}
/// Explicit current-source availability; absent evidence remains unknown.
pub(crate) fn effective_public_page_availability(source: &Value) -> &'static str {
    match source["publicPageAvailability"].as_str() {
        Some("available") => "available",
        Some("unavailable") => "unavailable",
        _ => "unknown",
    }
}
pub(super) fn ensure_source_fields(product: &mut Value) -> Result<()> {
    if product["sourceDescription"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty())
    {
        let retained = plain_string(product, "description", 40000);
        if retained.as_str().is_some_and(|s| !s.is_empty()) {
            product["sourceDescription"] = retained;
        }
    }
    let availability = effective_public_page_availability(product);
    if product["publicPageAvailability"] != availability {
        product
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?
            .insert("publicPageAvailability".into(), json!(availability));
    }
    Ok(())
}
/// Only canonical public HTTP 404/410 is deletion evidence. Other failures do not mutate metadata.
pub(super) fn apply_public_response(
    product: &mut Value,
    id: &str,
    status: u16,
    payload: Option<&Value>,
) -> Result<()> {
    if matches!(status, 404 | 410) {
        ensure_source_fields(product)?;
        product["publicPageAvailability"] = json!("unavailable");
        product["metadataUnavailable"] = json!(true);
        product["publicMetadataErrorCode"] = json!("PUBLIC_METADATA_UNAVAILABLE");
        return Ok(());
    }
    if !(200..300).contains(&status) {
        bail!("PUBLIC_METADATA_FAILED");
    }
    let payload = payload.ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
    let fresh = public_metadata(payload, id)?;
    ensure_source_fields(product)?;
    let object = product
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?;
    for (key, value) in fresh.as_object().unwrap() {
        // A recognized live range supersedes a prior exact quote; sparse/invalid data does not.
        if key == "price" && value.is_null() && listing_price_display(&payload["price"]).is_some() {
            object.insert(key.clone(), Value::Null);
            continue;
        }
        if value.is_null()
            || (matches!(
                key.as_str(),
                "sourceDescription" | "description" | "sourceTitle" | "title" | "name"
            ) && value.as_str().is_some_and(|s| s.trim().is_empty()))
        {
            continue;
        }
        if (matches!(key.as_str(), "sourceTags" | "tags") && !payload["tags"].is_array())
            || (key == "publicVariants" && !payload["variations"].is_array())
            || (key == "sourceCategory"
                && (!payload["category"].is_object()
                    || value
                        .as_object()
                        .is_some_and(|o| o.values().all(Value::is_null))))
        {
            continue;
        }
        if matches!(key.as_str(), "sourceCategory" | "sourceShop") {
            let incoming = value.as_object().unwrap();
            let target = object.entry(key.clone()).or_insert_with(|| json!({}));
            if !target.is_object() {
                *target = json!({});
            }
            for (field, field_value) in incoming {
                if !field_value.is_null()
                    && !field_value.as_str().is_some_and(|s| s.trim().is_empty())
                {
                    target
                        .as_object_mut()
                        .unwrap()
                        .insert(field.clone(), field_value.clone());
                }
            }
            continue;
        }
        object.insert(key.clone(), value.clone());
    }
    object.insert("publicMetadataObservedAt".into(), json!(super::now()));
    object.insert("publicMetadataState".into(), json!("complete"));
    object.remove("publicMetadataErrorCode");
    Ok(())
}
/// Public digital variation identity, label and exact current JPY quote.
fn public_variants(value: &Value) -> Vec<Value> {
    value["variations"]
        .as_array()
        .into_iter()
        .flatten()
        .take(500)
        .filter(|v| v["type"] == "digital")
        .filter_map(|v| {
            let id = v["id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| v["id"].as_u64().map(|id| id.to_string()))?;
            if !safety::numeric(&id) {
                return None;
            }
            // BOOTH's ordinary single digital variation can have a null name.
            // Its exact ID still binds the owned variation's current quote.
            let mut variant = json!({"id":id,"type":"digital"});
            if let Some(name) = v["name"]
                .as_str()
                .map(|name| plain_text(name, 1000))
                .filter(|name| !name.is_empty())
            {
                variant["name"] = json!(name);
            }
            if let Some(price) = parse_jpy_price(&v["price"]) {
                variant["price"] = json!(price);
                variant["currency"] = json!("JPY");
            }
            Some(variant)
        })
        .collect()
}
/// A second public HTML request is useful only where JSON has no positive support,
/// or an avatar-related digital offer cannot yet be interpreted. No global body is stored.
pub(super) fn needs_public_support_html(product: &Value) -> bool {
    if !matches!(
        crate::taxonomy::infer_product_category(product)["category"].as_str(),
        Some("Clothes" | "Hair" | "Accessory" | "Texture" | "Gimmick")
    ) {
        return false;
    }
    // HTML-only support is a last observation, not a reason to skip refreshing
    // the field that supplied it. Fresh JSON may omit that entire section.
    if product["publicSupportAvatars"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        return true;
    }
    let mut fresh = product.clone();
    if let Some(object) = fresh.as_object_mut() {
        object.remove("publicSupportAvatars");
        object.remove("publicSupportObservedAt");
        object.remove("listingAvatars");
    }
    crate::taxonomy::listing_avatar_evidence(&fresh).is_empty()
        || product["publicVariants"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| {
                v["type"] == "digital"
                    && (crate::taxonomy::is_all_inclusive_variant(v["name"].as_str().unwrap_or(""))
                        || crate::taxonomy::variant_avatars(v["name"].as_str().unwrap_or(""))
                            .is_empty())
            })
}
fn support_heading(text: &str) -> bool {
    let key: String = crate::taxonomy::normalize(text)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    [
        "対応アバター",
        "対応モデル",
        "対応素体",
        "対応素體",
        "對應素體",
        "相容素體",
        "支持的模型",
        "supportedavatars",
        "compatibleavatars",
        "supportedmodels",
        "지원아바타",
    ]
    .contains(&key.as_str())
}
fn negative_support(text: &str) -> bool {
    let text = crate::taxonomy::normalize(text);
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
    .any(|word| text.contains(word))
}
/// Only explicit public support sections and their base item links are read.
/// Shipping/comments/recommendations/dependency sections have no role here.
pub(super) fn apply_public_support_html(product: &mut Value, body: &str) -> Result<()> {
    let page = Html::parse_document(body);
    let selector = Selector::parse("section > h2, section > h3").unwrap();
    let dictionary = crate::taxonomy::dictionary();
    let known = dictionary["avatars"].as_array().unwrap();
    let mut avatars = std::collections::BTreeMap::new();
    let mut section_seen = false;
    for heading in page.select(&selector).take(200) {
        if !support_heading(&heading.text().collect::<String>()) {
            continue;
        }
        section_seen = true;
        for block in heading
            .next_siblings()
            .filter_map(ElementRef::wrap)
            .take(20)
        {
            if matches!(block.value().name(), "h1" | "h2" | "h3" | "h4" | "section") {
                break;
            }
            let text: String = block
                .text()
                .collect::<String>()
                .chars()
                .take(20000)
                .collect();
            let mut previous = "";
            for line in text.lines().take(500).map(str::trim) {
                if line.is_empty() {
                    continue;
                }
                // A preceding item label is enough to bind a plain autolink URL.
                // Known IDs resolve directly; unknown IDs retain the seller's label.
                for token in line
                    .split_whitespace()
                    .filter(|s| s.starts_with("https://"))
                {
                    let token = token.trim_end_matches([')', '）', ']', '】', '。', ',', '、']);
                    let Some(url) = safety::public_url(token, "item") else {
                        continue;
                    };
                    let Some(id) = safety::item_id(&url) else {
                        continue;
                    };
                    if negative_support(previous) || negative_support(line) {
                        continue;
                    }
                    let candidate = known
                        .iter()
                        .find(|v| v["productId"].as_str() == Some(id.as_str()))
                        .cloned();
                    let mut candidate = if let Some(candidate) = candidate {
                        candidate
                    } else {
                        let label = previous
                            .trim_start_matches(['・', '-', '*', '●', '•'])
                            .trim();
                        let original = crate::taxonomy::clean_avatar_name(label);
                        if original.is_empty()
                            || original.chars().count() > 96
                            || original.contains("://")
                            || !original.chars().any(char::is_alphabetic)
                        {
                            continue;
                        }
                        let english = crate::taxonomy::romanized_name(&original);
                        let mut candidate = json!({"id":format!("booth:{id}"),"productId":id,"officialName":original,"englishName":english});
                        candidate["displayName"] =
                            json!(crate::taxonomy::avatar_display_name(&candidate));
                        candidate
                    };
                    candidate["sourceUrl"] = json!(url);
                    candidate["basis"] = json!("publicSupportSection");
                    candidate["entitlementEstablished"] = json!(false);
                    avatars.insert(candidate["id"].as_str().unwrap().to_owned(), candidate);
                }
                if !line.starts_with("https://") && !negative_support(line) {
                    for mut candidate in crate::taxonomy::match_avatars(line) {
                        candidate["basis"] = json!("publicSupportSection");
                        avatars
                            .entry(candidate["id"].as_str().unwrap().to_owned())
                            .or_insert(candidate);
                    }
                }
                previous = line;
            }
            for link in block.select(&Selector::parse("a[href]").unwrap()).take(100) {
                let Some(url) = link
                    .value()
                    .attr("href")
                    .and_then(|raw| safety::public_url(raw, "item"))
                else {
                    continue;
                };
                let Some(id) = safety::item_id(&url) else {
                    continue;
                };
                let context = link
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .find(|e| matches!(e.value().name(), "p" | "li"));
                if negative_support(&link.text().collect::<String>())
                    || context.is_some_and(|e| negative_support(&e.text().collect::<String>()))
                {
                    continue;
                }
                if let Some(mut candidate) = known
                    .iter()
                    .find(|v| v["productId"].as_str() == Some(id.as_str()))
                    .cloned()
                {
                    candidate["sourceUrl"] = json!(url);
                    candidate["basis"] = json!("publicSupportSection");
                    candidate["entitlementEstablished"] = json!(false);
                    avatars.insert(candidate["id"].as_str().unwrap().to_owned(), candidate);
                }
            }
        }
    }
    // A successful matching public product page with its known description
    // scope can confirm that the former support section was removed. An
    // unrelated or login/error body cannot withdraw the last-known field.
    let description_seen = page
        .select(&Selector::parse("section.shop__text").unwrap())
        .next()
        .is_some();
    if section_seen || description_seen {
        product["publicSupportAvatars"] = json!(avatars.into_values().collect::<Vec<_>>());
        product["publicSupportObservedAt"] = json!(super::now());
    }
    Ok(())
}
/// Whitelist public metadata; public offered files never become account entitlements.
pub fn public_metadata(value: &Value, id: &str) -> Result<Value> {
    if !safety::numeric(id)
        || !value.is_object()
        || value.get("id").is_none()
        || value["name"]
            .as_str()
            .is_none_or(|s| plain_text(s, 4096).is_empty())
    {
        bail!("PUBLIC_METADATA_INVALID");
    }
    if let Some(actual) = value.get("id").map(|v| {
        v.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string())
    }) {
        if actual != id {
            bail!("PUBLIC_ID_MISMATCH");
        }
    }
    let shop = &value["shop"];
    let category = &value["category"];
    let tags = value["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(500)
                .filter_map(|t| t["name"].as_str().map(|s| plain_text(s, 1000)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let previews = value["images"]
        .as_array()
        .map(|a| {
            a.iter()
                .take(100)
                .filter_map(|i| {
                    i["original"]
                        .as_str()
                        .and_then(|s| safety::public_url(s, "preview"))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(
        json!({"id":id,"store":"BOOTH","title":string(value,"name"),"sourceTitle":plain_string(value,"name",4096),"name":string(value,"name"),"description":string(value,"description"),"sourceDescription":plain_string(value,"description",40000),"publicPageAvailability":"available","publishedAt":string(value,"published_at"),"listingPriceText":scalar(value,"price"),"price":parse_jpy_price(&value["price"]),"isAdult":value.get("is_adult").and_then(Value::as_bool),"sensitive":value.get("is_adult").and_then(Value::as_bool),"sourceShop":{"name":string(shop,"name"),"url":shop["url"].as_str().and_then(|s|safety::public_url(s,"shop")),"uuid":string(shop,"uuid")},"shop":string(shop,"name"),"author":string(shop,"name"),"sourceCategory":{"id":scalar(category,"id"),"name":plain_string(category,"name",4096),"parentId":scalar(&category["parent"],"id"),"parentName":plain_string(&category["parent"],"name",4096)},"sourceTags":tags,"tags":tags,"publicVariants":public_variants(value),"previews":previews,"previewUrl":previews.first(),"sourceUrl":format!("https://booth.pm/ja/items/{id}"),"metadataUnavailable":false,"observedAt":chrono::Utc::now().to_rfc3339()}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_digital_variant_labels_are_preserved_without_files_or_private_fields() {
        let payload = json!({"id":900001,"name":"Authored clothing","variations":[
            {"id":900010,"name":"<b>全部入りフルパック</b>","type":"digital","order_url":"PRIVATE_ORDER","downloadable":{"url":"PRIVATE_SIGNED"}},
            {"id":"900011","name":"Astercat専用","type":"digital"},
            {"id":900012,"name":"Physical version","type":"physical"},
            {"id":"bad-id","name":"Invalid","type":"digital"}]});
        let mut product = public_metadata(&payload, "900001").unwrap();
        assert_eq!(
            product["publicVariants"],
            json!([
            {"id":"900010","name":"全部入りフルパック","type":"digital"},
            {"id":"900011","name":"Astercat専用","type":"digital"}])
        );
        assert!(!product.to_string().contains("PRIVATE"));
        assert!(product.get("files").is_none());
        apply_public_response(
            &mut product,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Sparse authored"})),
        )
        .unwrap();
        assert_eq!(product["publicVariants"].as_array().unwrap().len(), 2);
        apply_public_response(
            &mut product,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Empty authored","variations":[]})),
        )
        .unwrap();
        assert_eq!(product["publicVariants"], json!([]));
    }
    #[test]
    fn public_owned_variation_quotes_keep_zero_and_do_not_allocate_listing_ranges() {
        let product = public_metadata(
            &json!({"id":900001,"name":"Authored variants","price":"¥ 100 - ¥ 300","variations":[
                {"id":800001,"name":"Zero","type":"digital","price":0},
                {"id":800002,"name":"Paid","type":"digital","price":"¥ 300"},
                {"id":800003,"name":"Unknown","type":"digital","price":"¥ 100 - ¥ 300"}
            ]}),
            "900001",
        )
        .unwrap();
        assert_eq!(product["publicVariants"][0]["price"].as_f64(), Some(0.0));
        assert_eq!(product["publicVariants"][1]["price"].as_f64(), Some(300.0));
        assert_eq!(product["publicVariants"][1]["currency"], "JPY");
        assert!(product["publicVariants"][2].get("price").is_none());
        assert!(product["price"].is_null());
    }
    #[test]
    fn unnamed_digital_variations_keep_exact_quotes_without_listing_inference() {
        let mut product = json!({"id":"900001","note":"Human note"});
        apply_public_response(
            &mut product,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Authored unnamed variations","description":"Authored digital clothing","shop":{"name":"Authored shop","url":"https://invented.booth.pm"},"category":{"id":1,"name":"3D衣装"},"tags":[{"name":"VRChat"}],"images":[],"price":"¥ 100~","variations":[
                {"id":800001,"name":null,"type":"digital","price":6000},
                {"id":"800002","type":"digital","price":0},
                {"id":800003,"name":" ","type":"digital","price":"¥ 100~"},
                {"id":800004,"name":null,"type":"physical","price":100},
                {"id":"invalid","name":null,"type":"digital","price":100}
            ]})),
        )
        .unwrap();
        assert_eq!(
            product["publicVariants"],
            json!([
                {"id":"800001","type":"digital","price":6000.0,"currency":"JPY"},
                {"id":"800002","type":"digital","price":0.0,"currency":"JPY"},
                {"id":"800003","type":"digital"}
            ])
        );
        assert!(product["price"].is_null());
        assert_eq!(product["note"], "Human note");
    }
    #[test]
    fn public_support_sections_use_positive_item_links_and_ignore_other_sections() {
        let mut product = json!({"sourceTitle":"Authored clothing","sourceCategory":{"name":"3D衣装"},"sourceTags":["VRChat"]});
        assert!(needs_public_support_html(&product));
        apply_public_support_html(&mut product,r#"<main>
            <section><h2>Credits</h2><p>真冬 https://booth.pm/ja/items/5007531</p></section>
            <section><h2>◆対応アバター</h2><p>・オリジナル3Dモデル「しなの」
https://ponderogen.booth.pm/items/6106863
・オリジナル3Dモデル「そらねこ」
https://synthetic.booth.pm/items/900099
・マヌカ非対応
https://jingo1016.booth.pm/items/5058077</p></section>
            <section><h2>Dependencies</h2><p>Lapwing https://booth.pm/ja/items/4993931</p></section>
            <section><h2>非対応アバター</h2><p>マリシア https://booth.pm/ja/items/6305948</p></section>
        </main>"#).unwrap();
        let found = product["publicSupportAvatars"].as_array().unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|v| v["id"] == "booth:6106863"));
        assert!(found
            .iter()
            .any(|v| v["id"] == "booth:900099" && v["officialName"] == "そらねこ"));
        assert!(found.iter().all(|v| v["entitlementEstablished"] == false));
        assert!(needs_public_support_html(&product));
        assert!(product.get("html").is_none());
        let prior = product["publicSupportAvatars"].clone();
        apply_public_support_html(
            &mut product,
            "<section><h2>Credits</h2><p>Manuka</p></section>",
        )
        .unwrap();
        assert_eq!(product["publicSupportAvatars"], prior);
        product["sourceCategory"] = json!({"name":"ソフトウェア"});
        product["sourceTitle"] = json!("Authored Blender addon");
        product["publicSupportAvatars"] = json!([]);
        assert!(!needs_public_support_html(&product));
    }
    #[test]
    fn html_only_public_support_refreshes_and_withdraws_stale_bases() {
        let mut product = json!({"sourceTitle":"Authored VRChat clothing","sourceCategory":{"name":"3D衣装"},"sourceTags":["VRChat"],"publicVariants":[{"id":"900010","name":"全部入りフルパック","type":"digital"}]});
        apply_public_support_html(&mut product,"<section class='shop__text'><h2>対応アバター</h2><p>しなの\nマヌカ\nLapwing\nマリシア</p></section>").unwrap();
        assert_eq!(product["publicSupportAvatars"].as_array().unwrap().len(), 4);
        assert!(needs_public_support_html(&product));
        assert_eq!(
            crate::taxonomy::account_variant_avatars(&product, Some("900010"), "").len(),
            4
        );
        apply_public_support_html(&mut product,"<section class='shop__text'><h2>対応アバター</h2><p>しなの\nマヌカ\nLapwing</p></section>").unwrap();
        assert_eq!(product["publicSupportAvatars"].as_array().unwrap().len(), 3);
        assert_eq!(
            crate::taxonomy::account_variant_avatars(&product, Some("900010"), "").len(),
            3
        );
        assert!(needs_public_support_html(&product));
        let retained = product["publicSupportAvatars"].clone();
        apply_public_support_html(&mut product, "<main>Login required</main>").unwrap();
        assert_eq!(product["publicSupportAvatars"], retained);
        apply_public_support_html(
            &mut product,
            "<section class='shop__text'><h2>対応アバター</h2><p></p></section>",
        )
        .unwrap();
        assert_eq!(product["publicSupportAvatars"], json!([]));
        assert!(crate::taxonomy::account_variant_avatars(&product, Some("900010"), "").is_empty());
        product["publicSupportAvatars"] = retained;
        apply_public_support_html(
            &mut product,
            "<section class='shop__text'><h2>商品概要</h2><p>Authored clothing</p></section>",
        )
        .unwrap();
        assert_eq!(product["publicSupportAvatars"], json!([]));
        assert!(needs_public_support_html(&product));
    }
    #[test]
    fn all_inclusive_offer_refreshes_support_even_with_known_title_bases() {
        let mut product = json!({"sourceTitle":"しなの マヌカ VRChat clothing","sourceCategory":{"name":"3D衣装"},"sourceTags":["VRChat","しなの","マヌカ"],"publicVariants":[{"id":"900010","name":"全部入りフルパック","type":"digital"}]});
        assert_eq!(crate::taxonomy::listing_avatar_evidence(&product).len(), 2);
        assert!(needs_public_support_html(&product));
        apply_public_support_html(&mut product, "<section class='shop__text'><h2>対応アバター</h2><p>しなの\nマヌカ\nLapwing\nマリシア</p></section>").unwrap();
        assert_eq!(product["publicSupportAvatars"].as_array().unwrap().len(), 4);
        assert_eq!(
            crate::taxonomy::account_variant_avatars(&product, Some("900010"), "").len(),
            4
        );
    }
    #[test]
    fn exact_jpy_prices_require_safe_whole_amounts_and_strict_grouping() {
        for (value, amount) in [
            (json!(0), 0.0),
            (json!(1500), 1500.0),
            (json!(1500.0), 1500.0),
            (json!("¥ 1,500"), 1500.0),
            (json!("￥１，５００"), 1500.0),
            (json!("  1,500円  "), 1500.0),
            (json!("1,500 JPY"), 1500.0),
            (json!("  ０　ＪＰＹ  "), 0.0),
            (json!("1500"), 1500.0),
            (json!("¥ 0"), 0.0),
            (json!(MAX_SAFE_JPY), MAX_SAFE_JPY as f64),
        ] {
            assert_eq!(parse_jpy_price(&value), Some(amount), "{value}");
        }
        for value in [
            json!(MAX_SAFE_JPY + 1),
            json!(-1),
            json!(-0.0),
            json!(1.5),
            json!(true),
            Value::Null,
            json!("無料"),
            json!("NaN"),
            json!("Infinity"),
            json!("¥ -1"),
            json!("+100"),
            json!("1e3"),
            json!("$ 100"),
            json!("USD 100"),
            json!("100 USD"),
            json!("JPY 100"),
            json!("100 jpy"),
            json!("¥ 100 JPY"),
            json!("100 円 JPY"),
            json!("1,50 JPY"),
            json!("01,500 JPY"),
            json!("100.0 JPY"),
            json!("-100 JPY"),
            json!("100 JPY shipping"),
            json!("100 JPY ~ 200 JPY"),
            json!("9007199254740992 JPY"),
            json!("¥ 1,50"),
            json!("¥ 1,500,"),
            json!("¥ 01,500"),
            json!("¥ 1 500"),
            json!("¥ 100.0"),
            json!("¥ 100 円"),
            json!("¥ 100 ~"),
            json!("¥ 100 ~ 200"),
            json!("<b>¥ 100</b>"),
            json!("¥ 100\u{0000}"),
            json!("9".repeat(129)),
        ] {
            assert!(parse_jpy_price(&value).is_none(), "{value}");
        }
    }
    #[test]
    fn bounded_price_display_recognizes_ranges_without_claiming_exact_price() {
        for (value, display) in [
            (json!("¥ 1,500 ~"), "¥ 1,500 ~"),
            (json!("￥１，５００　～"), "¥1,500 ~"),
            (json!("¥ 100 〜 ¥ 1,500"), "¥ 100 〜 ¥ 1,500"),
            (json!("¥ 100 ~ 100"), "¥ 100 ~ 100"),
        ] {
            assert_eq!(listing_price_display(&value).as_deref(), Some(display));
            assert!(parse_jpy_price(&value).is_none());
        }
        assert_eq!(listing_price_display(&json!(0)).as_deref(), Some("¥ 0"));
        assert_eq!(listing_starting_price(&json!("¥ 1,500 ~")), Some(1500.0));
        assert_eq!(listing_starting_price(&json!("¥ 100 〜 ¥ 1,500")), Some(100.0));
        assert_eq!(listing_starting_price(&json!(0)), Some(0.0));
        assert_eq!(listing_starting_price(&json!("¥ 500 ~ 100")), None);
        for text in [
            "¥ 500 ~ 100",
            "¥ 100 ~~",
            "¥ 100 ~ USD 200",
            "¥ 1,50 ~",
            "~ ¥ 100",
            "無料",
            "<b>¥ 100 ~</b>",
            "¥ 100 ~ shipping",
            "¥ 100 / ¥ 200",
        ] {
            assert!(listing_price_display(&json!(text)).is_none(), "{text}");
        }
    }
    #[test]
    fn public_price_quotes_preserve_source_text_and_never_use_variations() {
        for value in [json!("¥ 1,500"), json!(1500), json!("¥ 1,500 ~")] {
            let public = public_metadata(
                &json!({"id":900001,"name":"Authored","price":value,"variations":[{"price":0}]}),
                "900001",
            )
            .unwrap();
            assert_eq!(public["price"], json!(parse_jpy_price(&value)));
        }
        let mut product = json!({"id":"900001","price":1500,"listingPriceText":"¥ 1,500"});
        apply_public_response(
            &mut product,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Authored","price":"¥ 1,500 ~"})),
        )
        .unwrap();
        assert!(product["price"].is_null());
        assert_eq!(product["listingPriceText"], "¥ 1,500 ~");
        let range = product.clone();
        assert!(apply_public_response(&mut product, "900001", 503, None).is_err());
        assert_eq!(product, range);
        apply_public_response(
            &mut product,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Authored","price":0})),
        )
        .unwrap();
        assert_eq!(product["price"].as_f64(), Some(0.0));
        apply_public_response(
            &mut product,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Authored"})),
        )
        .unwrap();
        assert_eq!(product["price"].as_f64(), Some(0.0));
    }
    #[test]
    fn only_public_fields_and_safe_urls_survive() {
        let v = json!({"id":900001,"name":"Invented","cookies":"secret","shop":{"name":"Maker","url":"https://invented.booth.pm/?token=bad"},"images":[{"original":"https://booth.pximg.net/test.png?token=bad"}],"tags":[{"name":"Authored"}],"variations":[{"downloadable":{"url":"https://booth.pm/downloadables/7?token=secret"}}]});
        let p = public_metadata(&v, "900001").unwrap();
        assert_eq!(p["sourceUrl"], "https://booth.pm/ja/items/900001");
        let s = p.to_string();
        assert!(!s.contains("secret"));
        assert!(!s.contains("token"));
        assert!(p.get("files").is_none());
        assert!(public_metadata(&v, "900002").is_err());
    }
    fn retained_product() -> Value {
        json!({"id":"900001","sourceTitle":"Authored VRChat tool","title":"Authored VRChat tool","description":"Retained VRChat Unity description","sourceDescription":"Retained VRChat Unity description","sourceTags":["VRChat","Unity"],"tags":["VRChat","Unity"],"sourceCategory":{"name":"3D tools"},"publicPageAvailability":"available"})
    }
    #[test]
    fn public_description_is_clean_and_bounded() {
        let payload = json!({"id":900001,"name":"<b>Authored avatar</b>","description":"<p>Unity &amp; VRChat</p><script>NOT_CLASSIFICATION</script><style>NOT_CLASSIFICATION</style><p>対応\nモデル\u{0001}</p>","tags":[{"name":"<b>VRChat</b>"}],"category":{"name":"<b>3D</b>"}});
        let public = public_metadata(&payload, "900001").unwrap();
        assert_eq!(public["sourceDescription"], "Unity & VRChat 対応 モデル");
        assert_eq!(public["sourceTitle"], "Authored avatar");
        assert_eq!(public["sourceTags"], json!(["VRChat"]));
        assert_eq!(public["sourceCategory"]["name"], "3D");
        assert_eq!(public["publicPageAvailability"], "available");
        let long = public_metadata(
            &json!({"id":900001,"name":"Invented","description":"界".repeat(50000)}),
            "900001",
        )
        .unwrap();
        assert_eq!(
            long["sourceDescription"].as_str().unwrap().chars().count(),
            40000
        );
    }
    #[test]
    fn deleted_page_retains_last_good_vr_classification_inputs() {
        for status in [404, 410] {
            let mut product = retained_product();
            let before = product.clone();
            apply_public_response(&mut product, "900001", status, None).unwrap();
            for key in [
                "sourceTitle",
                "title",
                "description",
                "sourceDescription",
                "sourceTags",
                "tags",
                "sourceCategory",
            ] {
                assert_eq!(product[key], before[key], "{key}");
            }
            assert_eq!(product["publicPageAvailability"], "unavailable");
            assert_eq!(product["metadataUnavailable"], true);
            let deleted = product.clone();
            assert!(apply_public_response(&mut product, "900001", 503, None).is_err());
            assert_eq!(product, deleted);
            apply_public_response(
                &mut product,
                "900001",
                200,
                Some(&json!({"id":900001,"name":"Authored VRChat tool"})),
            )
            .unwrap();
            assert_eq!(product["publicPageAvailability"], "available");
            assert_eq!(product["metadataUnavailable"], false);
            assert_eq!(product["sourceDescription"], before["sourceDescription"]);
            assert_eq!(product["sourceTags"], before["sourceTags"]);
            assert_eq!(product["sourceCategory"], before["sourceCategory"]);
        }
    }
    #[test]
    fn failed_or_malformed_public_response_never_claims_deleted_or_erases_metadata() {
        for status in [401, 403, 429, 500, 503] {
            let mut product = retained_product();
            let before = product.clone();
            assert!(apply_public_response(&mut product, "900001", status, None).is_err());
            assert_eq!(product, before);
        }
        for payload in [
            json!({}),
            json!({"name":"Invented"}),
            json!({"id":900001,"name":""}),
            json!({"id":900002,"name":"Wrong item"}),
            json!({"id":900001,"name":"<script>Invalid</script>"}),
        ] {
            let mut product = retained_product();
            let before = product.clone();
            assert!(apply_public_response(&mut product, "900001", 200, Some(&payload)).is_err());
            assert_eq!(product, before);
        }
        let mut new = json!({"id":"900001","description":"<p>Legacy Unity VRChat</p>"});
        ensure_source_fields(&mut new).unwrap();
        assert_eq!(new["publicPageAvailability"], "unknown");
        assert_eq!(new["sourceDescription"], "Legacy Unity VRChat");
        let mut sparse = retained_product();
        apply_public_response(&mut sparse,"900001",200,Some(&json!({"id":900001,"name":"Authored VRChat tool","description":"  ","category":{}}))).unwrap();
        assert_eq!(
            sparse["sourceDescription"],
            "Retained VRChat Unity description"
        );
        assert_eq!(sparse["description"], "Retained VRChat Unity description");
        assert_eq!(sparse["sourceCategory"], json!({"name":"3D tools"}));
        sparse["sourceCategory"] = json!({"id":7,"name":"Shader","parentId":2,"parentName":"3D"});
        apply_public_response(
            &mut sparse,
            "900001",
            200,
            Some(&json!({"id":900001,"name":"Authored VRChat tool","category":{"id":7}})),
        )
        .unwrap();
        assert_eq!(
            sparse["sourceCategory"],
            json!({"id":7,"name":"Shader","parentId":2,"parentName":"3D"})
        );
        apply_public_response(&mut sparse, "900001", 200, Some(&json!({"id":900001,"name":"Authored VRChat tool","category":{"id":8,"name":"Motion","parent":{"id":3,"name":"Animation"}}}))).unwrap();
        assert_eq!(
            sparse["sourceCategory"],
            json!({"id":8,"name":"Motion","parentId":3,"parentName":"Animation"})
        );
    }
}
