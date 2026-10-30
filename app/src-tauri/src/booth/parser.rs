//! Independently authored Rust parser, informed by this project's MIT collector.
//! Digital item fields and explicitly bound receipt metadata only.
//! Physical/contact/shipping/comment bodies are not parsed.
use super::safety;
use anyhow::{bail, Result};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("fixed selector")
}
fn html_language(doc: &Html) -> &'static str {
    doc.select(&sel("html"))
        .next()
        .and_then(|node| node.value().attr("lang"))
        .filter(|value| !value.trim().is_empty())
        .map_or("unknown", |value| {
            let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
            match normalized.as_str() {
                "zh-cn" | "zh-hans" | "zh-hans-cn" => return "zh-cn",
                "zh-tw" | "zh-hant" | "zh-hant-tw" => return "zh-tw",
                _ => {}
            }
            match value.trim().split(['-', '_']).next().unwrap_or("") {
                value if value.eq_ignore_ascii_case("ja") => "ja",
                value if value.eq_ignore_ascii_case("en") => "en",
                value if value.eq_ignore_ascii_case("ko") => "ko",
                _ => "other",
            }
        })
}
fn text(e: ElementRef<'_>) -> String {
    safety::short(&e.text().collect::<String>())
}
fn first_text(e: ElementRef<'_>, selector: &str) -> Option<String> {
    e.select(&sel(selector))
        .next()
        .map(text)
        .filter(|s| !s.is_empty())
}
fn absolute(value: &str) -> Option<String> {
    url::Url::parse("https://accounts.booth.pm")
        .unwrap()
        .join(value)
        .ok()
        .map(|u| u.to_string())
}
fn links(e: ElementRef<'_>) -> Vec<ElementRef<'_>> {
    let mut v: Vec<_> = e.select(&sel("a[href]")).collect();
    if e.value().name() == "a" && e.value().attr("href").is_some() {
        v.insert(0, e);
    }
    v
}
fn identity(e: ElementRef<'_>) -> Option<String> {
    safety::item_id(&absolute(e.value().attr("href")?)?)
}
fn downloads(e: ElementRef<'_>, receipt: bool) -> Vec<(ElementRef<'_>, String, Option<String>, String)> {
    let mut found: Vec<_> = e.select(&sel(
        "[data-test='downloadable'],a[href*='/downloadables/'],[data-href*='/downloadables/']",
    ))
    .filter_map(|n| {
        let value = n
            .value()
            .attr("data-href")
            .or_else(|| n.value().attr("href"))?;
        let (id, variant, route) = safety::observed_resolver(&absolute(value)?)?;
        Some((n, id, variant, route))
    })
    .collect();
    for node in e.select(&sel(dropdown_controls(receipt))) {
        let Some(items) = dropdown_items(node) else { continue };
        // A track menu offers one downloadable in several formats (`path?type=`).
        // Every format must resolve, or the menu stays an unresolved control.
        let key = if is_track(node) { "path" } else { "deeplinkDownloadableUrl" };
        let resolved: Vec<_> = items.iter().map(|item| item[key].as_str().and_then(safety::observed_resolver)).collect();
        if is_track(node) && resolved.iter().any(Option::is_none) { continue; }
        let mut seen = BTreeSet::new();
        for (id, variant, route) in resolved.into_iter().flatten() {
            if seen.insert((id.clone(), variant.clone())) {
                found.push((node, id, variant, route));
            }
        }
    }
    let mut bindings = Vec::new();
    if receipt {
        // Primary receipt links omit variation_id; the same file row's alternate
        // methods supply it. Keep each control observation for closure checks,
        // while files() deduplicates their single confirmed file/variant identity.
        for (index, (node, file, variant, _)) in found.iter().enumerate() {
            if variant.is_some() { continue; }
            let Some(row) = node.ancestors().filter_map(ElementRef::wrap)
                .find(|row| row.value().classes().any(|class| class == "legacy-list-item")) else { continue };
            if row.id() != e.id() && !e.descendants().any(|child| child.id() == row.id()) { continue; }
            let controls: Vec<_> = found.iter().filter(|(control, _, _, _)|
                row.descendants().any(|child| child.id() == control.id())).collect();
            if controls.iter().any(|(_, id, _, _)| id != file) { continue; }
            let variants: BTreeSet<_> = controls.iter().filter_map(|(control, _, variant, _)|
                control.value().attr("data-dropdown-items").and(variant.as_ref())).collect();
            if variants.len() == 1 {
                bindings.push((index, (*variants.iter().next().unwrap()).clone()));
            }
        }
    } else {
        // Alternate app controls cannot add Library files. They can clarify an
        // already accessible primary file when its own filename row binds one
        // exact downloadable and one variation.
        for (index, (node, file, variant, _)) in found.iter().enumerate() {
            if variant.is_some() { continue; }
            // A track row shows a title rather than a filename; its own format
            // menu already scopes it to one downloadable.
            let track = is_track(*node);
            for row in node.ancestors().filter_map(ElementRef::wrap) {
                if row.id() == e.id() || !e.descendants().any(|child| child.id() == row.id()) { break; }
                if found.iter().any(|(control, id, _, _)| id != file && row.descendants().any(|child| child.id() == control.id())) { break; }
                let names = row.select(&sel("[data-test='filename'],.min-w-0.break-words.whitespace-pre-line .text-14,.legacy-list-item .min-w-0 > b"))
                    .filter(|name| looks_filename(&text(*name))).count();
                if names > 1 || links(row).into_iter().any(|link| identity(link).is_some()) { break; }
                if names == 0 && !track { continue; }
                let alternates: Vec<_> = row.select(&sel("[data-test='other-downloads-button'][data-dropdown-items]")).collect();
                if alternates.is_empty() {
                    if track { continue; }
                    break;
                }
                let mut variants = BTreeSet::new();
                let valid = alternates.iter().all(|alternate| {
                    let Some(raw) = alternate.value().attr("data-dropdown-items").filter(|raw| raw.len() <= 262_144) else { return false };
                    let Ok(items) = serde_json::from_str::<Vec<Value>>(raw) else { return false };
                    items.iter().all(|item| {
                        let Some(url) = item["deeplinkDownloadableUrl"].as_str() else { return false };
                        let Some((id, variant, _)) = safety::observed_resolver(url) else { return false };
                        if &id != file { return false; }
                        if let Some(variant) = variant { variants.insert(variant); }
                        true
                    })
                });
                let primary_variants_agree = found.iter().filter(|(control, _, _, _)|
                    row.descendants().any(|child| child.id() == control.id()))
                    .all(|(_, _, variant, _)| variant.as_ref().is_none_or(|variant| variants.contains(variant)));
                if valid && variants.len() == 1 && primary_variants_agree {
                    bindings.push((index, variants.into_iter().next().unwrap()));
                }
                break;
            }
        }
    }
    for (index, variant) in bindings {
        found[index].2 = Some(variant.clone());
        found[index].3 = format!("https://booth.pm/downloadables/{}?variation_id={variant}", found[index].1);
    }
    found
}
fn raw_download_controls(e: ElementRef<'_>, receipt: bool) -> usize {
    let tracks: BTreeSet<_> = downloads(e, receipt).into_iter().filter(|(node, ..)| is_track(*node)).map(|(_, id, ..)| id).collect();
    e.select(&sel(&format!("[data-test='downloadable'],a[href*='/downloadables/'],[data-href*='/downloadables/'],{}", dropdown_controls(receipt))))
        .filter(|node| !track_bundle(*node, &tracks))
        .count()
}
/// Library alternate-app menus can clarify a primary file but never add one.
fn dropdown_controls(receipt: bool) -> &'static str {
    if receipt { "[data-dropdown-items]" } else { "[data-dropdown-items]:not([data-test='other-downloads-button']):not([data-test='other-multitrack-downloads-button'])" }
}
fn dropdown_items(node: ElementRef<'_>) -> Option<Vec<Value>> {
    let raw = node.value().attr("data-dropdown-items").filter(|raw| raw.len() <= 262_144)?;
    serde_json::from_str(raw).ok()
}
fn is_track(node: ElementRef<'_>) -> bool {
    node.value().attr("data-test") == Some("track-download-button")
}
/// An album zip menu bundles listed tracks in the browser; it is not a file of
/// its own. An entry outside the card's resolved tracks keeps it unresolved.
fn track_bundle(node: ElementRef<'_>, tracks: &BTreeSet<String>) -> bool {
    node.value().attr("data-test") == Some("zipped-download-button")
        && dropdown_items(node).is_some_and(|items| !items.is_empty() && items.iter().all(|item| {
            item["zippedFiles"].as_array().is_some_and(|entries| !entries.is_empty() && entries.iter().all(|entry| {
                entry["prefetchUrl"].as_str().and_then(prefetch_file).is_some_and(|id| tracks.contains(&id))
            }))
        }))
}
fn prefetch_file(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("booth.pm") { return None; }
    let mut path = url.path_segments()?;
    let id = (path.next()? == "downloadables").then(|| path.next()).flatten()?;
    safety::numeric(id).then(|| id.to_owned())
}
/// Format choices name one track file only when they all carry the same name.
fn track_filename(node: ElementRef<'_>) -> Option<String> {
    let mut names = dropdown_items(node)?.iter().map(|item| item["data"]["filename"].as_str().map(str::to_owned)).collect::<Option<BTreeSet<_>>>()?;
    let name = names.pop_first().filter(|name| names.is_empty() && looks_filename(name))?;
    Some(name.chars().take(4096).collect())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub id: String,
    pub variant_id: Option<String>,
    pub name: Option<String>,
    pub size_bytes: Option<u64>,
    pub source_url: String,
    pub revision: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub source_record_id: String,
    pub product_id: Option<String>,
    pub order_id: Option<String>,
    pub name: Option<String>,
    pub shop: Option<String>,
    pub shop_url: Option<String>,
    pub preview_url: Option<String>,
    pub acquired_date_text: Option<String>,
    pub paid_amount_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_line_total_jpy: Option<u64>,
    pub variant_name: Option<String>,
    pub files: Vec<File>,
    pub manifest_complete: bool,
    pub unavailable: bool,
    pub error_code: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub source: String,
    pub route: String,
    pub rows: Vec<Row>,
    pub next: Vec<String>,
    pub details: Vec<String>,
    pub terminal: bool,
    pub unresolved: usize,
    pub fingerprint: String,
    pub is_detail: bool,
    #[serde(default)]
    pub ordering: Ordering,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) free_item: Option<super::free_item::Evidence>,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Ordering {
    #[default]
    Unknown,
    AcquiredNewestFirst,
    DownloadedNewestFirst,
}
fn ordering(doc: &Html, source: &str, heading: &str, is_detail: bool) -> Ordering {
    if is_detail
        || doc
            .select(&sel(
                "input[type='search'][value],input[name='q'][value],input[name='keyword'][value]",
            ))
            .any(|e| {
                e.value()
                    .attr("value")
                    .is_some_and(|v| !v.trim().is_empty())
            })
    {
        return Ordering::Unknown;
    }
    // BOOTH announcements 970 and 945 are the provider's ordering contract.
    // An offered sort option or a product title is not a selected sort witness.
    if source == "library" {
        let active: Vec<_> = doc.select(&sel("select option[selected],button[aria-pressed='true'],[role='option'][aria-selected='true'],[role='tab'][aria-selected='true']")).map(text).collect();
        if active.iter().any(|s| s == "入手が新しい順")
            && !active.iter().any(|s| s == "ファイル更新が新しい順")
        {
            return Ordering::AcquiredNewestFirst;
        }
    } else if source == "free"
        && (heading.contains("無料ダウンロード")
            || heading.contains("Free downloads")
            || heading.contains("Free Downloads"))
    {
        return Ordering::DownloadedNewestFirst;
    }
    Ordering::Unknown
}
fn files(card: ElementRef<'_>, receipt: bool) -> Vec<File> {
    let mut result = BTreeMap::new();
    let controls = downloads(card, receipt);
    for (button, id, variant, route) in controls.iter().cloned() {
        let key = format!("{id}:{}", variant.as_deref().unwrap_or(""));
        if result.contains_key(&key) {
            continue;
        }
        // A track row's text is a title; only its format menu can name the file.
        let track = controls.iter().find(|(node, i, v, _)| *i == id && *v == variant && is_track(*node)).map(|(node, ..)| *node);
        let mut name = track.and_then(track_filename);
        let mut size_bytes = None;
        for scope in button.ancestors().filter_map(ElementRef::wrap).filter(|_| track.is_none()) {
            if !card.descendants().any(|n| n.id() == scope.id()) && scope.id() != card.id() {
                break;
            }
            let keys: BTreeSet<_> = controls
                .iter()
                .filter(|(node, _, _, _)| scope.id() == node.id() || scope.descendants().any(|child| child.id() == node.id()))
                .map(|(_, i, v, _)| (i, v))
                .collect();
            if keys.len() != 1 {
                break;
            }
            let mut names: Vec<_> = scope.select(&sel("[data-test='filename']")).collect();
            if names.is_empty() {
                names = scope
                    .select(&sel(".min-w-0.break-words.whitespace-pre-line .text-14"))
                    .collect();
            }
            if names.is_empty() && scope.value().classes().any(|c| c == "legacy-list-item") {
                names = scope
                    .select(&sel(".min-w-0 > b"))
                    .filter(|n| {
                        n.select(&sel("a,button,input,textarea,select"))
                            .next()
                            .is_none()
                            && looks_filename(&text(*n))
                    })
                    .collect();
            }
            if names.len() == 1 {
                name = Some(
                    names[0]
                        .text()
                        .collect::<String>()
                        .trim()
                        .chars()
                        .take(4096)
                        .collect::<String>(),
                )
                .filter(|n| !n.is_empty());
                size_bytes = scope
                    .select(&sel("[data-size-bytes]"))
                    .next()
                    .and_then(|n| n.value().attr("data-size-bytes"))
                    .and_then(|v| v.parse().ok());
                break;
            }
            if names.len() > 1 || scope.id() == card.id() {
                break;
            }
        }
        if name.is_none() {
            name = button
                .value()
                .attr("data-label")
                .filter(|s| looks_filename(s))
                .map(|s| s.chars().take(4096).collect());
        }
        let revision = button
            .value()
            .attr("data-sha256")
            .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
            .map(|v| format!("sha256:{}", v.to_ascii_lowercase()))
            .or_else(|| {
                button
                    .value()
                    .attr("data-revision")
                    .filter(|v| {
                        !v.is_empty()
                            && v.len() <= 100
                            && v.bytes().all(|c| {
                                c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')
                            })
                    })
                    .map(|v| format!("source:{v}"))
            })
            .unwrap_or_else(|| "unknown".into());
        result.insert(
            key,
            File {
                id,
                variant_id: variant,
                name,
                size_bytes,
                source_url: route,
                revision,
            },
        );
    }
    result.into_values().collect()
}
fn looks_filename(value: &str) -> bool {
    value.trim().rsplit_once('.').is_some_and(|(_, ext)| {
        !ext.is_empty() && ext.len() <= 12 && ext.bytes().all(|c| c.is_ascii_alphanumeric())
    })
}
fn unavailable(card: ElementRef<'_>) -> bool {
    card.select(&sel("[data-test='item-unavailable'],.item-unavailable"))
        .next()
        .is_some()
        || card
            .descendants()
            .filter(|n| {
                !n.ancestors()
                    .filter_map(ElementRef::wrap)
                    .any(|e| matches!(e.value().name(), "script" | "style"))
            })
            .filter_map(|n| n.value().as_text())
            .filter(|t| t.len() <= 540)
            .any(|t| {
                matches!(
                    t.trim(),
                    "この商品は非公開です"
                        | "この商品は非公開になりました"
                        | "商品が非公開になりました"
                        | "非公開の商品"
                        | "非公開"
                        | "非公開中"
                        | "公開停止中"
                        | "This item is private"
                        | "商品は削除されました"
                        | "この商品は削除されました"
                        | "商品が削除されました"
                        | "削除済みの商品"
                        | "Deleted item"
                        | "商品が見つかりません"
                        | "この商品は削除または非公開となっています"
                        | "この商品は削除されたか、非公開になっています"
                        | "非公開または削除された商品"
                        | "Item unavailable"
                        | "This item is unavailable"
                        | "この商品は販売を終了しました"
                        | "販売終了の商品"
                        | "商品の公開は終了しました"
                        | "This item is no longer available"
                        | "This item has been deleted"
                )
            })
}
fn numeric_identity_present(card: ElementRef<'_>) -> bool {
    std::iter::once(card)
        .chain(card.select(&sel(
            "[data-item-id],[data-product-id],[data-id],[data-item],[data-product]",
        )))
        .any(|e| {
            [
                "data-item-id",
                "data-product-id",
                "data-id",
                "data-item",
                "data-product",
            ]
            .iter()
            .any(|a| e.value().attr(a).is_some_and(safety::numeric))
        })
}
pub fn parse(html: &str, source: &str, route: &str) -> Result<Page> {
    parse_internal(html, source, route, None, None)
}
pub(super) fn diagnostic_digital_file_keys(
    row: &Row,
) -> Option<BTreeSet<(String, Option<String>)>> {
    if !row.manifest_complete {
        return None;
    }
    product_field_file_keys(row)
}
/// Digital controls bind financial fields independently of all-files readiness.
/// Missing filenames remain an incomplete manifest, not an unknown purchase.
pub(super) fn product_field_file_keys(row: &Row) -> Option<BTreeSet<(String, Option<String>)>> {
    if row.unavailable
        || row
            .error_code
            .as_deref()
            .is_some_and(|code| code != "PRODUCT_TITLE_UNRESOLVED")
        || !row.product_id.as_deref().is_some_and(safety::numeric)
        || row.files.is_empty()
    {
        return None;
    }
    let mut keys = BTreeSet::new();
    for file in &row.files {
        let (id, variant, route) = safety::resolver(&file.source_url)?;
        if id != file.id
            || variant != file.variant_id
            || route != file.source_url
            || !keys.insert((id, variant))
        {
            return None;
        }
    }
    Some(keys)
}
fn plain_leaf(node: ElementRef<'_>, limit: usize) -> Option<String> {
    if node
        .children()
        .filter_map(ElementRef::wrap)
        .next()
        .is_some()
        || matches!(
            node.value().name(),
            "script" | "style" | "a" | "button" | "input" | "textarea" | "select" | "form"
        )
    {
        return None;
    }
    let value = node.text().collect::<String>();
    (value.chars().count() <= limit && !value.chars().any(|c| c.is_control() && !c.is_whitespace()))
        .then(|| value.trim().to_owned())
}
fn bounded_leaf_value(node: ElementRef<'_>) -> Option<String> {
    if let Some(value) = plain_leaf(node, 128) {
        return Some(value);
    }
    if node
        .select(&sel("a,button,input,textarea,select,form,script,style"))
        .next()
        .is_some()
    {
        return None;
    }
    let leaves: Vec<_> = node
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter_map(|n| plain_leaf(n, 128))
        .filter(|text| !text.is_empty())
        .collect();
    (leaves.len() == 1).then(|| leaves[0].clone())
}
pub(super) fn slash_date_shape(value: &str) -> bool {
    value.len() <= 32
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'/' | b':' | b' '))
        && (chrono::NaiveDate::parse_from_str(value, "%Y/%m/%d").is_ok()
            || ["%Y/%m/%d %H:%M", "%Y/%m/%d %H:%M:%S"]
                .iter()
                .any(|format| chrono::NaiveDateTime::parse_from_str(value, format).is_ok()))
}
fn noninteractive(node: ElementRef<'_>) -> bool {
    std::iter::once(node)
        .chain(node.ancestors().filter_map(ElementRef::wrap))
        .all(|n| {
            !matches!(
                n.value().name(),
                "a" | "time"
                    | "form"
                    | "button"
                    | "input"
                    | "textarea"
                    | "select"
                    | "script"
                    | "style"
            )
        })
}
fn currency_jpy(value: &str) -> Option<f64> {
    (value.contains(['¥', '￥', '円']) || value.trim_end().ends_with("JPY"))
        .then(|| super::metadata::parse_jpy_price(&serde_json::json!(value)))
        .flatten()
}
fn compact_quantity(value: &str) -> Option<u64> {
    let mut parts = value.split(['×', 'x']);
    currency_jpy(parts.next()?.trim())?;
    let quantity = parts.next()?.trim();
    if parts.next().is_some()
        || quantity.is_empty()
        || !quantity.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    quantity
        .parse::<u64>()
        .ok()
        .filter(|quantity| *quantity > 0 && *quantity <= 9_007_199_254_740_991)
}
fn rebound_digital_sheet(card: ElementRef<'_>, proof_rows: &[Row]) -> bool {
    if omitted_notice_scope(card) {
        return false;
    }
    let ids: BTreeSet<_> = links(card).into_iter().filter_map(identity).collect();
    let controls = downloads(card, true);
    let raw = raw_download_controls(card, true);
    if ids.len() != 1 || controls.is_empty() || raw != controls.len() {
        return false;
    }
    let id = ids.first().unwrap();
    let keys: BTreeSet<_> = controls
        .into_iter()
        .map(|(_, id, variant, _)| (id, variant))
        .collect();
    proof_rows.iter().any(|row| {
        row.product_id.as_ref() == Some(id)
            && diagnostic_digital_file_keys(row).as_ref() == Some(&keys)
    }) && card
        .select(&sel("[data-test='item-unavailable'],.item-unavailable"))
        .next()
        .is_none()
        && !std::iter::once(card)
            .chain(card.select(&sel("[data-product-id],[data-item-id]")))
            .any(|node| {
                ["data-product-id", "data-item-id"]
                    .iter()
                    .any(|key| node.value().attr(key).is_some_and(|value| value != id))
            })
}
fn rebound_product_fields(card: ElementRef<'_>, proof_rows: &[Row]) -> bool {
    // A recognized message prefix excludes its opaque body from product
    // binding. This does not validate closure or gate unrelated receipt UI.
    if omitted_notice_scope(card) {
        return false;
    }
    let ids: BTreeSet<_> = links(card).into_iter().filter_map(identity).collect();
    let controls = downloads(card, true);
    let raw = raw_download_controls(card, true);
    if ids.len() != 1 || controls.is_empty() || raw != controls.len() {
        return false;
    }
    let id = ids.first().unwrap();
    let keys: BTreeSet<_> = controls
        .into_iter()
        .map(|(_, id, variant, _)| (id, variant))
        .collect();
    proof_rows.iter().any(|row| {
        row.product_id.as_ref() == Some(id) && product_field_file_keys(row).as_ref() == Some(&keys)
    }) && !unavailable(card)
        && !std::iter::once(card)
            .chain(card.select(&sel("[data-product-id],[data-item-id]")))
            .any(|node| {
                ["data-product-id", "data-item-id"]
                    .iter()
                    .any(|key| node.value().attr(key).is_some_and(|value| value != id))
            })
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReceiptTemplateDiagnostics {
    version: usize,
    digital_proof_bound: bool,
    summary_candidates: usize,
    summary_order_number_text_nodes: usize,
    summary_ordered_date_text_nodes: usize,
    summary_role_candidates: usize,
    summary_receipt_number_matches: usize,
    summary_unique_date_pairs: usize,
    summary_slash_dates: usize,
    summary_iso_dates: usize,
    summary_ambiguous_pairs: usize,
    jpy_parents: usize,
    jpy_parent_direct_text_nodes: usize,
    jpy_parent_multiplication_tokens: usize,
    jpy_parent_quantity_nodes: usize,
    jpy_parent_compact_syntax_rows: usize,
    jpy_parent_same_product_line_rows: usize,
    price_scope_boost_text_nodes: usize,
    price_scope_explicit_paid_text_nodes: usize,
    price_scope_subtotal_text_nodes: usize,
    normalized_title_line_witnesses: usize,
}
fn template_literal(value: &str) -> &str {
    value.trim().trim_end_matches([':', '：']).trim()
}
fn order_number_label(value: &str) -> bool {
    matches!(
        template_literal(value),
        "注文番号" | "ご注文番号" | "注文ID" | "Order number" | "Order Number" | "Order ID"
    )
}
fn ordered_date_label(value: &str) -> bool {
    matches!(
        template_literal(value),
        "注文日時" | "ご注文日時" | "注文日" | "ご注文日" | "Ordered at" | "Order date" | "Created At"
    )
}
fn template_label_value<'a>(
    mut label: ElementRef<'a>,
    scope: ElementRef<'a>,
    literal: &str,
) -> Option<ElementRef<'a>> {
    let mut paired = None;
    for _ in 0..=2 {
        if label.id() == scope.id()
            || !scope.descendants().any(|n| n.id() == label.id())
            || label
                .select(&sel("a,form,button,input,textarea,select,script,style"))
                .next()
                .is_some()
        {
            break;
        }
        let parts: Vec<_> = label
            .text()
            .filter(|part| !part.trim().is_empty())
            .collect();
        if parts.len() != 1 || template_literal(parts[0]) != literal {
            break;
        }
        if let Some(next) = label.next_siblings().filter_map(ElementRef::wrap).next() {
            if scope.descendants().any(|node|node.id()==next.id()) && std::iter::once(next).chain(next.ancestors().filter_map(ElementRef::wrap)).all(|node|!matches!(node.value().name(),"a"|"form"|"button"|"input"|"textarea"|"select"|"script"|"style"))
                && links(next).is_empty() && next.select(&sel("form,button,input,textarea,select,script,style,[data-product-id],[data-item-id]")).next().is_none(){paired=Some(next);}
        }
        let Some(parent) = label.parent().and_then(ElementRef::wrap) else {
            break;
        };
        label = parent;
    }
    paired
}
fn template_summary_candidate(candidate: ElementRef<'_>) -> bool {
    if omitted_notice_scope(candidate) {
        return false;
    }
    !matches!(candidate.value().name(),"main"|"body"|"html") && candidate.descendants().count()<=256
        && noninteractive(candidate)
        && std::iter::once(candidate).chain(candidate.select(&sel("[data-product-id],[data-item-id]"))).all(|node|node.value().attr("data-product-id").is_none() && node.value().attr("data-item-id").is_none())
        && candidate.select(&sel("form,a,button,input,textarea,select,script,style,[data-test='downloadable'],[data-href*='/downloadables/']")).next().is_none()
        && candidate.select(&sel(".sheet")).filter(|node|node.select(&sel(".sheet")).next().is_none()).count()<=1
}
// Private observations are never serialized or included in diagnostics.
#[derive(Default)]
struct ReceiptSummaryObservation {
    number_labels: usize,
    date_labels: usize,
    role: bool,
    number_matches: bool,
    date: Option<String>,
    slash: bool,
    iso: bool,
    ambiguous: bool,
}
fn receipt_summary_observation(
    candidate: ElementRef<'_>,
    receipt_id: &str,
) -> ReceiptSummaryObservation {
    let mut result = ReceiptSummaryObservation::default();
    if !safety::numeric(receipt_id) || !template_summary_candidate(candidate) {
        return result;
    }
    let mut numbers = Vec::new();
    let mut ordered = Vec::new();
    for node in candidate.descendants() {
        let Some(raw) = node
            .value()
            .as_text()
            .filter(|raw| raw.chars().count() <= 40)
        else {
            continue;
        };
        let Some(label) = node
            .parent()
            .and_then(ElementRef::wrap)
            .filter(|parent| noninteractive(*parent))
        else {
            continue;
        };
        if order_number_label(raw) {
            numbers.push((label, template_literal(raw)));
        }
        if ordered_date_label(raw) {
            ordered.push((label, template_literal(raw)));
        }
    }
    result.number_labels = numbers.len();
    result.date_labels = ordered.len();
    if numbers.len() != 1 || ordered.len() != 1 {
        result.ambiguous = !numbers.is_empty() || !ordered.is_empty();
        return result;
    }
    result.role = true;
    let number =
        template_label_value(numbers[0].0, candidate, numbers[0].1).and_then(bounded_leaf_value);
    result.number_matches = number
        .as_deref()
        .map(|number| {
            number
                .trim()
                .strip_prefix('#')
                .unwrap_or(number.trim())
                .trim()
        })
        .is_some_and(|number| safety::numeric(number) && number == receipt_id);
    // Do not read a date value until the bounded summary proves this receipt.
    if !result.number_matches {
        result.ambiguous = true;
        return result;
    }
    let Some(value) =
        template_label_value(ordered[0].0, candidate, ordered[0].1).and_then(bounded_leaf_value)
    else {
        result.ambiguous = true;
        return result;
    };
    result.slash = slash_date_shape(&value);
    result.iso = chrono::DateTime::parse_from_rfc3339(&value).is_ok()
        || chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d").is_ok();
    if result.slash || result.iso {
        result.date = Some(value);
    } else {
        result.ambiguous = true;
    }
    result
}
fn receipt_summary_candidates<'a>(
    cards: &[ElementRef<'a>],
    digital: &[ElementRef<'a>],
) -> Vec<ElementRef<'a>> {
    let mut summaries = cards
        .iter()
        .copied()
        .filter(|candidate| template_summary_candidate(*candidate))
        .collect::<Vec<_>>();
    for card in digital {
        let mut current = Some(*card);
        for _ in 0..4 {
            let Some(node) = current else {
                break;
            };
            if matches!(node.value().name(), "main" | "body" | "html") {
                break;
            }
            current = node.parent().and_then(ElementRef::wrap);
            if let Some(candidate) = node.prev_siblings().filter_map(ElementRef::wrap).next() {
                if template_summary_candidate(candidate)
                    && !summaries.iter().any(|scope| scope.id() == candidate.id())
                {
                    summaries.push(candidate);
                }
            }
        }
    }
    summaries
}
fn receipt_acquired_date<'a>(
    cards: &[ElementRef<'a>],
    digital: ElementRef<'a>,
    receipt_id: &str,
) -> Option<String> {
    let mut dates = BTreeSet::new();
    for candidate in receipt_summary_candidates(cards, &[digital]) {
        let observation = receipt_summary_observation(candidate, receipt_id);
        if observation.ambiguous {
            return None;
        }
        if let Some(date) = observation.date {
            dates.insert(date);
        }
    }
    (dates.len() == 1).then(|| dates.into_iter().next().unwrap())
}
fn normalized_product_lines<'a>(
    price_parent: ElementRef<'a>,
    card: ElementRef<'a>,
    proof_rows: &[Row],
) -> Vec<ElementRef<'a>> {
    let mut result = Vec::new();
    for scope in std::iter::once(price_parent)
        .chain(price_parent.ancestors().filter_map(ElementRef::wrap))
        .take(7)
    {
        if scope.id() == card.id() || !card.descendants().any(|node| node.id() == scope.id()) {
            break;
        }
        if scope.descendants().count()>128 || !noninteractive(scope)
            || scope.select(&sel("form,button,input,textarea,select,script,style,[data-test='downloadable'],a[href*='/downloadables/'],[data-href*='/downloadables/']")).next().is_some(){continue;}
        let anchors = links(scope);
        let ids: BTreeSet<_> = anchors.iter().copied().filter_map(identity).collect();
        for row in proof_rows
            .iter()
            .filter(|row| product_field_file_keys(row).is_some())
        {
            let Some(id) = row.product_id.as_deref() else {
                continue;
            };
            let title = row.name.as_deref().unwrap_or("");
            if ids.len() != 1 || ids.first().map(String::as_str) != Some(id) {
                continue;
            }
            let known_links = anchors.iter().copied().all(|anchor| {
                identity(anchor).as_deref() == Some(id)
                    || anchor
                        .value()
                        .attr("href")
                        .and_then(absolute)
                        .and_then(|url| safety::public_url(&url, "shop"))
                        .is_some()
            });
            if known_links
                && anchors.iter().copied().any(|anchor| {
                    identity(anchor).as_deref() == Some(id) && text(anchor) == safety::short(title)
                })
            {
                result.push(scope);
                break;
            }
        }
    }
    result
}
fn template_dom_pieces(parent: ElementRef<'_>) -> Option<Vec<String>> {
    use unicode_normalization::UnicodeNormalization;
    if !noninteractive(parent) || parent.descendants().count() > 128 {
        return None;
    }
    let mut parts = Vec::new();
    for node in parent.children() {
        let value = if let Some(text) = node.value().as_text() {
            if text.chars().count() > 64 {
                return None;
            }
            text.trim().to_owned()
        } else if let Some(element) = ElementRef::wrap(node) {
            if !matches!(
                element.value().name(),
                "div" | "span" | "b" | "strong" | "small" | "p" | "td" | "label"
            ) {
                return None;
            }
            bounded_leaf_value(element)?
        } else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        parts.push(value.nfkc().collect::<String>().trim().to_owned());
    }
    (parts.len() <= 3).then_some(parts)
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PaidLabel {
    Boost,
    Paid,
    Subtotal,
    Unit,
    Generic,
}
fn paid_label(value: &str) -> Option<PaidLabel> {
    match template_literal(value) {
        "BOOST" | "BOOST↑" | "BOOST金額" | "ブースト" => Some(PaidLabel::Boost),
        "支払金額" | "支払額" | "Paid amount" => Some(PaidLabel::Paid),
        "小計" | "Subtotal" => Some(PaidLabel::Subtotal),
        "単価" | "Unit price" => Some(PaidLabel::Unit),
        "価格" | "金額" | "商品価格" | "Price" | "Amount" | "Item price" => {
            Some(PaidLabel::Generic)
        }
        _ => None,
    }
}
// Read all direct DOM pieces, including text next to wrappers. A wrapper with a
// recognized leaf plus unknown direct text must never silently lose that text.
fn paid_dom_pieces(node: ElementRef<'_>) -> Option<Vec<String>> {
    use unicode_normalization::UnicodeNormalization;
    fn collect(node: ElementRef<'_>, depth: usize, result: &mut Vec<String>) -> Option<()> {
        if depth > 3
            || !matches!(
                node.value().name(),
                "div" | "span" | "b" | "strong" | "small" | "p" | "td" | "dd" | "label"
            )
        {
            return None;
        }
        for child in node.children() {
            if let Some(raw) = child.value().as_text() {
                if raw.chars().count() > 64
                    || raw.chars().any(|c| c.is_control() && !c.is_whitespace())
                {
                    return None;
                }
                let value = raw.nfkc().collect::<String>().trim().to_owned();
                if !value.is_empty() {
                    result.push(value);
                }
            } else if let Some(element) = ElementRef::wrap(child) {
                collect(element, depth + 1, result)?;
            }
            if result.len() > 8 {
                return None;
            }
        }
        Some(())
    }
    if !noninteractive(node) || node.descendants().count() > 128 {
        return None;
    }
    let mut result = Vec::new();
    collect(node, 0, &mut result)?;
    (result.iter().map(String::len).sum::<usize>() <= 128).then_some(result)
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProductLineFieldDiagnostics {
    version: usize,
    saved_receipt_witness: bool,
    digital_bound_items: usize,
    unique_metadata_items: usize,
    unit_price_items: usize,
    explicit_quantity_items: usize,
    omitted_quantity_items: usize,
    explicit_boost_items: usize,
    product_line_proof_items: usize,
    ambiguous_items: usize,
    digital_control_binding_unproven_items: usize,
    metadata_shape_unrecognized_items: usize,
    unit_price_unrecognized_items: usize,
    quantity_unrecognized_items: usize,
    boost_unrecognized_items: usize,
}
// Only the needed product fields participate; unrelated receipt UI has no role.
struct ProductLineProof {
    diagnostics: ProductLineFieldDiagnostics,
    charges: Vec<ReceiptCharge>,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReceiptNoticeDiagnostics {
    version: usize,
    notice_header_matches: usize,
    closed_notices: usize,
    structural_guard_rejected: usize,
    rival_presence_rejected: usize,
    outer_wrapping_rejected: usize,
    sibling_structure_rejected: usize,
    header_placement_rejected: usize,
    body_role_rejected: usize,
    body_type_rejected: usize,
    body_depth_rejected: usize,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ReceiptContactDiagnostics {
    version: usize,
    contact_action_matches: usize,
    closed_contacts: usize,
    structural_guard_rejected: usize,
    rival_presence_rejected: usize,
    envelope_structure_rejected: usize,
    instruction_rejected: usize,
    opaque_body_structure_rejected: usize,
}
// This typed proof stays local; production rows receive only the exact bound amount.
struct ReceiptCharge {
    source_record_id: String,
    product_id: String,
    amount: u64,
}
fn apply_product_line_charges(rows: &mut [Row], proof: ProductLineProof, receipt_id: &str) {
    for charge in proof.charges {
        let matches: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.source_record_id == charge.source_record_id
                    && row.product_id.as_deref() == Some(charge.product_id.as_str())
                    && row.order_id.as_deref() == Some(receipt_id)
                    && product_field_file_keys(row).is_some()
            })
            .map(|(index, _)| index)
            .collect();
        if matches.len() == 1 && charge.amount <= 9_007_199_254_740_991 {
            rows[matches[0]].product_line_total_jpy = Some(charge.amount);
        }
    }
}
struct ContactPrefix<'a> {
    instruction: ElementRef<'a>,
    action: ElementRef<'a>,
    body: ElementRef<'a>,
}
fn strict_contact_route(action: ElementRef<'_>) -> bool {
    // Only the positively nominated outside action supplies a URL. Other
    // anchors are rejected by presence without ever reading their href.
    let Some(link) = action
        .value()
        .attr("href")
        .and_then(absolute)
        .and_then(|value| url::Url::parse(&value).ok())
    else {
        return false;
    };
    let Some(segment) = link.path().strip_prefix("/conversations/") else {
        return false;
    };
    safety::secure(&link)
        && link.host_str() == Some("accounts.booth.pm")
        && link.fragment().is_none()
        && !segment.is_empty()
        && segment.len() <= 128
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
fn known_contact_prefix(card: ElementRef<'_>) -> Option<ContactPrefix<'_>> {
    if !sel(".sheet").matches(&card) || card.select(&sel(".sheet")).next().is_some() {
        return None;
    }
    let mut node = card;
    for _ in 0..256 {
        if sel("div.text-14").matches(&node) {
            return None;
        }
        let children: Vec<_> = node.children().filter_map(ElementRef::wrap).collect();
        if children
            .first()
            .is_some_and(|child| child.value().name() == "p")
        {
            let instruction = children[0];
            let action = *children
                .get(1)
                .filter(|child| child.value().name() == "a")?;
            let body = *children.get(2)?;
            if instruction
                .children()
                .filter_map(ElementRef::wrap)
                .next()
                .is_some()
                || action
                    .children()
                    .filter_map(ElementRef::wrap)
                    .next()
                    .is_some()
            {
                return None;
            }
            let label = action.text().collect::<String>();
            if label.chars().count() > 40
                || label.trim() != "ショップに連絡する"
                || !strict_contact_route(action)
            {
                return None;
            }
            // The first P and next known action establish the outside prefix;
            // the following body is retained only as a node boundary.
            return Some(ContactPrefix {
                instruction,
                action,
                body,
            });
        }
        if children.len() != 1 {
            return None;
        }
        node = children[0];
    }
    None
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ContactEnvelopePhase {
    StructuralGuard,
    RivalPresence,
    EnvelopeStructure,
    Instruction,
    OpaqueBodyStructure,
    Closed,
}
fn shop_contact_envelope_phase(
    card: ElementRef<'_>,
    prefix: &ContactPrefix<'_>,
) -> ContactEnvelopePhase {
    if !sel(".sheet").matches(&card) || card.descendants().count() > 48 {
        return ContactEnvelopePhase::StructuralGuard;
    }
    let rivals = sel("img,form,input,textarea,select,button,script,style,iframe,table,time,.particulars-heading,.icon-boost,pixiv-icon[name='24/XTwitter'],[onclick],[role='button'],[data-id],[data-item],[data-product],[data-product-id],[data-item-id],[data-test='item-name'],[data-test='item-thumbnail'],[data-test='item-unavailable'],[data-test='filename'],[data-test='downloadable'],[data-test='downloadable-list'],[data-test='paid-amount'],[data-test='quantity'],[data-test='ordered-at'],[data-test='acquired-at'],[data-test='downloaded-at'],.item-title,.item-name,.item-thumbnail,.item-unavailable,.l-library-item-thumbnail,.list.list--collapse,.legacy-list-item");
    if card
        .select(&sel("a,[href],[data-href]"))
        .any(|node| node.id() != prefix.action.id())
        || prefix.action.value().attr("data-href").is_some()
        || rivals.matches(&card)
        || card.select(&rivals).next().is_some()
        || card.select(&sel(".sheet")).next().is_some()
        || std::iter::once(card)
            .chain(card.descendants().filter_map(ElementRef::wrap))
            .any(|node| {
                financial_attribute(node)
                    || [
                        "data-id",
                        "data-item",
                        "data-product",
                        "data-product-id",
                        "data-item-id",
                    ]
                    .iter()
                    .any(|key| node.value().attr(key).is_some())
                    || (node.id() != prefix.action.id()
                        && ["href", "data-href"]
                            .iter()
                            .any(|key| node.value().attr(key).is_some()))
            })
    {
        return ContactEnvelopePhase::RivalPresence;
    }
    let outer: Vec<_> = card.children().filter_map(ElementRef::wrap).collect();
    if outer.len() != 1 || outer[0].value().name() != "div" {
        return ContactEnvelopePhase::EnvelopeStructure;
    }
    let children: Vec<_> = outer[0].children().filter_map(ElementRef::wrap).collect();
    if children.len() != 3
        || children[0].id() != prefix.instruction.id()
        || children[1].id() != prefix.action.id()
        || children[2].id() != prefix.body.id()
    {
        return ContactEnvelopePhase::EnvelopeStructure;
    }
    if prefix.body.value().name() != "div" {
        return ContactEnvelopePhase::OpaqueBodyStructure;
    }
    if !prefix.body.children().all(|node| {
        matches!(
            node.value(),
            scraper::Node::Text(_) | scraper::Node::Comment(_)
        )
    }) {
        return ContactEnvelopePhase::OpaqueBodyStructure;
    }
    if plain_leaf(prefix.instruction, 40).as_deref() != Some("お支払いが完了しました") {
        return ContactEnvelopePhase::Instruction;
    }
    // The opaque body is positively bounded above. Only outside wrapper
    // whitespace is checked; no body text value, length or trim is accessed.
    if !card
        .children()
        .filter_map(|node| node.value().as_text())
        .all(|raw| raw.trim().is_empty())
        || !outer[0]
            .children()
            .filter_map(|node| node.value().as_text())
            .all(|raw| raw.trim().is_empty())
    {
        return ContactEnvelopePhase::EnvelopeStructure;
    }
    ContactEnvelopePhase::Closed
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoticeEnvelopePhase {
    StructuralGuard,
    RivalPresence,
    OuterWrapping,
    SiblingStructure,
    HeaderPlacement,
    BodyRole,
    BodyType,
    BodyDepth,
    Closed,
}
struct NoticePrefix<'a> {
    header: ElementRef<'a>,
    body: ElementRef<'a>,
    parent: ElementRef<'a>,
}
fn bounded_notice_heading(node: ElementRef<'_>) -> bool {
    if node.descendants().count() > 32 {
        return false;
    }
    std::iter::once(node)
        .chain(node.descendants().filter_map(ElementRef::wrap))
        .all(|part| {
            matches!(
                part.value().name(),
                "div" | "span" | "b" | "strong" | "h1" | "h2"
            ) && part.children().filter_map(ElementRef::wrap).count() <= 1
                && !financial_attribute(part)
                && !part.value().attrs().any(|(key, _)| {
                    key.starts_with("data-") || matches!(key, "id" | "href" | "onclick" | "role")
                })
        })
}
fn known_notice_prefix(card: ElementRef<'_>) -> Option<NoticePrefix<'_>> {
    if !card.value().classes().any(|class| class == "sheet")
        || card.select(&sel(".sheet")).next().is_some()
    {
        return None;
    }
    // First capture the header/following-DIV-or-P boundary, then compare only the
    // bounded inline heading. A mismatch never searches siblings or body.
    let mut node = card;
    for _ in 0..256 {
        // A known body-only wrapper cannot supply a heading from its contents.
        if node.value().name() == "div" && node.value().classes().any(|class| class == "text-14") {
            return None;
        }
        let children: Vec<_> = node.children().filter_map(ElementRef::wrap).collect();
        if children.len() >= 2 {
            let header = children[0];
            let body = children[1];
            if !matches!(body.value().name(), "div" | "p")
                || (body.value().name() == "p"
                    && (header.value().name() != "b"
                        || header
                            .children()
                            .filter_map(ElementRef::wrap)
                            .next()
                            .is_some()))
                || !bounded_notice_heading(header)
            {
                return None;
            }
            let literal = header.text().collect::<String>();
            return (literal.chars().count() <= 40 && literal.trim() == "発送通知コメント")
                .then_some(NoticePrefix {
                    header,
                    body,
                    parent: node,
                });
        }
        if children.len() != 1
            || node
                .children()
                .any(|child| matches!(child.value(), scraper::Node::Text(_)))
            || !matches!(children[0].value().name(), "div" | "span")
        {
            return None;
        }
        node = children[0];
    }
    None
}
fn known_notice_header(card: ElementRef<'_>) -> Option<ElementRef<'_>> {
    known_notice_prefix(card).map(|prefix| prefix.header)
}
fn provider_notice_envelope_phase(card: ElementRef<'_>) -> NoticeEnvelopePhase {
    // Known fixed message heading plus authored DIV body or the original
    // human-confirmed B/P shape. The message body is never read.
    if !card.value().classes().any(|class| class == "sheet") || card.descendants().count() > 128 {
        return NoticeEnvelopePhase::StructuralGuard;
    }
    let rivals = sel("a,[href],[data-href],button,input,textarea,select,form,script,style,iframe,table,img,time,.particulars-heading,.icon-boost,pixiv-icon[name='24/XTwitter'],[onclick],[role='button'],[data-id],[data-item],[data-product],[data-product-id],[data-item-id],[data-test='item-name'],[data-test='item-thumbnail'],[data-test='item-unavailable'],[data-test='filename'],[data-test='downloadable'],[data-test='downloadable-list'],.item-title,.item-name,.item-thumbnail,.item-unavailable,.l-library-item-thumbnail,.list.list--collapse,.legacy-list-item");
    if rivals.matches(&card)
        || card.select(&rivals).next().is_some()
        || card.select(&sel(".sheet")).next().is_some()
        || std::iter::once(card)
            .chain(card.descendants().filter_map(ElementRef::wrap))
            .any(|node| {
                financial_attribute(node)
                    || [
                        "data-id",
                        "data-item",
                        "data-product",
                        "data-product-id",
                        "data-item-id",
                    ]
                    .iter()
                    .any(|key| node.value().attr(key).is_some())
            })
    {
        return NoticeEnvelopePhase::RivalPresence;
    }
    let Some(prefix) = known_notice_prefix(card) else {
        return NoticeEnvelopePhase::HeaderPlacement;
    };
    let children: Vec<_> = prefix
        .parent
        .children()
        .filter_map(ElementRef::wrap)
        .collect();
    if children.len() != 2 {
        return NoticeEnvelopePhase::SiblingStructure;
    }
    if children[0].id() != prefix.header.id() {
        return NoticeEnvelopePhase::HeaderPlacement;
    }
    if children[1].id() != prefix.body.id()
        || !matches!(prefix.body.value().name(), "div" | "p")
        || (prefix.body.value().name() == "p"
            && prefix
                .body
                .descendants()
                .filter_map(ElementRef::wrap)
                .filter(|node| node.value().name() == "br")
                .count()
                != 3)
    {
        return NoticeEnvelopePhase::BodyRole;
    }
    let mut wrappers = Vec::new();
    let mut wrapper = card;
    while wrapper.id() != prefix.parent.id() {
        wrappers.push(wrapper);
        let children: Vec<_> = wrapper.children().filter_map(ElementRef::wrap).collect();
        if children.len() != 1 || !matches!(children[0].value().name(), "div" | "span") {
            return NoticeEnvelopePhase::OuterWrapping;
        }
        wrapper = children[0];
    }
    if !matches!(prefix.parent.value().name(), "div" | "span") {
        return NoticeEnvelopePhase::OuterWrapping;
    }
    // Body node types and depth only: no text(), text-content checks,
    // serialization, HTML, class names or values from the opaque body.
    if !children[1]
        .descendants()
        .filter_map(ElementRef::wrap)
        .all(|node| matches!(node.value().name(), "div" | "span" | "p" | "br"))
    {
        return NoticeEnvelopePhase::BodyType;
    }
    if !children[1]
        .descendants()
        .filter_map(ElementRef::wrap)
        .all(|node| {
            node.id() == children[1].id()
                || node
                    .ancestors()
                    .take_while(|ancestor| ancestor.id() != children[1].id())
                    .count()
                    <= 6
        })
    {
        return NoticeEnvelopePhase::BodyDepth;
    }
    // The body boundary is now positively identified. Whitespace checks are
    // confined to its outside wrappers, never to body descendants.
    if wrappers.iter().any(|wrapper| {
        wrapper
            .children()
            .any(|node| matches!(node.value(), scraper::Node::Text(_)))
    }) {
        return NoticeEnvelopePhase::OuterWrapping;
    }
    if !prefix
        .parent
        .children()
        .filter_map(|node| node.value().as_text())
        .all(|raw| raw.trim().is_empty())
    {
        return NoticeEnvelopePhase::SiblingStructure;
    }
    NoticeEnvelopePhase::Closed
}
fn strict_whole_jpy(value: &str, currency_required: bool) -> Option<u64> {
    let amount = if currency_required {
        currency_jpy(value)?
    } else {
        super::metadata::parse_jpy_price(&serde_json::json!(value))?
    };
    Some(amount as u64)
}
fn receipt_boost_amount(node: ElementRef<'_>) -> Option<u64> {
    use unicode_normalization::UnicodeNormalization;
    fn collect(
        node: ElementRef<'_>,
        depth: usize,
        parts: &mut Vec<String>,
        icons: &mut usize,
    ) -> Option<()> {
        if depth > 3 || !noninteractive(node) {
            return None;
        }
        if node.value().classes().any(|class| class == "icon-boost") {
            if !matches!(node.value().name(), "i" | "span" | "pixiv-icon" | "svg") {
                return None;
            }
            let value = node.text().collect::<String>();
            if !value.trim().is_empty() && paid_label(&value) != Some(PaidLabel::Boost) {
                return None;
            }
            *icons += 1;
            return Some(());
        }
        if !matches!(
            node.value().name(),
            "div" | "span" | "b" | "strong" | "small" | "p" | "label"
        ) {
            return None;
        }
        for child in node.children() {
            if let Some(raw) = child.value().as_text() {
                if raw.chars().count() > 64
                    || raw.chars().any(|c| c.is_control() && !c.is_whitespace())
                {
                    return None;
                }
                let value = raw.nfkc().collect::<String>().trim().to_owned();
                if !value.is_empty() {
                    parts.push(value);
                }
            } else if let Some(element) = ElementRef::wrap(child) {
                collect(element, depth + 1, parts, icons)?;
            }
            if parts.len() > 8 || *icons > 1 {
                return None;
            }
        }
        Some(())
    }
    if node.descendants().count() > 128 {
        return None;
    }
    let mut parts = Vec::new();
    let mut icons = 0;
    collect(node, 0, &mut parts, &mut icons)?;
    let mut value = parts.join(" ");
    let mut labels = 0;
    for literal in ["BOOST↑", "BOOST金額", "BOOST", "ブースト"] {
        if let Some(rest) = value.strip_prefix(literal).filter(|rest| {
            rest.is_empty()
                || rest.starts_with(char::is_whitespace)
                || rest.starts_with([':', '：'])
        }) {
            value = rest
                .trim()
                .trim_start_matches([':', '：'])
                .trim()
                .to_owned();
            labels += 1;
            break;
        }
        if let Some(rest) = value
            .strip_suffix(literal)
            .filter(|rest| rest.ends_with(char::is_whitespace))
        {
            value = rest.trim().to_owned();
            labels += 1;
            break;
        }
    }
    if icons > 1 || labels > 1 || (icons == 0 && labels != 1) || value.len() > 128 {
        return None;
    }
    strict_whole_jpy(&value, false)
}
fn receipt_quantity(node: ElementRef<'_>) -> Option<u64> {
    let parts = paid_dom_pieces(node)?;
    let joined = parts.join(" ");
    let mut value = joined.trim();
    for label in ["数量", "個数", "Quantity", "×", "x"] {
        if let Some(rest) = value.strip_prefix(label) {
            value = rest.trim().trim_start_matches([':', '：']).trim();
            break;
        }
    }
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|quantity| *quantity > 0 && *quantity <= 9_007_199_254_740_991)
}
fn receipt_item_metadata<'a>(card: ElementRef<'a>, row: &Row) -> Option<Vec<ElementRef<'a>>> {
    let id = row.product_id.as_deref()?;
    let title = row.name.as_deref().unwrap_or("");
    let mut metadata = Vec::new();
    for anchor in card.select(&sel("b a[href]")) {
        if identity(anchor).as_deref() != Some(id) || text(anchor) != safety::short(title) {
            continue;
        }
        let Some(title_div) = anchor
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|node| node.value().name() == "div")
        else {
            continue;
        };
        let Some(scope) = title_div.parent().and_then(ElementRef::wrap) else {
            continue;
        };
        if scope.id() == card.id()
            || scope.value().name() != "div"
            || !card.descendants().any(|node| node.id() == scope.id())
            || !normalized_product_lines(scope, card, std::slice::from_ref(row))
                .iter()
                .any(|line| line.id() == scope.id())
        {
            continue;
        }
        let children: Vec<_> = scope.children().filter_map(ElementRef::wrap).collect();
        if !(children.len() == 3 || children.len() == 4)
            || children.iter().any(|child| child.value().name() != "div")
            || children[0].id() != title_div.id()
            || !scope
                .children()
                .filter_map(|node| node.value().as_text())
                .all(|raw| raw.trim().is_empty())
        {
            continue;
        }
        if !metadata
            .iter()
            .any(|(seen, _): &(ElementRef<'_>, Vec<ElementRef<'_>>)| seen.id() == scope.id())
        {
            metadata.push((scope, children));
        }
    }
    (metadata.len() == 1).then(|| metadata.remove(0).1)
}
fn product_line_proof(html: &str, rows: &[Row], receipt_id: &str) -> Result<ProductLineProof> {
    if !safety::numeric(receipt_id) {
        bail!("CHECKPOINT_INVALID");
    }
    let doc = Html::parse_document(html);
    product_line_proof_document(&doc, rows, receipt_id)
}
fn product_line_proof_document(
    doc: &Html,
    rows: &[Row],
    receipt_id: &str,
) -> Result<ProductLineProof> {
    if doc
        .select(&sel(
            "input[type='password'],form[action*='sign_in'],form[action*='login']",
        ))
        .next()
        .is_some()
    {
        bail!("AUTH_REQUIRED");
    }
    if doc
        .select(&sel("#challenge-form,.cf-challenge,iframe[src*='captcha']"))
        .next()
        .is_some()
    {
        bail!("AUTH_CHALLENGE");
    }
    let main = doc
        .select(&sel("main,[role='main']"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    // Unknown regions without file controls never enter product-field binding.
    let cards: Vec<_> = main.select(&sel(".sheet"))
        .filter(|card| card.select(&sel(".sheet")).next().is_none()

            && raw_download_controls(*card, true) != 0)
        .collect();
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.order_id.as_deref() == Some(receipt_id)
                && !row.source_record_id.is_empty()
                && product_field_file_keys(row).is_some()
        })
        .collect();
    let mut result = ProductLineProof {
        diagnostics: ProductLineFieldDiagnostics {
            version: 1,
            digital_control_binding_unproven_items: rows
                .iter()
                .filter(|row| row.order_id.as_deref() == Some(receipt_id))
                .count()
                .saturating_sub(matching.len()),
            ..Default::default()
        },
        charges: Vec::new(),
    };
    result.diagnostics.digital_bound_items = cards
        .iter()
        .filter(|card| {
            matching
                .iter()
                .any(|row| rebound_product_fields(**card, std::slice::from_ref(*row)))
        })
        .count();
    for row in &matching {
        let id = row.product_id.as_deref().unwrap();
        let keys = product_field_file_keys(row).unwrap();
        let siblings: Vec<_> = rows
            .iter()
            .filter(|other| {
                other.order_id.as_deref() == Some(receipt_id)
                    && other.product_id.as_deref() == Some(id)
            })
            .collect();
        // Separate rows of the same product are valid only when each row has
        // its own exact single variation and disjoint, rebound file controls.
        // No variant-name matching or aggregate amount allocation is involved.
        let mut variants = BTreeSet::new();
        let mut records = BTreeSet::new();
        let mut sibling_files = BTreeSet::new();
        let distinct_variants = siblings.iter().all(|other| {
            let Some(variant) = other.files.first().and_then(|f| f.variant_id.as_deref()) else {
                return false;
            };
            if !safety::numeric(variant)
                || !variants.insert(variant)
                || other.source_record_id.is_empty()
                || !records.insert(&other.source_record_id)
                || other
                    .files
                    .iter()
                    .any(|f| f.variant_id.as_deref() != Some(variant))
            {
                return false;
            }
            product_field_file_keys(other)
                .is_some_and(|files| files.into_iter().all(|file| sibling_files.insert(file)))
        });
        let duplicate = rows
            .iter()
            .filter(|other| {
                other.order_id.as_deref() == Some(receipt_id)
                    && other.source_record_id == row.source_record_id
            })
            .count()
            != 1
            || (siblings.len() != 1 && !distinct_variants);
        let conflicting_files = matching.iter().any(|other| {
            other.source_record_id != row.source_record_id
                && product_field_file_keys(other)
                    .is_some_and(|other_keys| !keys.is_disjoint(&other_keys))
        });
        let bound: Vec<_> = cards
            .iter()
            .copied()
            .filter(|card| rebound_product_fields(*card, std::slice::from_ref(*row)))
            .collect();
        if duplicate || conflicting_files || bound.len() != 1 {
            result.diagnostics.ambiguous_items += 1;
            result.diagnostics.digital_control_binding_unproven_items += 1;
            continue;
        }
        let Some(fields) = receipt_item_metadata(bound[0], row) else {
            result.diagnostics.ambiguous_items += 1;
            result.diagnostics.metadata_shape_unrecognized_items += 1;
            continue;
        };
        let scope = fields[0].parent().and_then(ElementRef::wrap).unwrap();
        if links(fields[0]).len() != 1
            || !closed_title(fields[0], row)
            || std::iter::once(scope)
                .chain(scope.descendants().filter_map(ElementRef::wrap))
                .any(financial_attribute)
        {
            result.diagnostics.ambiguous_items += 1;
            result.diagnostics.metadata_shape_unrecognized_items += 1;
            continue;
        }
        result.diagnostics.unique_metadata_items += 1;
        let unit =
            paid_dom_pieces(fields[1]).and_then(|parts| strict_whole_jpy(&parts.join(" "), true));
        let boost = receipt_boost_amount(*fields.last().unwrap());
        let quantity = if fields.len() == 4 {
            let quantity = receipt_quantity(fields[2]);
            result.diagnostics.explicit_quantity_items += usize::from(quantity.is_some());
            quantity
        } else {
            result.diagnostics.omitted_quantity_items += 1;
            None
        };
        result.diagnostics.unit_price_items += usize::from(unit.is_some());
        result.diagnostics.explicit_boost_items += usize::from(boost.is_some());
        result.diagnostics.unit_price_unrecognized_items += usize::from(unit.is_none());
        result.diagnostics.quantity_unrecognized_items +=
            usize::from(fields.len() == 4 && quantity.is_none());
        result.diagnostics.boost_unrecognized_items += usize::from(boost.is_none());
        let amount = match (unit, boost, quantity) {
            (Some(unit), Some(boost), Some(quantity)) => unit
                .checked_mul(quantity)
                .and_then(|price| price.checked_add(boost)),
            // Official current BOOTH cart omits its quantity spinner for digital
            // goods. Infer one only for this recognized three-field metadata
            // grammar and a freshly rebound digital control proof.
            (Some(unit), Some(boost), None) if fields.len() == 3 => unit.checked_add(boost),
            _ => None,
        }
        .filter(|amount| *amount <= 9_007_199_254_740_991);
        let Some(amount) = amount else {
            result.diagnostics.ambiguous_items += 1;
            continue;
        };
        result.charges.push(ReceiptCharge {
            source_record_id: row.source_record_id.clone(),
            product_id: id.to_owned(),
            amount,
        });
    }
    result.diagnostics.product_line_proof_items = result.charges.len();
    Ok(result)
}
pub(super) fn diagnose_product_line_fields(
    html: &str,
    rows: &[Row],
    receipt_id: &str,
    saved_receipt_witness: bool,
) -> Result<ProductLineFieldDiagnostics> {
    let mut proof = product_line_proof(html, rows, receipt_id)?;
    proof.diagnostics.saved_receipt_witness = saved_receipt_witness;
    Ok(proof.diagnostics)
}
fn covered_by(node: ElementRef<'_>, roles: &[ElementRef<'_>]) -> bool {
    roles.iter().any(|role| {
        role.id() == node.id() || role.descendants().any(|child| child.id() == node.id())
    })
}
fn omitted_notice_scope(candidate: ElementRef<'_>) -> bool {
    let known = |card| known_notice_header(card).is_some() || known_contact_prefix(card).is_some();
    known(candidate)
        || candidate
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|node| node.value().classes().any(|class| class == "sheet"))
            .is_some_and(known)
        || candidate.select(&sel(".sheet")).any(known)
}
fn no_unconsumed_text(scope: ElementRef<'_>, roles: &[ElementRef<'_>]) -> bool {
    scope.descendants().all(|node| {
        node.parent()
            .and_then(ElementRef::wrap)
            .is_some_and(|parent| covered_by(parent, roles))
            || !node
                .value()
                .as_text()
                .is_some_and(|raw| !raw.trim().is_empty())
    })
}
fn financial_attribute(node: ElementRef<'_>) -> bool {
    [
        "data-price",
        "data-amount",
        "data-total",
        "data-quantity",
        "data-paid-amount",
        "data-unit-price",
        "data-line-total",
        "data-subtotal",
        "data-discount",
        "data-fee",
    ]
    .iter()
    .any(|key| node.value().attr(key).is_some())
}
fn closed_title<'a>(title: ElementRef<'a>, row: &Row) -> bool {
    let mut roles = Vec::new();
    for anchor in links(title) {
        if identity(anchor).as_deref() != row.product_id.as_deref()
            || text(anchor) != row.name.as_deref().map(safety::short).unwrap_or_default()
        {
            return false;
        }
        roles.push(anchor);
    }
    !roles.is_empty()
        && no_unconsumed_text(title, &roles)
        && title
            .select(&sel(
                "form,button,input,textarea,select,script,style,[data-href]",
            ))
            .next()
            .is_none()
        && std::iter::once(title)
            .chain(title.descendants().filter_map(ElementRef::wrap))
            .all(|node| !financial_attribute(node))
}
pub(super) fn diagnose_receipt_notice_envelope(
    html: &str,
    rows: &[Row],
    receipt_id: &str,
) -> Result<ReceiptNoticeDiagnostics> {
    if !safety::numeric(receipt_id) {
        bail!("CHECKPOINT_INVALID");
    }
    let doc = Html::parse_document(html);
    if doc
        .select(&sel(
            "input[type='password'],form[action*='sign_in'],form[action*='login']",
        ))
        .next()
        .is_some()
    {
        bail!("AUTH_REQUIRED");
    }
    if doc
        .select(&sel("#challenge-form,.cf-challenge,iframe[src*='captcha']"))
        .next()
        .is_some()
    {
        bail!("AUTH_CHALLENGE");
    }
    let main = doc
        .select(&sel("main,[role='main']"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    let cards: Vec<_> = main
        .select(&sel(".sheet"))
        .filter(|card| card.select(&sel(".sheet")).next().is_none())
        .collect();
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.order_id.as_deref() == Some(receipt_id)
                && diagnostic_digital_file_keys(row).is_some()
        })
        .cloned()
        .collect();
    if !cards
        .iter()
        .any(|card| rebound_digital_sheet(*card, &matching))
    {
        bail!("DIGITAL_PROOF_UNPROVEN");
    }
    let mut result = ReceiptNoticeDiagnostics {
        version: 1,
        ..Default::default()
    };
    for card in cards {
        if known_notice_header(card).is_none() {
            continue;
        }
        // Count nominated leaf cards; opaque body formatting cannot nominate.
        result.notice_header_matches += 1;
        match provider_notice_envelope_phase(card) {
            NoticeEnvelopePhase::StructuralGuard => result.structural_guard_rejected += 1,
            NoticeEnvelopePhase::RivalPresence => result.rival_presence_rejected += 1,
            NoticeEnvelopePhase::OuterWrapping => result.outer_wrapping_rejected += 1,
            NoticeEnvelopePhase::SiblingStructure => result.sibling_structure_rejected += 1,
            NoticeEnvelopePhase::HeaderPlacement => result.header_placement_rejected += 1,
            NoticeEnvelopePhase::BodyRole => result.body_role_rejected += 1,
            NoticeEnvelopePhase::BodyType => result.body_type_rejected += 1,
            NoticeEnvelopePhase::BodyDepth => result.body_depth_rejected += 1,
            NoticeEnvelopePhase::Closed => result.closed_notices += 1,
        }
    }
    Ok(result)
}
pub(super) fn diagnose_receipt_contact_envelope(
    html: &str,
    rows: &[Row],
    receipt_id: &str,
) -> Result<ReceiptContactDiagnostics> {
    if !safety::numeric(receipt_id) {
        bail!("CHECKPOINT_INVALID");
    }
    let doc = Html::parse_document(html);
    if doc
        .select(&sel(
            "input[type='password'],form[action*='sign_in'],form[action*='login']",
        ))
        .next()
        .is_some()
    {
        bail!("AUTH_REQUIRED");
    }
    if doc
        .select(&sel("#challenge-form,.cf-challenge,iframe[src*='captcha']"))
        .next()
        .is_some()
    {
        bail!("AUTH_CHALLENGE");
    }
    let main = doc
        .select(&sel("main,[role='main']"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    let cards: Vec<_> = main
        .select(&sel(".sheet"))
        .filter(|card| card.select(&sel(".sheet")).next().is_none())
        .collect();
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.order_id.as_deref() == Some(receipt_id)
                && diagnostic_digital_file_keys(row).is_some()
        })
        .cloned()
        .collect();
    // Excluded contact/notice regions cannot provide independent digital context.
    if !cards
        .iter()
        .any(|card| rebound_digital_sheet(*card, &matching))
    {
        bail!("DIGITAL_PROOF_UNPROVEN");
    }
    let mut result = ReceiptContactDiagnostics {
        version: 1,
        ..Default::default()
    };
    for card in cards {
        let Some(prefix) = known_contact_prefix(card) else {
            continue;
        };
        result.contact_action_matches += 1;
        match shop_contact_envelope_phase(card, &prefix) {
            ContactEnvelopePhase::StructuralGuard => result.structural_guard_rejected += 1,
            ContactEnvelopePhase::RivalPresence => result.rival_presence_rejected += 1,
            ContactEnvelopePhase::EnvelopeStructure => result.envelope_structure_rejected += 1,
            ContactEnvelopePhase::Instruction => result.instruction_rejected += 1,
            ContactEnvelopePhase::OpaqueBodyStructure => result.opaque_body_structure_rejected += 1,
            ContactEnvelopePhase::Closed => result.closed_contacts += 1,
        }
    }
    Ok(result)
}
pub(super) fn diagnose_receipt_template(
    html: &str,
    proof_rows: &[Row],
    receipt_id: &str,
) -> Result<ReceiptTemplateDiagnostics> {
    if !safety::numeric(receipt_id) {
        bail!("CHECKPOINT_INVALID");
    }
    let doc = Html::parse_document(html);
    if doc
        .select(&sel(
            "input[type='password'],form[action*='sign_in'],form[action*='login']",
        ))
        .next()
        .is_some()
    {
        bail!("AUTH_REQUIRED");
    }
    if doc
        .select(&sel("#challenge-form,.cf-challenge,iframe[src*='captcha']"))
        .next()
        .is_some()
    {
        bail!("AUTH_CHALLENGE");
    }
    let main = doc
        .select(&sel("main,[role='main']"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    let cards: Vec<_> = main
        .select(&sel(".sheet"))
        .filter(|node| node.select(&sel(".sheet")).next().is_none())
        .collect();
    let digital: Vec<_> = cards
        .iter()
        .copied()
        .filter(|card| rebound_digital_sheet(*card, proof_rows))
        .collect();
    if digital.is_empty() {
        bail!("DIGITAL_PROOF_UNPROVEN");
    }
    let mut counts = ReceiptTemplateDiagnostics {
        version: 1,
        digital_proof_bound: true,
        ..Default::default()
    };
    let summaries = receipt_summary_candidates(&cards, &digital);
    let mut seen_parents = Vec::new();
    let mut seen_lines = Vec::new();
    let mut seen_price_scopes = Vec::new();
    for card in digital {
        for money in card.select(&sel("span,div,b,strong,small,p,td")) {
            if !noninteractive(money)
                || plain_leaf(money, 128)
                    .as_deref()
                    .and_then(currency_jpy)
                    .is_none()
            {
                continue;
            }
            let Some(parent) = money.parent().and_then(ElementRef::wrap) else {
                continue;
            };
            if parent.id() != card.id() && !card.descendants().any(|node| node.id() == parent.id())
            {
                continue;
            }
            if seen_parents.contains(&parent.id()) {
                continue;
            }
            seen_parents.push(parent.id());
            counts.jpy_parents += 1;
            counts.jpy_parent_direct_text_nodes += parent
                .children()
                .filter_map(|node| node.value().as_text())
                .filter(|text| !text.trim().is_empty() && text.chars().count() <= 64)
                .count();
            let pieces = template_dom_pieces(parent);
            if let Some(parts) = &pieces {
                for token in parts {
                    let token = token.trim();
                    let quantity = token
                        .strip_prefix('×')
                        .or_else(|| token.strip_prefix('x'))
                        .unwrap_or(token)
                        .trim();
                    counts.jpy_parent_multiplication_tokens += usize::from(
                        token == "×"
                            || token == "x"
                            || token.starts_with(['×', 'x'])
                                && !quantity.is_empty()
                                && quantity.bytes().all(|c| c.is_ascii_digit()),
                    );
                    counts.jpy_parent_quantity_nodes += usize::from(
                        !quantity.is_empty()
                            && quantity.bytes().all(|c| c.is_ascii_digit())
                            && quantity
                                .parse::<u64>()
                                .is_ok_and(|number| number > 0 && number <= 9_007_199_254_740_991),
                    );
                }
            }
            let syntax = pieces
                .as_ref()
                .and_then(|pieces| compact_quantity(&pieces.join(" ")))
                .is_some();
            counts.jpy_parent_compact_syntax_rows += usize::from(syntax);
            let lines = normalized_product_lines(parent, card, proof_rows);
            counts.jpy_parent_same_product_line_rows += usize::from(syntax && !lines.is_empty());
            for line in lines {
                if !seen_lines.contains(&line.id()) {
                    seen_lines.push(line.id());
                    counts.normalized_title_line_witnesses += 1;
                }
            }
            for scope in std::iter::once(parent)
                .chain(parent.ancestors().filter_map(ElementRef::wrap))
                .take(3)
            {
                if scope.id() != card.id()
                    && !card.descendants().any(|node| node.id() == scope.id())
                {
                    break;
                }
                if !seen_price_scopes
                    .iter()
                    .any(|seen: &ElementRef<'_>| seen.id() == scope.id())
                    && scope.descendants().count() <= 128
                    && noninteractive(scope)
                {
                    seen_price_scopes.push(scope);
                }
                if scope.id() == card.id() {
                    break;
                }
            }
        }
    }
    // Count known money labels once, even where nested price scopes overlap.
    let mut seen_label_text = Vec::new();
    for scope in seen_price_scopes {
        for node in scope.descendants() {
            if seen_label_text.contains(&node.id()) {
                continue;
            }
            let Some(raw) = node
                .value()
                .as_text()
                .filter(|raw| raw.chars().count() <= 40)
            else {
                continue;
            };
            let Some(parent) = node.parent().and_then(ElementRef::wrap) else {
                continue;
            };
            if !noninteractive(parent) {
                continue;
            }
            seen_label_text.push(node.id());
            match template_literal(raw) {
                "BOOST" | "BOOST↑" | "BOOST金額" | "ブースト" => {
                    counts.price_scope_boost_text_nodes += 1
                }
                "支払金額" | "支払額" | "Paid amount" => {
                    counts.price_scope_explicit_paid_text_nodes += 1
                }
                "小計" | "Subtotal" => counts.price_scope_subtotal_text_nodes += 1,
                _ => (),
            }
        }
    }
    let mut dates = BTreeSet::new();
    for candidate in summaries {
        counts.summary_candidates += 1;
        let observation = receipt_summary_observation(candidate, receipt_id);
        counts.summary_order_number_text_nodes += observation.number_labels;
        counts.summary_ordered_date_text_nodes += observation.date_labels;
        counts.summary_role_candidates += usize::from(observation.role);
        counts.summary_receipt_number_matches += usize::from(observation.number_matches);
        counts.summary_ambiguous_pairs += usize::from(observation.ambiguous);
        counts.summary_unique_date_pairs += usize::from(observation.date.is_some());
        counts.summary_slash_dates += usize::from(observation.slash);
        counts.summary_iso_dates += usize::from(observation.iso);
        if let Some(date) = observation.date {
            dates.insert(date);
        }
    }
    counts.summary_ambiguous_pairs += usize::from(dates.len() > 1);
    Ok(counts)
}
pub(super) fn diagnose_manifests(
    html: &str,
    source: &str,
    route: &str,
) -> Result<super::diagnostics::ManifestDiagnostics> {
    let mut counts = super::diagnostics::ManifestDiagnostics::default();
    let page = parse_internal(html, source, route, Some(&mut counts), None)?;
    counts.finish(&page);
    Ok(counts)
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileStructureDiagnostics {
    version: usize,
    rows: usize,
    valid_controls: usize,
    modern_name_controls: usize,
    test_name_controls: usize,
    legacy_name_controls: usize,
    unrecognized_entry_controls: usize,
    recognized_entries: usize,
    rows_all_entries_recognized: usize,
    rows_file_only_container: usize,
    rows_card_only: usize,
    orphan_modern_names: usize,
    rows_with_orphan_modern_name: usize,
    rows_conflicting_entry_scopes: usize,
    rows_other_download_methods: usize,
    rows_expand_or_load_more: usize,
    rows_variant_selector: usize,
    rows_unknown_dropdown: usize,
    rows_advertised_count: usize,
    rows_advertised_count_matches: usize,
    rows_advertised_count_mismatch: usize,
    rows_advertised_count_invalid: usize,
}
pub(super) fn diagnose_file_structure(
    html: &str,
    source: &str,
    route: &str,
) -> Result<FileStructureDiagnostics> {
    let mut counts = FileStructureDiagnostics::default();
    let page = parse_internal(html, source, route, None, Some(&mut counts))?;
    counts.version = 1;
    counts.rows = page.rows.len();
    Ok(counts)
}
impl FileStructureDiagnostics {
    fn modern_list_witness(card: ElementRef<'_>, files: &[File]) -> bool {
        let mut evidence = Self::default();
        evidence.record(card, files);
        evidence.valid_controls > 0
            && evidence.modern_name_controls == evidence.valid_controls
            && evidence.test_name_controls == 0
            && evidence.legacy_name_controls == 0
            && evidence.unrecognized_entry_controls == 0
            && evidence.rows_all_entries_recognized == 1
            && evidence.rows_file_only_container == 1
            && evidence.orphan_modern_names == 0
            && evidence.rows_conflicting_entry_scopes == 0
            && evidence.rows_expand_or_load_more == 0
            && evidence.rows_variant_selector == 0
            && evidence.rows_unknown_dropdown == 0
            && evidence.rows_advertised_count_invalid == 0
            && evidence.rows_advertised_count_mismatch == 0
            && (evidence.rows_advertised_count == 0 || evidence.rows_advertised_count_matches == 1)
    }
    fn record(&mut self, card: ElementRef<'_>, files: &[File]) {
        let controls = downloads(card, false);
        self.valid_controls += controls.len();
        let mut entries = BTreeMap::new();
        let mut recognized = 0;
        let mut accepted_modern_names = vec![];
        let mut conflicting_entries = false;
        for (button, id, variant, _) in &controls {
            let mut found = None;
            for scope in button.ancestors().filter_map(ElementRef::wrap).take(4) {
                if scope.id() == card.id() || !card.descendants().any(|n| n.id() == scope.id()) {
                    break;
                }
                let keys: BTreeSet<_> = downloads(scope, false)
                    .into_iter()
                    .map(|(_, id, variant, _)| (id, variant))
                    .collect();
                if keys.len() != 1 {
                    break;
                }
                let test: Vec<_> = scope.select(&sel("[data-test='filename']")).collect();
                let modern: Vec<_> = scope
                    .select(&sel(".min-w-0.break-words.whitespace-pre-line .text-14"))
                    .collect();
                let legacy: Vec<_> = if scope.value().classes().any(|c| c == "legacy-list-item") {
                    scope
                        .select(&sel(".min-w-0 > b"))
                        .filter(|n| {
                            n.select(&sel("a,button,input,textarea,select"))
                                .next()
                                .is_none()
                                && looks_filename(&text(*n))
                        })
                        .collect()
                } else {
                    vec![]
                };
                let (names, kind) = if !test.is_empty() {
                    (test, 0)
                } else if !modern.is_empty() {
                    (modern, 1)
                } else {
                    (legacy, 2)
                };
                if names.len() > 1 {
                    break;
                }
                if names.len() == 1 {
                    if !text(names[0]).is_empty() {
                        found = Some((scope, kind, names[0].id()));
                    }
                    break;
                }
            }
            if let Some((scope, kind, name_id)) = found {
                recognized += 1;
                match kind {
                    0 => self.test_name_controls += 1,
                    1 => {
                        self.modern_name_controls += 1;
                        accepted_modern_names.push(name_id);
                    }
                    _ => self.legacy_name_controls += 1,
                }
                let key = (id.clone(), variant.clone());
                if entries
                    .get(&key)
                    .is_some_and(|old: &ElementRef<'_>| old.id() != scope.id())
                {
                    conflicting_entries = true;
                }
                entries.entry(key).or_insert(scope);
            } else {
                self.unrecognized_entry_controls += 1;
            }
        }
        self.recognized_entries += entries.len();
        let all = !files.is_empty() && recognized == controls.len() && entries.len() == files.len();
        self.rows_all_entries_recognized += usize::from(all);
        let container = if all {
            entries.values().next().and_then(|first| {
                first
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .take(4)
                    .take_while(|n| n.id() != card.id())
                    .filter(|n| {
                        matches!(n.value().name(), "div" | "ul" | "ol" | "section")
                            && links(*n).into_iter().all(|a| identity(a).is_none())
                            && downloads(*n, false).len() == controls.len()
                            && entries
                                .values()
                                .all(|e| n.descendants().any(|d| d.id() == e.id()))
                    })
                    .last()
            })
        } else {
            None
        };
        self.rows_file_only_container += usize::from(container.is_some());
        self.rows_card_only += usize::from(all && container.is_none());
        self.rows_conflicting_entry_scopes += usize::from(conflicting_entries);
        // A same-product anchor can disqualify an outer wrapper while leaving a
        // smaller eligible container. Do not let it hide a sibling filename row.
        let orphan_names = card
            .select(&sel(".min-w-0.break-words.whitespace-pre-line .text-14"))
            .filter(|name| !accepted_modern_names.contains(&name.id()))
            .count();
        self.orphan_modern_names += orphan_names;
        self.rows_with_orphan_modern_name += usize::from(orphan_names > 0);
        self.rows_other_download_methods += usize::from(
            card.select(&sel("[data-test='other-downloads-button']"))
                .next()
                .is_some(),
        );
        self.rows_expand_or_load_more+=usize::from(card.select(&sel("[data-test='load-more'],[data-test='show-more'],[data-test='more-downloadables'],button[aria-expanded='false']:not([data-test='other-downloads-button'])")).next().is_some());
        self.rows_variant_selector+=usize::from(card.select(&sel("[data-test='variation-select'],[data-test='variation-selector'],select[name='variation_id'],select[name='variation']")).next().is_some());
        self.rows_unknown_dropdown += usize::from(
            card.select(&sel(
                "[data-dropdown-items]:not([data-test='other-downloads-button'])",
            ))
            .next()
            .is_some(),
        );
        let mut advertised = vec![];
        for node in std::iter::once(card).chain(card.select(&sel(
            "[data-file-count],[data-downloadable-count],[data-total-count]",
        ))) {
            for attr in [
                "data-file-count",
                "data-downloadable-count",
                "data-total-count",
            ] {
                if let Some(value) = node.value().attr(attr) {
                    advertised.push(value.parse::<usize>().ok());
                }
            }
        }
        self.rows_advertised_count += usize::from(!advertised.is_empty());
        self.rows_advertised_count_matches += usize::from(
            !advertised.is_empty() && advertised.iter().all(|v| *v == Some(files.len())),
        );
        self.rows_advertised_count_mismatch += usize::from(
            advertised
                .iter()
                .any(|v| v.is_some_and(|n| n != files.len())),
        );
        self.rows_advertised_count_invalid += usize::from(advertised.iter().any(Option::is_none));
    }
}
fn parse_internal(
    html: &str,
    source: &str,
    route: &str,
    mut diagnostic: Option<&mut super::diagnostics::ManifestDiagnostics>,
    mut structure: Option<&mut FileStructureDiagnostics>,
) -> Result<Page> {
    let url = safety::route(route, source)?;
    let source = safety::source_name(source)?.to_owned();
    let path = safety::source_path(&source)?;
    let is_detail = path == "/orders" && url.path() != path;
    let doc = Html::parse_document(html);
    crate::support_trace::parser_language(&source, html_language(&doc));
    // The site's anonymous OAuth page has no password/login form. Its explicit
    // signed-out marker must also reject an otherwise recognizable empty shelf.
    if doc
        .select(&sel("meta[name='js_const_user_signed_in']"))
        .any(|meta| meta.value().attr("content") == Some("false"))
        || doc
            .select(&sel(
                "input[type='password'],form[action*='sign_in'],form[action*='login']",
            ))
            .next()
            .is_some()
    {
        bail!("AUTH_REQUIRED");
    }
    if doc
        .select(&sel("#challenge-form,.cf-challenge,iframe[src*='captcha']"))
        .next()
        .is_some()
    {
        bail!("AUTH_CHALLENGE");
    }
    let heading = if is_detail {
        String::new()
    } else {
        doc.select(&sel("h1,h2,[data-test='page-title']"))
            .map(text)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let marker = is_detail
        || match source.as_str() {
            "library" => heading.contains("ライブラリ") || heading.contains("Library"),
            "purchase" => heading.contains("注文") || heading.contains("Order"),
            "gift" => heading.contains("ギフト") || heading.contains("Gift"),
            "free" => {
                heading.contains("無料")
                    || heading.contains("Free")
                    || heading.contains("ダウンロード")
            }
            _ => false,
        }
        || doc.select(&sel("a[href]")).any(|a| {
            a.value()
                .attr("href")
                .and_then(absolute)
                .and_then(|u| url::Url::parse(&u).ok())
                .is_some_and(|u| u.host_str() == Some("accounts.booth.pm") && u.path() == path)
        });
    if !marker {
        bail!("SOURCE_MARKER_UNKNOWN");
    }
    let main = doc
        .select(&sel("main,[role='main']"))
        .next()
        .unwrap_or_else(|| doc.root_element());
    let selector = if is_detail {
        ".sheet"
    } else if source == "purchase" {
        "[data-test='order'],.order-list-item,.order-item,.sheet"
    } else {
        "[data-test='library-item'],.l-library-item,.bg-white.p-16"
    };
    let mut cards: Vec<_> = main
        .select(&sel(selector))
        .filter(|e| e.select(&sel(selector)).next().is_none())
        .collect();
    // Digital-only fallback: scope each product anchor to one repeated component.
    if cards.is_empty() {
        for a in main.select(&sel("a[href]")) {
            let target = if source == "purchase" && !is_detail {
                order_id(a)
            } else {
                identity(a)
            };
            if target.is_none() {
                continue;
            }
            let mut picked = None;
            for n in a.ancestors().filter_map(ElementRef::wrap) {
                if n.id() == main.id()
                    || matches!(
                        n.value().name(),
                        "body" | "html" | "nav" | "header" | "footer"
                    )
                {
                    break;
                }
                let ids: BTreeSet<_> = links(n)
                    .into_iter()
                    .filter_map(|a| {
                        if source == "purchase" && !is_detail {
                            order_id(a)
                        } else {
                            identity(a)
                        }
                    })
                    .collect();
                if ids.len() > 1 {
                    break;
                }
                picked = Some(n);
                if matches!(n.value().name(), "article" | "li" | "tr")
                    || n.value().classes().any(|c| matches!(c, "sheet" | "p-16"))
                {
                    break;
                }
            }
            if let Some(n) = picked {
                if !cards.iter().any(|e| e.id() == n.id()) {
                    cards.push(n);
                }
            }
        }
    }
    let mut page = Page {
        source: source.clone(),
        route: format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        ),
        rows: vec![],
        next: vec![],
        details: vec![],
        terminal: false,
        unresolved: 0,
        fingerprint: String::new(),
        is_detail,
        ordering: ordering(&doc, &source, &heading, is_detail),
        free_item: None,
    };
    let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
    for (ordinal, card) in cards.iter().copied().enumerate() {
        if source == "purchase" && !is_detail {
            let mut ids: BTreeSet<_> = links(card).into_iter().filter_map(order_id).collect();
            if let Some(id) = enclosing_order(card) {
                ids.insert(id);
            }
            if ids.len() == 1 {
                let id = ids.into_iter().next().unwrap();
                page.details.push(format!("/orders/{id}"));
            } else {
                page.unresolved += 1;
            }
            continue;
        }
        let receipt = source == "purchase" && is_detail;
        let raw_controls = raw_download_controls(card, receipt);
        if is_detail && (raw_controls == 0 || omitted_notice_scope(card)) {
            continue;
        }
        let anchors = links(card);
        let ids: BTreeSet<_> = anchors.iter().copied().filter_map(identity).collect();
        let f = files(card, receipt);
        let unavailable = unavailable(card);
        // Do not inspect text of order summaries, physical goods, comments or contacts.
        if is_detail && f.is_empty() && !unavailable {
            // Ignore unrelated receipt regions. Invalid digital candidates still
            // prevent a complete traversal, without inspecting their body.
            if raw_controls != 0 {
                page.unresolved += 1;
                page.rows.push(Row {
                    source_record_id: format!("purchase:{}:{ordinal}", url.path()),
                    order_id: url.path().strip_prefix("/orders/").map(str::to_owned),
                    error_code: Some("DIGITAL_CONTROLS_UNRESOLVED".into()),
                    ..Row::default()
                });
            }
            continue;
        }
        let product_id = if ids.len() == 1 {
            ids.into_iter().next()
        } else {
            None
        };
        let mut error_code = if product_id.is_none()
            && unavailable
            && f.is_empty()
            && !numeric_identity_present(card)
        {
            Some("BOOTH_WITHHELD_PRODUCT_ID".to_owned())
        } else if product_id.is_none() {
            Some("PRODUCT_ID_UNRESOLVED".to_owned())
        } else {
            None
        };
        if error_code.as_deref() == Some("PRODUCT_ID_UNRESOLVED") {
            page.unresolved += 1;
        }
        let title = first_text(card,"[data-test='item-name'],.font-bold.text-16.break-all,.text-text-default.font-bold,b a.nav[href*='/items/']").or_else(||anchors.iter().copied().find(|a|product_id.is_some() && identity(*a)==product_id && !text(*a).is_empty()).map(text));
        if product_id.is_some() && title.is_none() && !unavailable {
            error_code = Some("PRODUCT_TITLE_UNRESOLVED".into());
            page.unresolved += 1;
        }
        if raw_controls != downloads(card, receipt).len() {
            error_code = Some("DIGITAL_CONTROLS_UNRESOLVED".into());
            page.unresolved += 1;
        }
        let shop = anchors.iter().copied().find(|a| {
            a.value()
                .attr("href")
                .and_then(absolute)
                .and_then(|v| safety::public_url(&v, "shop"))
                .is_some()
        });
        let img = card
            .select(&sel(
                "img.l-library-item-thumbnail,img[src*='pximg'],[data-test='item-thumbnail']",
            ))
            .next();
        let explicit_list = card
            .select(&sel("[data-test='downloadable-list'],.list.list--collapse"))
            .next()
            .is_some()
            || card
                .select(&sel(".legacy-list-item [data-dropdown-items]"))
                .next()
                .is_some();
        let manifest_complete = !f.is_empty()
            && error_code.is_none()
            && !unavailable
            && raw_controls == downloads(card, receipt).len()
            && f.iter().all(|f| f.name.is_some())
            && (explicit_list
                || (matches!(source.as_str(), "library" | "gift" | "free")
                    && FileStructureDiagnostics::modern_list_witness(card, &f)));
        let positive_purchase = source == "purchase"
            && is_detail
            && error_code
                .as_deref()
                .is_none_or(|code| code == "PRODUCT_TITLE_UNRESOLVED")
            && rebound_product_fields(
                card,
                &[Row {
                    product_id: product_id.clone(),
                    files: f.clone(),
                    manifest_complete,
                    ..Row::default()
                }],
            );
        // Retain the previous direct-card component for existing native IDs only.
        // A receipt-summary date never enters this legacy identity component.
        // Preserve previously established native keys; a newly recoverable
        // financial field must not change an existing undated acquisition ID.
        let legacy_date = if positive_purchase {
            card.select(&sel("time[datetime],[data-test='downloaded-at'],[data-test='acquired-at'],[data-test='ordered-at']")).next().map(|n|n.value().attr("datetime").map(safety::short).unwrap_or_else(||text(n)))
        } else {
            None
        };
        let mut date = if source == "purchase" && is_detail {
            None
        } else {
            card.select(&sel("time[datetime],[data-test='downloaded-at'],[data-test='acquired-at'],[data-test='ordered-at']")).next().map(|n|n.value().attr("datetime").map(safety::short).unwrap_or_else(||text(n)))
        };
        let line = card
            .value()
            .attr("data-order-item-id")
            .or_else(|| card.value().attr("data-line-item-id"))
            .filter(|v| safety::numeric(v));
        let order_id = if is_detail {
            url.path().strip_prefix("/orders/").map(str::to_owned)
        } else {
            None
        };
        let variant_name = first_text(card, "[data-test='variation-name'],.variation-name");
        let variant_identity = card
            .value()
            .attr("data-variation-id")
            .filter(|v| safety::numeric(v))
            .unwrap_or("");
        let record_key = line.map(|v| format!("line:{v}")).unwrap_or_else(|| {
            format!(
                "{}:{}:{}:{}:{}",
                order_id.as_deref().unwrap_or(""),
                product_id.as_deref().unwrap_or("unknown"),
                variant_identity,
                // Preserve both previously dated and undated native purchase IDs.
                if source == "purchase" && is_detail {
                    legacy_date.as_deref().unwrap_or("")
                } else {
                    date.as_deref().unwrap_or("")
                },
                if product_id.is_none() {
                    format!("{route}:{ordinal}")
                } else {
                    String::new()
                }
            )
        });
        let base_record_id = format!("{source}:{}", safety::hash(record_key));
        let occurrence = occurrences.entry(base_record_id.clone()).or_default();
        let source_record_id = if *occurrence == 0 {
            base_record_id
        } else {
            format!("{base_record_id}:occurrence:{occurrence}")
        };
        *occurrence += 1;
        if positive_purchase {
            date = order_id
                .as_deref()
                .and_then(|id| receipt_acquired_date(&cards, card, id));
            if legacy_date
                .as_ref()
                .is_some_and(|legacy| date.as_ref().is_some_and(|date| date != legacy))
            {
                date = None;
            }
        }
        if let Some(counts) = diagnostic.as_deref_mut() {
            counts.record(
                explicit_list,
                &f,
                error_code.is_none(),
                unavailable,
                raw_controls,
                downloads(card, receipt).len(),
                manifest_complete,
            );
        }
        if let Some(counts) = structure.as_deref_mut() {
            counts.record(card, &f);
        }
        page.rows.push(Row {
            source_record_id,
            product_id,
            order_id,
            name: title,
            shop: shop.map(text),
            shop_url: shop
                .and_then(|a| a.value().attr("href"))
                .and_then(absolute)
                .and_then(|v| safety::public_url(&v, "shop")),
            preview_url: img
                .and_then(|e| e.value().attr("src").or_else(|| e.value().attr("data-src")))
                .and_then(absolute)
                .and_then(|v| safety::public_url(&v, "preview")),
            acquired_date_text: date,
            paid_amount_text: first_text(card, "[data-test='paid-amount']"),
            product_line_total_jpy: None,
            variant_name,
            files: f,
            manifest_complete,
            unavailable,
            error_code,
        });
    }
    if is_detail {
        if cards.is_empty() {
            page.unresolved += 1;
        }
    } else {
        if cards.is_empty() {
            let empty = main
                .select(&sel(
                    "[data-test='empty-state'],.empty-state,.library-empty,.no-items",
                ))
                .any(|n| {
                    let s = text(n).to_lowercase();
                    [
                        "ありません",
                        "まだ",
                        "見つかりません",
                        "no items",
                        "no orders",
                        "no downloads",
                        "no gifts",
                        "empty",
                    ]
                    .iter()
                    .any(|v| s.contains(v))
                });
            if !empty {
                bail!("NO_RECOGNIZED_ROWS");
            }
            page.terminal = true;
        }
        let current = url
            .query_pairs()
            .find(|(k, _)| k == "page")
            .and_then(|(_, v)| v.parse::<u32>().ok())
            .unwrap_or(1);
        for a in doc.select(&sel("a[href]")) {
            if let Some(u) = a
                .value()
                .attr("href")
                .and_then(|v| safety::route(v, &source).ok())
            {
                if u.path() == path {
                    let p = u
                        .query_pairs()
                        .next()
                        .and_then(|(_, v)| v.parse::<u32>().ok())
                        .unwrap_or(0);
                    if p > current {
                        page.next.push(format!("{path}?page={p}"));
                    }
                }
            }
        }
        page.next.sort();
        page.next.dedup();
        if page.next.is_empty() {
            let explicit_last = doc
                .select(&sel("a.last-page[href],a[aria-label='Last'][href]"))
                .any(|a| {
                    a.value()
                        .attr("href")
                        .and_then(|v| safety::route(v, &source).ok())
                        .is_some_and(|u| u == url)
                });
            if explicit_last {
                page.terminal = true;
            }
        }
        for pager in doc.select(&sel(".pagination,.pager")) {
            let own = pager.select(&sel("a[href]")).any(|a| {
                a.value()
                    .attr("href")
                    .and_then(|v| safety::route(v, &source).ok())
                    .is_some_and(|u| u.path() == path)
            });
            if !own {
                continue;
            }
            let current_matches = pager
                .select(&sel(
                    "li.current > a[href],a.current[href],a[aria-current='page']",
                ))
                .any(|a| {
                    a.value()
                        .attr("href")
                        .and_then(|v| safety::route(v, &source).ok())
                        .is_some_and(|u| u == url)
                });
            let enabled_next = pager.select(&sel("a[rel='next'][href]")).any(|a| {
                a.value().attr("aria-disabled") != Some("true")
                    && !a.value().classes().any(|c| c == "disabled")
            });
            let disabled_next=pager.select(&sel(".next.disabled,.next-page.disabled,[rel='next'][aria-disabled='true'],[aria-label='Next'][aria-disabled='true']")).next().is_some();
            if page.next.is_empty() && (disabled_next || (current_matches && !enabled_next)) {
                page.terminal = true;
            }
        }
    }
    if source == "purchase" && is_detail {
        if let Some(receipt_id) = url.path().strip_prefix("/orders/") {
            let (fields, rejected) =
                match product_line_proof_document(&doc, &page.rows, receipt_id) {
                    Ok(proof) => {
                        let fields = serde_json::to_value(&proof.diagnostics).unwrap_or(Value::Null);
                        apply_product_line_charges(&mut page.rows, proof, receipt_id);
                        (fields, false)
                    }
                    Err(_) => (Value::Null, true),
                };
            let (rows, paid_rows, dated_rows) = page
                .rows
                .iter()
                .filter(|row| row.order_id.as_deref() == Some(receipt_id))
                .fold((0u64, 0u64, 0u64), |(rows, paid, dated), row| {
                    (
                        rows + 1,
                        paid + u64::from(row.product_line_total_jpy.is_some()),
                        dated
                            + u64::from(
                                row.acquired_date_text
                                    .as_ref()
                                    .is_some_and(|value| !value.trim().is_empty()),
                            ),
                    )
                });
            let language = html_language(&doc);
            // This counts parser calls in the current job attempt, not unique
            // receipts. Only fixed counters/booleans and a closed language code
            // leave the parser; receipt identities and source values stay here.
            crate::support_trace::receipt(
                &fields, rows, paid_rows, dated_rows, rejected, language,
            );
        }
    }
    page.details.sort();
    page.details.dedup();
    // Include actual file facts, never signing parameters, in restart-boundary evidence.
    page.fingerprint = safety::hash(serde_json::to_vec(
        &serde_json::json!({"rows":page.rows.iter().map(|r|(&r.source_record_id,&r.product_id,r.files.iter().map(|f|(&f.id,&f.variant_id,&f.revision,&f.name,f.size_bytes)).collect::<Vec<_>>())).collect::<Vec<_>>(),"next":page.next,"details":page.details,"terminal":page.terminal,"unresolved":page.unresolved,"ordering":page.ordering}),
    )?);
    Ok(page)
}
fn order_id(e: ElementRef<'_>) -> Option<String> {
    let u = url::Url::parse(&absolute(e.value().attr("href")?)?).ok()?;
    let id = u.path().strip_prefix("/orders/")?;
    (safety::secure(&u)
        && u.host_str() == Some("accounts.booth.pm")
        && safety::numeric(id)
        && u.query().is_none())
    .then(|| id.to_owned())
}
fn enclosing_order(card: ElementRef<'_>) -> Option<String> {
    let repeated = sel("[data-test='order'],.order-list-item,.order-item,.sheet");
    for wrapper in card.ancestors().filter_map(ElementRef::wrap).take(4) {
        if matches!(
            wrapper.value().name(),
            "main" | "body" | "html" | "nav" | "header" | "footer" | "article" | "li" | "tr"
        ) {
            break;
        }
        // A shared parent containing another order card cannot supply identity.
        if wrapper.select(&repeated).count() != 1 {
            break;
        }
        if wrapper.value().name() == "a" {
            return order_id(wrapper);
        }
        if wrapper.value().name() != "div" {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anonymous_account_marker_rejects_oauth_and_empty_library_without_rejecting_signed_in_shelf()
    {
        let signed_out = "<meta name='js_const_user_signed_in' content='false'>";
        let oauth = format!("{signed_out}<main><h1>ログイン</h1><form action='/users/auth/pixiv' method='post'><input type='submit' value='pixivアカウントでログイン'></form></main>");
        let empty = "<main><h1>Library</h1><div class='library-empty'>No items</div></main>";
        for html in [oauth, format!("{signed_out}{empty}")] {
            assert_eq!(
                parse(&html, "library", "/library?page=1")
                    .err()
                    .unwrap()
                    .to_string(),
                "AUTH_REQUIRED"
            );
        }
        for html in [
            empty.to_owned(),
            format!("<meta name='js_const_user_signed_in' content='true'>{empty}"),
        ] {
            let page = parse(&html, "library", "/library?page=1").unwrap();
            assert!(page.terminal);
            assert!(page.rows.is_empty());
        }
        assert_eq!(
            parse(
                "<main><h1>Unknown page</h1></main>",
                "library",
                "/library?page=1"
            )
            .err()
            .unwrap()
            .to_string(),
            "SOURCE_MARKER_UNKNOWN"
        );
    }
    fn charge_item(
        id: &str,
        file: &str,
        unit: &str,
        quantity: Option<&str>,
        boost: &str,
    ) -> String {
        let quantity = quantity
            .map(|value| format!("<div>{value}</div>"))
            .unwrap_or_default();
        format!("<div class='sheet' data-product-id='{id}'><section><div><div><b><a href='https://invented.booth.pm/items/{id}'>Authored {id}</a></b></div><div>{unit}</div>{quantity}<div><i class='icon-boost'></i><span>{boost}</span></div></div></section><div data-test='downloadable-list'><b data-test='filename'>authored-{file}.zip</b><a href='https://booth.pm/downloadables/{file}'>Download</a></div></div>")
    }
    fn charge_receipt(items: &str, total: &str, extra_summary: &str) -> String {
        format!("<main>{items}<div class='sheet'><div><div><span>注文番号</span><span>910001</span></div><div><span>注文日時</span><span>2026/10/10 09:30</span></div><div><span>お支払方法</span><span>PRIVATE_OPAQUE_METHOD</span></div><div><span>お支払金額</span><span>{total}</span></div>{extra_summary}</div></div></main>")
    }
    fn authored_notice(body: &str) -> String {
        format!("<div class='sheet'><div><div class='text-16 font-bold'>発送通知コメント</div><div class='text-14'>{body}</div></div></div>")
    }
    fn authored_contact(body: &str) -> String {
        format!("<div class='sheet'><div><p>お支払いが完了しました</p><a href='/conversations/authored_token?opaque=PRIVATE_QUERY'>ショップに連絡する</a><div>{body}</div></div></div>")
    }
    fn assert_bound_product_price(page: &Page, amount: u64) {
        assert_eq!(
            page.rows
                .iter()
                .filter(|row| row.product_id.as_deref() == Some("900001"))
                .count(),
            1
        );
        for row in &page.rows {
            assert_eq!(
                row.product_line_total_jpy,
                (row.product_id.as_deref() == Some("900001")).then_some(amount)
            );
        }
    }
    #[tokio::test]
    async fn parser_language_counts_received_pages_before_early_errors() {
        let cases = [
            ("<html lang='ja-JP'><head><meta name='js_const_user_signed_in' content='false'></head><body>PRIVATE_AUTH</body></html>", "AUTH_REQUIRED"),
            ("<html lang='EN_us'><body><main><h1>Library</h1><form id='challenge-form'></form></main></body></html>", "AUTH_CHALLENGE"),
            ("<html lang='ko-KR'><body><main><h1>PRIVATE_HEADING</h1></main></body></html>", "SOURCE_MARKER_UNKNOWN"),
            ("<html lang='zh-Hans'><body><main><h1>PRIVATE_HEADING</h1></main></body></html>", "SOURCE_MARKER_UNKNOWN"),
            ("<html lang='ZH_tw'><body><main><h1>PRIVATE_HEADING</h1></main></body></html>", "SOURCE_MARKER_UNKNOWN"),
            ("<html lang='zh-Hant'><body><main><h1>PRIVATE_HEADING</h1></main></body></html>", "SOURCE_MARKER_UNKNOWN"),
            ("<html lang='zh-Hant-PRIVATE_LANGUAGE'><body><main><h1>PRIVATE_HEADING</h1></main></body></html>", "SOURCE_MARKER_UNKNOWN"),
            ("<main><h1>Library</h1></main>", "NO_RECOGNIZED_ROWS"),
            ("<html lang='  '><body><main><h1>Library</h1></main></body></html>", "NO_RECOGNIZED_ROWS"),
        ];
        let (_, trace) = crate::support_trace::capture(async {
            for (html, expected) in cases {
                let error = parse(html, "library", "/library?page=1").unwrap_err();
                assert_eq!(error.to_string(), expected);
            }
        }).await;
        let counts = &trace["sources"]["library"];
        for (key, expected) in [
            ("parserCalls", 9), ("htmlJa", 1), ("htmlEn", 1),
            ("htmlKo", 1), ("htmlZhCn", 1), ("htmlZhTw", 2),
            ("htmlOther", 1), ("htmlUnknown", 2),
        ] {
            assert_eq!(counts[key], expected, "{key}");
        }
        assert_eq!(counts.as_object().unwrap().len(), 8);
        assert!(trace["financial"].as_object().unwrap().is_empty());
        for private in ["PRIVATE", "ja-JP", "EN_us", "ko-KR", "zh-Hans", "ZH_tw", "zh-Hant", "Library", "AUTH_REQUIRED"] {
            assert!(!trace.to_string().contains(private), "{private}");
        }
    }
    #[tokio::test]
    async fn receipt_trace_counts_only_current_parser_calls_without_source_values() {
        let item = charge_item("900001", "700001", "¥ 137", None, "0");
        let known = charge_receipt(&item, "¥ 137", "");
        let expected = serde_json::to_value(parse(&known, "purchase", "/orders/910001").unwrap()).unwrap();
        for language in ["ja-JP", "en-US", "ko-KR", "zh-Hans", "zh-Hant", "zh-Hant-PRIVATE_LANGUAGE"] {
            let localized = format!("<html lang='{language}'><body>{known}</body></html>");
            assert_eq!(serde_json::to_value(parse(&localized, "purchase", "/orders/910001").unwrap()).unwrap(), expected);
        }
        let unknown_price = charge_receipt(
            &charge_item("900001", "700001", "PRIVATE_INVALID_PRICE", None, "0"),
            "¥ 137",
            "",
        );
        let cases = [
            format!("<html lang='ja-JP'><body>{known}</body></html>"),
            format!("<html lang='en-US'><body>{unknown_price}</body></html>"),
            format!("<html lang='zh-Hant-PRIVATE_LANGUAGE'><body><main>{item}</main></body></html>"),
            known.clone(),
            format!("<html lang='ja-JP'><body>{known}</body></html>"),
            format!("<html lang='ko-KR'><body>{known}</body></html>"),
            format!("<html lang='zh-Hans'><body>{known}</body></html>"),
            format!("<html lang='zh-Hant'><body>{known}</body></html>"),
        ];
        let (pages, trace) = crate::support_trace::capture(async {
            let pages = cases
                .iter()
                .map(|html| parse(html, "purchase", "/orders/910001").unwrap())
                .collect::<Vec<_>>();
            // A different source must not enter the purchase denominators.
            parse(HTML, "library", "/library?page=1").unwrap();
            pages
        })
        .await;
        for (index, page) in pages.iter().enumerate() {
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.rows[0].product_line_total_jpy, (index != 1).then_some(137));
            assert_eq!(page.rows[0].acquired_date_text.is_some(), index != 2);
        }
        let facts = &trace["financial"];
        // The repeated receipt is intentionally another parser call, not a new
        // unique receipt or transaction. Missing price and date stay separate.
        for (key, expected) in [
            ("receiptPages", 8),
            ("rows", 8),
            ("rowsWithPaidProof", 7),
            ("rowsWithDateProof", 7),
            ("proofRejected", 0),
            ("digitalBoundItems", 8),
            ("uniqueMetadataItems", 8),
            ("unitPriceItems", 7),
            ("omittedQuantityItems", 8),
            ("explicitBoostItems", 8),
            ("productLineProofItems", 7),
            ("ambiguousItems", 1),
            ("digitalControlBindingUnprovenItems", 0),
            ("metadataShapeUnrecognizedItems", 0),
            ("unitPriceUnrecognizedItems", 1),
            ("quantityUnrecognizedItems", 0),
            ("boostUnrecognizedItems", 0),
            ("dateProofUnavailableRows", 1),
            ("htmlJa", 2),
            ("htmlEn", 1),
            ("htmlKo", 1),
            ("htmlZhCn", 1),
            ("htmlZhTw", 1),
            ("htmlOther", 1),
            ("htmlUnknown", 1),
        ] {
            assert_eq!(facts[key], expected, "{key}");
        }
        let serialized = trace.to_string();
        for private in [
            "PRIVATE", "900001", "700001", "910001", "https://", "booth.pm",
            "Authored", "2026/10/10", "¥", "137", "authored-700001.zip",
        ] {
            assert!(!serialized.contains(private), "source value {private}");
        }
        assert!(crate::support_trace::snapshot().is_none());
    }
    #[test]
    fn contact_diagnostic_has_fixed_eight_counts_and_closed_context_gates() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let footer = authored_contact("PRIVATE_CUSTOMER");
        let mut cases = vec![
            (
                authored_contact(&"<span>PRIVATE</span>".repeat(25)),
                "structuralGuardRejected",
            ),
            (
                authored_contact(
                    "<a href='https://PRIVATE.invalid/SECRET_BODY_ROUTE'>PRIVATE_OTHER_LINK</a>",
                ),
                "rivalPresenceRejected",
            ),
            (
                footer
                    .replace("<div class='sheet'><div>", "<div class='sheet'><section>")
                    .replace("</div></div></div>", "</div></section></div>"),
                "envelopeStructureRejected",
            ),
            (
                footer.replace("お支払いが完了しました", "PRIVATE_UNKNOWN_STATUS"),
                "instructionRejected",
            ),
            (
                authored_contact("<span>PRIVATE_CUSTOMER</span>"),
                "opaqueBodyStructureRejected",
            ),
            (footer.clone(), "closedContacts"),
            (
                footer.replace(
                    "<div>PRIVATE_CUSTOMER</div>",
                    "<div data-discount='100'>PRIVATE_CUSTOMER</div>",
                ),
                "rivalPresenceRejected",
            ),
            (
                footer.replace(
                    "<div>PRIVATE_CUSTOMER</div>",
                    "<div data-fee='100'>PRIVATE_CUSTOMER</div>",
                ),
                "rivalPresenceRejected",
            ),
            (
                footer.replace("<div class='sheet'>", "<div class='sheet'>PRIVATE_OUTSIDE"),
                "envelopeStructureRejected",
            ),
        ];
        cases.extend(
            [
                "data-id",
                "data-item",
                "data-product",
                "data-product-id",
                "data-item-id",
            ]
            .map(|attribute| {
                (
                    footer.replace(
                        "<div class='sheet'>",
                        &format!("<div class='sheet' {attribute}='900003'>"),
                    ),
                    "rivalPresenceRejected",
                )
            }),
        );
        cases.extend(
            [
                footer.replace("class='sheet'", "class='sheet icon-boost'"),
                footer.replace(
                    "<div class='sheet'>",
                    "<div class='sheet' data-test='filename'>",
                ),
            ]
            .map(|candidate| (candidate, "rivalPresenceRejected")),
        );
        let keys = [
            "version",
            "contactActionMatches",
            "closedContacts",
            "structuralGuardRejected",
            "rivalPresenceRejected",
            "envelopeStructureRejected",
            "instructionRejected",
            "opaqueBodyStructureRejected",
        ];
        for (footer, expected) in cases {
            let page = html.replace("</main>", &format!("{footer}</main>"));
            let value = serde_json::to_value(
                diagnose_receipt_contact_envelope(&page, &saved, "910001").unwrap(),
            )
            .unwrap();
            let object = value.as_object().unwrap();
            assert_eq!(object.len(), 8);
            assert!(object.keys().all(|key| keys.contains(&key.as_str())));
            assert!(object.values().all(serde_json::Value::is_u64));
            assert_eq!(value["version"], 1);
            assert_eq!(value["contactActionMatches"], 1);
            assert_eq!(value[expected], 1, "{expected}");
            assert_eq!(
                keys[2..]
                    .iter()
                    .map(|key| value[*key].as_u64().unwrap())
                    .sum::<u64>(),
                1
            );
            for private in [
                "PRIVATE",
                "SECRET",
                "900001",
                "700001",
                "910001",
                "conversations",
                "ショップ",
                "https://",
                "span",
                "class",
                "2026/",
            ] {
                assert!(!value.to_string().contains(private));
            }
            if expected != "closedContacts" {
                assert_bound_product_price(
                    &parse(&page, "purchase", "/orders/910001").unwrap(),
                    100,
                );
            }
        }
        for unknown in [
            footer.replace("ショップに連絡する", "PRIVATE_OTHER_ACTION"),
            footer.replace(
                "/conversations/authored_token?opaque=PRIVATE_QUERY",
                "/unknown/PRIVATE_PATH",
            ),
            footer.replace(
                "/conversations/authored_token?opaque=PRIVATE_QUERY",
                "https://PRIVATE.invalid/conversations/SECRET_TOKEN",
            ),
        ] {
            let page = html.replace("</main>", &format!("{unknown}</main>"));
            let value = serde_json::to_value(
                diagnose_receipt_contact_envelope(&page, &saved, "910001").unwrap(),
            )
            .unwrap();
            assert_eq!(value["contactActionMatches"], 0);
        }
        assert_eq!(
            diagnose_receipt_contact_envelope("<input type='password'>", &saved, "910001")
                .err()
                .unwrap()
                .to_string(),
            "AUTH_REQUIRED"
        );
        assert_eq!(
            diagnose_receipt_contact_envelope("<div id='challenge-form'></div>", &saved, "910001")
                .err()
                .unwrap()
                .to_string(),
            "AUTH_CHALLENGE"
        );
        assert_eq!(
            diagnose_receipt_contact_envelope(&html, &saved, "910002")
                .err()
                .unwrap()
                .to_string(),
            "DIGITAL_PROOF_UNPROVEN"
        );
        assert_eq!(
            diagnose_receipt_contact_envelope(&format!("<main>{footer}</main>"), &saved, "910001")
                .err()
                .unwrap()
                .to_string(),
            "DIGITAL_PROOF_UNPROVEN"
        );
    }
    #[test]
    fn contact_customer_text_values_never_change_counts_dates_or_proven_amounts() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let bodies = [
            "PRIVATE_CUSTOMER".to_owned(),
            "PRIVATE".repeat(200),
            "  \n\t  ".into(),
            "注文番号 910001 注文日時 1999/01/01 01:01 お支払金額 ¥ 777777".into(),
            "".into(),
            "PRIVATE<!--PRIVATE_COMMENT-->OTHER_PRIVATE_TEXT".into(),
        ];
        let mut outcomes = Vec::new();
        for body in bodies {
            let footer = authored_contact(&body);
            let page = html.replace("</main>", &format!("{footer}</main>"));
            let contact = serde_json::to_value(
                diagnose_receipt_contact_envelope(&page, &saved, "910001").unwrap(),
            )
            .unwrap();
            assert_eq!(contact["closedContacts"], 1);
            let parsed = parse(&page, "purchase", "/orders/910001").unwrap();
            let row = parsed
                .rows
                .iter()
                .find(|row| row.product_id.as_deref() == Some("900001"))
                .unwrap();
            assert_eq!(row.product_line_total_jpy, Some(100));
            assert_eq!(row.acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
            outcomes.push((contact, row.source_record_id.clone()));
        }
        assert!(outcomes.windows(2).all(|pair| pair[0] == pair[1]));
    }
    #[test]
    fn known_contact_opaque_body_is_excluded_before_metadata_links_files_and_legacy_labels() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let labels = "<div><span>注文番号</span><span>910001</span></div><div><span>注文日時</span><span>1999/01/01 01:01</span></div><div><span>お支払金額</span><span>¥ 777777</span></div>";
        let footer = authored_contact(labels);
        for candidate in [
            footer.clone(),
            authored_contact(&format!("<h1>PRIVATE_BODY_HEADING</h1>{labels}")),
            authored_contact(&format!("<h2>PRIVATE_BODY_SUBHEADING</h2><div data-test='page-title'>PRIVATE_BODY_TITLE</div>{labels}")),
            footer.replace("<div class='sheet'><div>", "<div class='sheet'><section>").replace("</div></div></div>", "</div></section></div>"),
            authored_contact(&format!("{labels}<a href='https://PRIVATE.invalid/PRIVATE_BODY_URI'>PRIVATE_BODY_ACTION</a>")),
            authored_contact(&format!("{labels}<a href='https://invented.booth.pm/items/900002'>PRIVATE_FAKE_PRODUCT</a><div data-test='downloadable-list'><b data-test='filename'>PRIVATE.zip</b><a href='https://booth.pm/downloadables/700002'>Download</a></div>")),
        ] {
            let fragment = Html::parse_fragment(&format!("<section data-test='wrapper'>{candidate}</section>"));
            let card = fragment.select(&sel(".sheet")).next().unwrap();
            let prefix = known_contact_prefix(card).unwrap();
            let wrapper = fragment.select(&sel("[data-test='wrapper']")).next().unwrap();
            assert!(!rebound_digital_sheet(card, &saved));
            for node in [card, wrapper, prefix.body] {
                assert!(!template_summary_candidate(node));
                let observation = receipt_summary_observation(node, "910001");
                assert_eq!(observation.number_labels, 0);
                assert_eq!(observation.date_labels, 0);
                assert_eq!(observation.date, None);
            }
            let before = html.replace("<main>", &format!("<main>{candidate}"));
            let parsed = parse(&before, "purchase", "/orders/910001").unwrap();
            assert_eq!(parsed.rows.iter().filter(|row| row.product_id.is_some()).count(), 1);
            assert_eq!(parsed.rows.len(), 1);
            assert_eq!(parsed.rows[0].files.len(), 1);
            assert_eq!(parsed.rows[0].files[0].id, "700001");
            assert_eq!(parsed.rows[0].product_id.as_deref(), Some("900001"));
            assert!(parsed.rows[0].name.as_deref().is_some_and(|name| !name.contains("PRIVATE")));
            assert!(parsed.rows[0].paid_amount_text.is_none());
            assert!(!serde_json::to_string(&parsed).unwrap().contains("PRIVATE"));
            let digital = parsed.rows.iter().find(|row| row.product_id.as_deref() == Some("900001")).unwrap();
            assert_eq!(digital.acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
            assert_eq!(digital.product_line_total_jpy, Some(100));
            let no_summary = parse(&format!("<main>{candidate}{item}</main>"), "purchase", "/orders/910001").unwrap();
            assert!(no_summary.rows.iter().all(|row| row.acquired_date_text.is_none()));
            assert_bound_product_price(&no_summary, 100);
            assert_eq!(diagnose_receipt_contact_envelope(&format!("<main>{candidate}</main>"), &saved, "910001").err().unwrap().to_string(), "DIGITAL_PROOF_UNPROVEN");
        }
        let body_link_only = "<div class='sheet'><div class='text-14'><p>お支払いが完了しました</p><a href='/conversations/PRIVATE_TOKEN'>ショップに連絡する</a><div>PRIVATE_BODY</div></div></div>";
        let fragment = Html::parse_fragment(body_link_only);
        assert!(known_contact_prefix(fragment.select(&sel(".sheet")).next().unwrap()).is_none());
    }
    #[test]
    fn known_notice_diagnostic_reports_only_fixed_first_failure_card_counts() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let rows = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let header = "<div class='text-16 font-bold'>発送通知コメント</div>";
        let body = "<div class='text-14'><p>PRIVATE_NOTICE_BODY 777777</p></div>";
        let notice = authored_notice("<p>PRIVATE_NOTICE_BODY 777777</p>");
        let cases = vec![
            (
                authored_notice(&"<span>PRIVATE</span>".repeat(70)),
                "structuralGuardRejected",
            ),
            (
                notice.replace("<p>", "<p data-id='900003'>"),
                "rivalPresenceRejected",
            ),
            (
                format!("<section class='sheet'>{header}{body}</section>"),
                "outerWrappingRejected",
            ),
            (
                format!(
                    "<div class='sheet'><div>{header}{body}<div>PRIVATE_EXTRA</div></div></div>"
                ),
                "siblingStructureRejected",
            ),
            (
                "<div class='sheet'><b>発送通知コメント</b><p>PRIVATE<br>PRIVATE<br></p></div>".to_string(),
                "bodyRoleRejected",
            ),
            (
                "<div class='sheet'><b>発送通知コメント</b><p>PRIVATE<br>PRIVATE<br>PRIVATE<br>PRIVATE<br></p></div>".to_string(),
                "bodyRoleRejected",
            ),
            (authored_notice("<em>PRIVATE</em>"), "bodyTypeRejected"),
            (
                authored_notice(&format!(
                    "{}PRIVATE{}",
                    "<div>".repeat(8),
                    "</div>".repeat(8)
                )),
                "bodyDepthRejected",
            ),
            (notice.clone(), "closedNotices"),
            (
                authored_notice(&format!("{header}<p>PRIVATE_DUPLICATE</p>")),
                "closedNotices",
            ),
            (
                format!("<div class='sheet'><div>{header}{header}</div></div>"),
                "closedNotices",
            ),
            (
                format!("<div class='sheet'><div>{header}{header}{body}</div></div>"),
                "siblingStructureRejected",
            ),
            (
                notice.replace("<div class='text-14'>", "PRIVATE_OUTSIDE<div class='text-14'>"),
                "siblingStructureRejected",
            ),
        ];
        let keys = [
            "version",
            "noticeHeaderMatches",
            "closedNotices",
            "structuralGuardRejected",
            "rivalPresenceRejected",
            "outerWrappingRejected",
            "siblingStructureRejected",
            "headerPlacementRejected",
            "bodyRoleRejected",
            "bodyTypeRejected",
            "bodyDepthRejected",
        ];
        for (notice, expected) in cases {
            let page = html.replace("</main>", &format!("{notice}</main>"));
            let value = serde_json::to_value(
                diagnose_receipt_notice_envelope(&page, &rows, "910001").unwrap(),
            )
            .unwrap();
            let object = value.as_object().unwrap();
            assert_eq!(object.len(), 11);
            assert!(object.keys().all(|key| keys.contains(&key.as_str())));
            assert!(object.values().all(serde_json::Value::is_u64));
            assert_eq!(value["version"], 1);
            assert_eq!(value["noticeHeaderMatches"], 1);
            assert_eq!(value[expected], 1, "{expected}");
            assert_eq!(
                keys[2..]
                    .iter()
                    .map(|key| value[*key].as_u64().unwrap())
                    .sum::<u64>(),
                1
            );
            for private in [
                "PRIVATE", "777777", "900001", "900003", "700001", "910001", "発送", "https://",
                "class", "span", "2026/",
            ] {
                assert!(!value.to_string().contains(private));
            }
            if expected != "closedNotices" {
                assert_bound_product_price(
                    &parse(&page, "purchase", "/orders/910001").unwrap(),
                    100,
                );
            }
        }
        let unknown = html.replace("</main>", "<div class='sheet'><div class='text-16 font-bold'>PRIVATE_UNKNOWN_HEADER</div><p>PRIVATE</p></div></main>");
        let unknown = serde_json::to_value(
            diagnose_receipt_notice_envelope(&unknown, &rows, "910001").unwrap(),
        )
        .unwrap();
        assert_eq!(unknown["noticeHeaderMatches"], 0);
        assert_eq!(
            unknown
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| *key != "version")
                .map(|(_, value)| value.as_u64().unwrap())
                .sum::<u64>(),
            0
        );
        assert!(diagnose_receipt_notice_envelope(&html, &rows, "910002").is_err());
        assert_eq!(
            diagnose_receipt_notice_envelope("<input type='password'>", &rows, "910001")
                .err()
                .unwrap()
                .to_string(),
            "AUTH_REQUIRED"
        );
    }
    #[test]
    fn unsupported_and_sibling_duplicate_notice_headers_omit_financial_date_nomination() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let body = "<div><span>注文番号</span><span>910001</span></div><div><span>注文日時</span><span>1999/01/01 01:01</span></div><div><span>お支払金額</span><span>¥ 777777</span></div><p>PRIVATE_OPAQUE</p>";
        let valid = authored_notice(body);
        for notice in [
            valid.replace("<p>", "<p data-discount='100'>"),
            valid.replace("<div class='sheet'>", "<div class='sheet icon-boost'>"),
            valid.replace("<p>", "<em>").replace("</p>", "</em>"),
            valid.replace("<div class='text-16 font-bold'>発送通知コメント</div>", "<div class='text-16 font-bold'>発送通知コメント</div><div class='text-16 font-bold'>発送通知コメント</div>"),
        ] {
            let wrapped =
                Html::parse_fragment(&format!("<section data-test='wrapper'>{notice}</section>"));
            let card = wrapped.select(&sel(".sheet")).next().unwrap();
            let wrapper = wrapped
                .select(&sel("[data-test='wrapper']"))
                .next()
                .unwrap();
            let direct_body = card.select(&sel("p,em")).next().unwrap();
            for candidate in [card, wrapper, direct_body] {
                assert!(!template_summary_candidate(candidate));
                let observation = receipt_summary_observation(candidate, "910001");
                assert_eq!(observation.number_labels, 0);
                assert_eq!(observation.date_labels, 0);
                assert_eq!(observation.date, None);
                assert!(!observation.role);
            }
            let before = html.replace("<main>", &format!("<main>{notice}"));
            let parsed = parse(&before, "purchase", "/orders/910001").unwrap();
            let digital = parsed
                .rows
                .iter()
                .find(|row| row.product_id.as_deref() == Some("900001"))
                .unwrap();
            assert_eq!(
                digital.acquired_date_text.as_deref(),
                Some("2026/10/10 09:30")
            );
            assert_eq!(digital.product_line_total_jpy, Some(100));
            let no_summary = parse(
                &format!("<main>{notice}{item}</main>"),
                "purchase",
                "/orders/910001",
            )
            .unwrap();
            assert!(no_summary.rows.iter().all(|row| row.acquired_date_text.is_none()));
            assert_bound_product_price(&no_summary, 100);
        }
    }
    #[test]
    fn notice_semantic_heading_and_known_p_body_preserve_proven_facts() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let mut snapshots = Vec::new();
        let body = "<div>PRIVATE ¥ 777777</div><div class='text-16 font-bold'>発送通知コメント</div><p>PRIVATE</p>";
        for notice in [
            authored_notice(body),
            authored_notice(body).replace("text-16 font-bold", "text-16"),
            authored_notice(body).replace("class='text-14'", "class='unknown-body'"),
            format!("<div class='sheet'><div>発送通知コメント</div><div>{body}</div></div>"),
            format!("<div class='sheet'><span><div><span><b><strong>発送通知コメント</strong></b></span></div><div>{body}</div></span></div>"),
            format!("<div class='sheet'><div><h1><span>発送通知コメント</span></h1><div>{body}</div></div></div>"),
            format!("<div class='sheet'><div><h2>発送通知コメント</h2><div>{body}</div></div></div>"),
            "<div class='sheet'><b>発送通知コメント</b><p>PRIVATE<br>PRIVATE<br>PRIVATE<br>PRIVATE</p></div>".to_string(),
            "<div class='sheet'><span><b>発送通知コメント</b><p>PRIVATE<br><span>PRIVATE<br>PRIVATE<br></span></p></span></div>".to_string(),
        ] {
            let page = html.replace("</main>", &format!("{notice}</main>"));
            let parsed = parse(&page, "purchase", "/orders/910001").unwrap();
            let product = parsed.rows.iter().find(|row| row.product_id.as_deref() == Some("900001")).unwrap();
            assert_eq!(product.product_line_total_jpy, Some(100));
            assert_eq!(product.acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
            let counts = serde_json::to_value(diagnose_receipt_notice_envelope(&page, &saved, "910001").unwrap()).unwrap();
            assert_eq!(counts["noticeHeaderMatches"], 1);
            assert_eq!(counts["closedNotices"], 1);
            snapshots.push((counts, product.source_record_id.clone(),
                product.acquired_date_text.clone(), product.product_line_total_jpy));
        }
        assert!(snapshots.iter().all(|snapshot| snapshot == &snapshots[0]));
    }
    #[test]
    fn notice_prefix_never_retries_unknown_or_text_bearing_body_wrappers() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let header = "<b>発送通知コメント</b>";
        let pair = format!("{header}<div>PRIVATE</div>");
        let mut cases: Vec<_> = [
            format!("<div class='sheet'><div class='text-14'><div>発送通知コメント</div><div>PRIVATE</div></div></div>"),
            format!("<div class='sheet'><div>PRIVATE_BODY<div>{pair}</div></div></div>"),
            format!("<div class='sheet'><div> <div>{pair}</div></div></div>"),
            format!("<div class='sheet'><div><span>PRIVATE_UNKNOWN_HEADING</span><div>{pair}</div></div></div>"),
            format!("<div class='sheet'><div><div>{pair}</div><div>PRIVATE</div></div></div>"),
            format!("<div class='sheet'><div><span>PRIVATE_UNKNOWN_HEADING</span><div>PRIVATE</div>{pair}</div></div>"),
            format!("<div class='sheet'><section>{pair}</section></div>"),
            "<div class='sheet'><div>発送通知コメント</div><p>PRIVATE<br><br><br></p></div>".to_string(),
            "<div class='sheet'><b><span>発送通知コメント</span></b><p>PRIVATE<br><br><br></p></div>".to_string(),
            format!("<div class='sheet'>{header}<section>PRIVATE<br><br><br></section></div>"),
            format!("<div class='sheet'><b data-id='900003'>発送通知コメント</b><div>PRIVATE</div></div>"),
            // Relative links resolve to the account host, which is not an item host.
            format!("<div class='sheet'><b><a href='/items/900003'>発送通知コメント</a></b><div>PRIVATE</div></div>"),
        ].into_iter().collect();
        cases.push(format!("<div class='sheet'><b><a href='https://booth.pm/ja/items/900003'>発送通知コメント</a></b><div>PRIVATE</div></div>"));
        for notice in cases {
            let fragment = Html::parse_fragment(&notice);
            assert!(known_notice_prefix(fragment.select(&sel(".sheet")).next().unwrap()).is_none());
            let page = html.replace("</main>", &format!("{notice}</main>"));
            let counts = serde_json::to_value(
                diagnose_receipt_notice_envelope(&page, &saved, "910001").unwrap(),
            )
            .unwrap();
            assert_eq!(counts["noticeHeaderMatches"], 0);
            assert_eq!(counts["closedNotices"], 0);
            assert_bound_product_price(&parse(&page, "purchase", "/orders/910001").unwrap(), 100);
        }
    }
    #[test]
    fn opaque_styled_notice_body_never_supplies_a_header_or_changes_proven_facts() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let mut positive = Vec::new();
        let mut body_only = Vec::new();
        for literal in ["PRIVATE_OPAQUE_STYLED_BODY", "発送通知コメント"] {
            let body = format!(
                "<div class='text-16 font-bold'>{literal}</div><p>PRIVATE_BODY_TEXT ¥ 777777</p>"
            );
            let notice = authored_notice(&body);
            let page = html.replace("</main>", &format!("{notice}</main>"));
            let parsed = parse(&page, "purchase", "/orders/910001").unwrap();
            let product = parsed
                .rows
                .iter()
                .find(|row| row.product_id.as_deref() == Some("900001"))
                .unwrap();
            assert_eq!(product.product_line_total_jpy, Some(100));
            assert_eq!(
                product.acquired_date_text.as_deref(),
                Some("2026/10/10 09:30")
            );
            let notice_counts = serde_json::to_value(
                diagnose_receipt_notice_envelope(&page, &saved, "910001").unwrap(),
            )
            .unwrap();
            assert_eq!(notice_counts["noticeHeaderMatches"], 1);
            assert_eq!(notice_counts["closedNotices"], 1);
            positive.push((notice_counts, product.source_record_id.clone()));
            let body_first = format!("<div class='sheet'><div class='text-14'>{body}</div></div>");
            let fragment = Html::parse_fragment(&body_first);
            let card = fragment.select(&sel(".sheet")).next().unwrap();
            assert!(known_notice_header(card).is_none());
            let page = html.replace("</main>", &format!("{body_first}</main>"));
            let counts = serde_json::to_value(
                diagnose_receipt_notice_envelope(&page, &saved, "910001").unwrap(),
            )
            .unwrap();
            assert_eq!(counts["noticeHeaderMatches"], 0);
            assert_eq!(counts["closedNotices"], 0);
            assert_bound_product_price(&parse(&page, "purchase", "/orders/910001").unwrap(), 100);
            let body_before_header = format!("<div class='sheet'><div><div class='text-14'>{body}</div><div class='text-16 font-bold'>発送通知コメント</div></div></div>");
            let fragment = Html::parse_fragment(&body_before_header);
            assert!(known_notice_header(fragment.select(&sel(".sheet")).next().unwrap()).is_none());
            let swapped = html.replace("</main>", &format!("{body_before_header}</main>"));
            assert_eq!(
                serde_json::to_value(
                    diagnose_receipt_notice_envelope(&swapped, &saved, "910001").unwrap()
                )
                .unwrap(),
                counts
            );
            body_only.push(counts);
        }
        assert_eq!(positive[0], positive[1]);
        assert_eq!(body_only[0], body_only[1]);
    }
    #[test]
    fn independent_product_fields_ignore_unrelated_ui_and_infer_only_known_digital_quantity() {
        let explicit = charge_item("900001", "700001", "¥ 100", Some("数量: 2"), "30").replace(
            "</section>",
            "</section><button>PRIVATE_UNRELATED_ACTION</button><p>PRIVATE_UNRELATED_TEXT</p>",
        );
        let omitted = charge_item("900001", "700001", "¥ 100", None, "30");
        let unknown = "<div class='sheet'><p>PRIVATE_UNKNOWN_UI ¥ 999999</p></div><nav><button>PRIVATE_NAV</button></nav>";
        for (item, amount, explicit_count, omitted_count) in
            [(explicit, 230, 1, 0), (omitted, 130, 0, 1)]
        {
            let html = format!("<main>{item}{unknown}</main>");
            let page = parse(&html, "purchase", "/orders/910001").unwrap();
            assert_bound_product_price(&page, amount);
            assert!(page.rows.iter().all(|row| row.acquired_date_text.is_none()));
            let proof = product_line_proof(&html, &page.rows, "910001").unwrap();
            assert_eq!(proof.charges.len(), 1);
            assert_eq!(proof.charges[0].amount, amount);
            assert_eq!(proof.diagnostics.explicit_quantity_items, explicit_count);
            assert_eq!(proof.diagnostics.omitted_quantity_items, omitted_count);
            let counts = serde_json::to_value(
                diagnose_product_line_fields(&html, &page.rows, "910001", true).unwrap(),
            )
            .unwrap();
            let keys = [
                "version",
                "savedReceiptWitness",
                "digitalBoundItems",
                "uniqueMetadataItems",
                "unitPriceItems",
                "explicitQuantityItems",
                "omittedQuantityItems",
                "explicitBoostItems",
                "productLineProofItems",
                "ambiguousItems",
                "digitalControlBindingUnprovenItems",
                "metadataShapeUnrecognizedItems",
                "unitPriceUnrecognizedItems",
                "quantityUnrecognizedItems",
                "boostUnrecognizedItems",
            ];
            assert_eq!(counts.as_object().unwrap().len(), keys.len());
            assert!(counts
                .as_object()
                .unwrap()
                .keys()
                .all(|key| keys.contains(&key.as_str())));
            assert_eq!(counts["version"], 1);
            assert_eq!(counts["savedReceiptWitness"], true);
            assert_eq!(counts["digitalBoundItems"], 1);
            assert_eq!(counts["uniqueMetadataItems"], 1);
            assert_eq!(counts["unitPriceItems"], 1);
            assert_eq!(counts["explicitBoostItems"], 1);
            assert_eq!(counts["productLineProofItems"], 1);
            assert_eq!(counts["ambiguousItems"], 0);
            for key in [
                "digitalControlBindingUnprovenItems", "metadataShapeUnrecognizedItems",
                "unitPriceUnrecognizedItems", "quantityUnrecognizedItems", "boostUnrecognizedItems",
            ] {
                assert_eq!(counts[key], 0, "{key}");
            }
            assert!(counts.as_object().unwrap().iter().all(|(key, value)| {
                if key == "savedReceiptWitness" {
                    value.is_boolean()
                } else {
                    value.is_u64()
                }
            }));
            for private in [
                "PRIVATE", "999999", "900001", "700001", "910001", "https://", "Authored",
            ] {
                assert!(!counts.to_string().contains(private));
            }
            let changed = html.replace(
                "PRIVATE_UNKNOWN_UI ¥ 999999",
                "PRIVATE_OTHER_UNRELATED_VALUE",
            );
            assert_eq!(
                serde_json::to_value(
                    diagnose_product_line_fields(&changed, &page.rows, "910001", true).unwrap()
                )
                .unwrap(),
                counts
            );
        }
    }
    #[test]
    fn five_locale_receipts_bind_english_summary_without_changing_acquisition_identity() {
        let japanese = charge_receipt(
            &charge_item("900001", "700001", "¥ 137", None, "¥ 30"), "¥ 999999", "",
        ).replace("2026/10/10 09:30", "2026/10/10 09:30:12");
        let baseline = parse(&japanese, "purchase", "/orders/910001").unwrap();
        let international = japanese
            .replace("¥ 137", "137 JPY")
            .replace("¥ 30", "30 JPY")
            .replace("¥ 999999", "999999 JPY")
            .replace("注文番号", "Order Number")
            .replace("注文日時", "Created At");
        for (language, receipt) in [
            ("ja", &japanese), ("en", &international), ("ko", &international),
            ("zh-CN", &international), ("zh-TW", &international),
        ] {
            let html = format!("<html lang='{language}'><body>{receipt}</body></html>");
            let page = parse(&html, "purchase", "/orders/910001").unwrap();
            assert_bound_product_price(&page, 167);
            assert_eq!(page.rows[0].acquired_date_text.as_deref(), Some("2026/10/10 09:30:12"));
            assert_eq!(serde_json::to_value(page).unwrap(), serde_json::to_value(&baseline).unwrap());
        }
        for receipt in [
            international.replace("<span>910001</span>", "<span>910002</span>"),
            international.replace("Created At", "PRIVATE_UNKNOWN_DATE_LABEL"),
            international.replace("<span>Created At</span>", "<span>Created At</span><span>Created At</span>"),
            international.replace("2026/10/10 09:30", "2026/13/10 09:30"),
            international.replace("</main>", "<div class='sheet'><div><span>Order Number</span><span>910001</span></div><div><span>Created At</span><span>2026/10/11 09:30</span></div></div></main>"),
        ] {
            let page = parse(&receipt, "purchase", "/orders/910001").unwrap();
            assert_bound_product_price(&page, 167);
            assert_eq!(page.rows[0].acquired_date_text, None);
            assert_eq!(page.rows[0].source_record_id, baseline.rows[0].source_record_id);
            assert_eq!(page.rows[0].product_id, baseline.rows[0].product_id);
            assert_eq!(serde_json::to_value(&page.rows[0].files).unwrap(), serde_json::to_value(&baseline.rows[0].files).unwrap());
        }
    }
    #[test]
    fn receipt_jpy_code_preserves_bound_totals_and_rejects_other_amount_spellings() {
        for (quantity, amount) in [(None, 167), (Some("Quantity: 2"), 304)] {
            let japanese = charge_receipt(
                &charge_item("900001", "700001", "¥ 137", quantity, "¥ 30"),
                "¥ 999999",
                "",
            );
            let international = charge_receipt(
                &charge_item("900001", "700001", "137 JPY", quantity, "30 JPY"),
                "999999 JPY",
                "",
            );
            let expected = parse(&japanese, "purchase", "/orders/910001").unwrap();
            let parsed = parse(&international, "purchase", "/orders/910001").unwrap();
            assert_bound_product_price(&parsed, amount);
            assert_eq!(serde_json::to_value(parsed).unwrap(), serde_json::to_value(expected).unwrap());
        }
        for (unit, boost) in [
            ("137 USD", "30 JPY"),
            ("137 JPY", "30 USD"),
            ("¥ 137 JPY", "30 JPY"),
            ("137 JPY", ""),
            ("137.0 JPY", "30 JPY"),
            ("137 JPY ~ 999 JPY", "30 JPY"),
        ] {
            let html = charge_receipt(
                &charge_item("900001", "700001", unit, None, boost), "999999 JPY", "",
            );
            let page = parse(&html, "purchase", "/orders/910001").unwrap();
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.rows[0].product_line_total_jpy, None);
            assert_eq!(page.rows[0].acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
            assert_eq!(page.rows[0].files.len(), 1);
        }
    }
    #[test]
    fn product_field_rejection_counts_describe_existing_decisions_without_source_values() {
        let item = charge_item("900001", "700001", "¥ 100", Some("Quantity: 2"), "30");
        let html = charge_receipt(&item, "¥ 230", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        let reasons = [
            "digitalControlBindingUnprovenItems", "metadataShapeUnrecognizedItems",
            "unitPriceUnrecognizedItems", "quantityUnrecognizedItems", "boostUnrecognizedItems",
        ];
        for (changed, expected_reason) in [
            (html.replace("700001", "700002"), reasons[0]),
            (html.replace("<div>¥ 100</div>", "<div>¥ 100</div><div>PRIVATE_EXTRA_FIELD</div>"), reasons[1]),
            (html.replace("<div>¥ 100</div>", "<div>PRIVATE_UNIT_VALUE</div>"), reasons[2]),
            (html.replace("Quantity: 2", "PRIVATE_QUANTITY_VALUE"), reasons[3]),
            (html.replace("<span>30</span>", "<span>PRIVATE_BOOST_VALUE</span>"), reasons[4]),
        ] {
            let proof = product_line_proof(&changed, &saved, "910001").unwrap();
            assert!(proof.charges.is_empty());
            assert_eq!(proof.diagnostics.ambiguous_items, 1);
            let counts = serde_json::to_value(&proof.diagnostics).unwrap();
            for reason in reasons {
                assert_eq!(counts[reason], usize::from(reason == expected_reason), "{reason}");
            }
            for private in ["PRIVATE", "900001", "700001", "700002", "910001", "https://", "Authored", "¥"] {
                assert!(!counts.to_string().contains(private), "{private}");
            }
        }
        let mut unavailable = saved;
        unavailable.iter_mut().find(|row| row.product_id.is_some()).unwrap().unavailable = true;
        let proof = product_line_proof(&html, &unavailable, "910001").unwrap();
        assert!(proof.charges.is_empty());
        assert_eq!(proof.diagnostics.digital_control_binding_unproven_items, 1);
        // Eligibility failures did not previously enter the ambiguous-item denominator.
        assert_eq!(proof.diagnostics.ambiguous_items, 0);
    }
    #[test]
    fn independent_product_fields_reject_bad_needed_values_and_duplicate_binding() {
        let item = charge_item("900001", "700001", "¥ 100", Some("数量: 2"), "30");
        let html = charge_receipt(&item, "¥ 230", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        for changed in [
            item.replace("¥ 100", "USD 100"), item.replace("¥ 100", "¥ 100 - 20"),
            item.replace("¥ 100", "¥ 9007199254740992"),
            item.replace("¥ 100", "¥ 9007199254740991"),
            item.replace("数量: 2", "数量: 0"), item.replace("数量: 2", "数量: -2"),
            item.replace("数量: 2", "数量: 2.5"), item.replace("数量: 2", "PRIVATE_UNKNOWN_QUANTITY"),
            item.replace("<i class='icon-boost'></i>", ""),
            item.replace("<span>30</span>", "<span>PRIVATE 30</span>"),
            item.replace("<div>¥ 100</div>", "<div data-discount='20'>¥ 100</div>"),
            item.replace("<div>¥ 100</div>", "<div>¥ 100</div><div>¥ 999</div>"),
            item.replace("</b></div>", "</b><b><a href='https://invented.booth.pm/items/900001'>Authored 900001</a></b></div>"),
        ] {
            let page = parse(&format!("<main>{changed}</main>"), "purchase", "/orders/910001").unwrap();
            assert!(page.rows.iter().all(|row| row.product_line_total_jpy.is_none()));
            assert!(product_line_proof(&format!("<main>{changed}</main>"), &page.rows, "910001").unwrap().charges.is_empty());
        }
        let mut duplicate = saved.clone();
        duplicate.push(
            saved
                .iter()
                .find(|row| row.product_id.is_some())
                .unwrap()
                .clone(),
        );
        assert!(product_line_proof(&html, &duplicate, "910001")
            .unwrap()
            .charges
            .is_empty());
        let stale = html.replace("700001", "700002");
        assert!(product_line_proof(&stale, &saved, "910001")
            .unwrap()
            .charges
            .is_empty());
        assert!(product_line_proof(&html, &saved, "910002")
            .unwrap()
            .charges
            .is_empty());
        for case in 0..3 {
            let mut invalid = saved.clone();
            let row = invalid
                .iter_mut()
                .find(|row| row.product_id.is_some())
                .unwrap();
            match case {
                0 => row.files[0].id = "700099".into(),
                1 => row.unavailable = true,
                2 => row.error_code = Some("FILE_INCOMPLETE".into()),
                _ => unreachable!(),
            }
            assert!(product_line_proof(&html, &invalid, "910001")
                .unwrap()
                .charges
                .is_empty());
        }
    }
    #[test]
    fn independent_product_fields_keep_zero_and_only_proven_products() {
        for boost in ["0", "30"] {
            let html = format!(
                "<main>{}</main>",
                charge_item("900001", "700001", "¥ 0", None, boost)
            );
            assert_bound_product_price(
                &parse(&html, "purchase", "/orders/910001").unwrap(),
                boost.parse().unwrap(),
            );
        }
        let first = charge_item("900001", "700001", "¥ 100", Some("数量: 2"), "30");
        let second = charge_item(
            "900002",
            "700002",
            "PRIVATE_BAD_UNIT",
            Some("数量: 2"),
            "30",
        );
        let html = format!("<main>{first}{second}</main>");
        let page = parse(&html, "purchase", "/orders/910001").unwrap();
        assert_eq!(
            page.rows
                .iter()
                .filter(|row| row.product_id.is_some())
                .map(|row| row.product_line_total_jpy)
                .collect::<Vec<_>>(),
            vec![Some(230), None]
        );
        let duplicate_product =
            format!("<main>{first}{}</main>", first.replace("700001", "700003"));
        assert!(parse(&duplicate_product, "purchase", "/orders/910001")
            .unwrap()
            .rows
            .iter()
            .all(|row| row.product_line_total_jpy.is_none()));
        let conflicting_file = format!(
            "<main>{first}{}</main>",
            charge_item("900002", "700001", "¥ 100", None, "30")
        );
        assert!(parse(&conflicting_file, "purchase", "/orders/910001")
            .unwrap()
            .rows
            .iter()
            .all(|row| row.product_line_total_jpy.is_none()));
    }
    #[test]
    fn independent_product_fields_bind_multiple_exact_owned_variants_without_amount_allocation() {
        let first = charge_item("900001", "700001", "¥ 100", Some("数量: 2"), "30").replace(
            "/downloadables/700001'",
            "/downloadables/700001?variation_id=800001'",
        );
        let second = charge_item("900001", "700002", "¥ 0", None, "0").replace(
            "/downloadables/700002'",
            "/downloadables/700002?variation_id=800002'",
        );
        let page = parse(
            &format!("<main>{first}{second}</main>"),
            "purchase",
            "/orders/910001",
        )
        .unwrap();
        assert_eq!(page.rows.len(), 2);
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.product_line_total_jpy)
                .collect::<Vec<_>>(),
            vec![Some(230), Some(0)]
        );
        assert_ne!(page.rows[0].source_record_id, page.rows[1].source_record_id);
        let ambiguous = second.replace("800002", "800001");
        assert!(parse(
            &format!("<main>{first}{ambiguous}</main>"),
            "purchase",
            "/orders/910001"
        )
        .unwrap()
        .rows
        .iter()
        .all(|row| row.product_line_total_jpy.is_none()));
    }
    #[test]
    fn needed_financial_fields_survive_missing_title_with_exact_digital_binding() {
        for (unit, quantity, boost, amount) in
            [("¥ 0", None, "0", 0), ("¥ 100", Some("数量: 2"), "30", 230)]
        {
            let item = charge_item("900001", "700001", unit, quantity, boost)
                .replace("Authored 900001", "");
            let html = charge_receipt(&item, &format!("¥ {amount}"), "");
            let page = parse(&html, "purchase", "/orders/910001").unwrap();
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.unresolved, 1);
            let row = &page.rows[0];
            assert!(row.name.is_none());
            assert_eq!(row.error_code.as_deref(), Some("PRODUCT_TITLE_UNRESOLVED"));
            assert!(!row.manifest_complete);
            assert!(diagnostic_digital_file_keys(row).is_none());
            assert!(product_field_file_keys(row).is_some());
            assert_eq!(row.acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
            assert_eq!(row.product_line_total_jpy, Some(amount));
            let mut invalid = row.clone();
            for code in ["PRODUCT_ID_UNRESOLVED", "DIGITAL_CONTROLS_UNRESOLVED"] {
                invalid.error_code = Some(code.into());
                assert!(product_field_file_keys(&invalid).is_none());
            }
            invalid.error_code = row.error_code.clone();
            invalid.unavailable = true;
            assert!(product_field_file_keys(&invalid).is_none());
            let duplicate = charge_receipt(
                &format!("{item}{}", item.replace("700001", "700002")),
                &format!("¥ {}", amount * 2),
                "",
            );
            assert!(parse(&duplicate, "purchase", "/orders/910001")
                .unwrap()
                .rows
                .iter()
                .all(|row| row.product_line_total_jpy.is_none()));
        }
    }
    #[test]
    fn needed_fields_survive_missing_filename_and_unrelated_receipt_regions() {
        let item = charge_item("900001", "700001", "¥ 100", Some("数量: 2"), "30");
        let complete_html = charge_receipt(&item, "¥ 230", "");
        let complete = parse(&complete_html, "purchase", "/orders/910001").unwrap();
        let html = complete_html
            .replace("<b data-test='filename'>authored-700001.zip</b>", "")
            .replace(
                "</main>",
                "<div class='sheet'><p>UNRELATED_UNKNOWN_REGION ¥ 999999</p></div></main>",
            );
        let page = parse(&html, "purchase", "/orders/910001").unwrap();
        let row = &page.rows[0];
        assert_eq!(page.unresolved, 0);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(row.source_record_id, complete.rows[0].source_record_id);
        assert_eq!(row.product_line_total_jpy, Some(230));
        assert_eq!(row.acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
        assert!(!row.manifest_complete);
        assert!(product_field_file_keys(row).is_some());
        assert!(diagnostic_digital_file_keys(row).is_none());
        let broken = html.replace("</main>", "<div class='sheet'><a data-test='downloadable' href='https://evil.test/downloadables/700003'>Download</a></div></main>");
        let broken_page = parse(&broken, "purchase", "/orders/910001").unwrap();
        assert!(broken_page.unresolved > 0);
        assert_eq!(broken_page.rows[0].product_line_total_jpy, Some(230));
        assert_eq!(
            broken_page.rows[0].acquired_date_text.as_deref(),
            Some("2026/10/10 09:30")
        );
        let unknown = broken_page
            .rows
            .iter()
            .find(|row| row.product_id.is_none())
            .unwrap();
        assert_eq!(unknown.order_id.as_deref(), Some("910001"));
        assert_eq!(
            unknown.error_code.as_deref(),
            Some("DIGITAL_CONTROLS_UNRESOLVED")
        );
        assert!(
            unknown.name.is_none()
                && unknown.paid_amount_text.is_none()
                && unknown.files.is_empty()
        );
        let mixed = html.replace("<div data-test='downloadable-list'>", "<div data-test='downloadable-list'><a href='https://evil.test/downloadables/700003'>Download</a>");
        let mixed_page = parse(&mixed, "purchase", "/orders/910001").unwrap();
        assert!(mixed_page.unresolved > 0);
        assert!(mixed_page.rows[0].product_line_total_jpy.is_none());
    }
    #[test]
    fn native_paid_producer_joins_exact_rows_including_zero_without_changing_identity() {
        let items = format!(
            "{}{}",
            charge_item("900001", "700001", "¥ 0", None, "0"),
            charge_item("900002", "700002", "¥ 200", Some("数量: 2"), "30")
        );
        let html = charge_receipt(&items, "¥ 430", "");
        let page = parse(&html, "purchase", "/orders/910001").unwrap();
        let digital: Vec<_> = page
            .rows
            .iter()
            .filter(|row| row.product_id.is_some())
            .collect();
        assert_eq!(
            digital
                .iter()
                .map(|row| row.product_line_total_jpy)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(430)]
        );
        let unsupported = parse(
            &html.replace("¥ 430", "¥ 429"),
            "purchase",
            "/orders/910001",
        )
        .unwrap();
        assert_eq!(
            unsupported
                .rows
                .iter()
                .filter(|row| row.product_id.is_some())
                .map(|row| row.product_line_total_jpy)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(430)]
        );
        assert_eq!(
            page.rows
                .iter()
                .map(|row| &row.source_record_id)
                .collect::<Vec<_>>(),
            unsupported
                .rows
                .iter()
                .map(|row| &row.source_record_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(page.fingerprint, unsupported.fingerprint);
        let mut changed = page.rows.clone();
        for row in &mut changed {
            row.product_line_total_jpy = None;
        }
        let proof = product_line_proof(&html, &changed, "910001").unwrap();
        changed[0].source_record_id.push_str("-different");
        apply_product_line_charges(&mut changed, proof, "910001");
        assert_eq!(
            changed
                .iter()
                .map(|row| row.product_line_total_jpy)
                .collect::<Vec<_>>(),
            vec![None, Some(430)]
        );
        let shelf_html = html.replace("<main>", "<main><h1>Library</h1>");
        let shelf = parse(&shelf_html, "library", "/library?page=1").unwrap();
        assert_eq!(
            shelf
                .rows
                .iter()
                .filter(|row| row.product_id.is_some())
                .count(),
            2
        );
        assert!(shelf
            .rows
            .iter()
            .all(|row| row.product_line_total_jpy.is_none()));
    }
    #[test]
    fn old_row_json_defaults_paid_proof_to_none_and_omits_the_new_key() {
        let original = Row {
            source_record_id: "purchase:authored".into(),
            ..Row::default()
        };
        let value = serde_json::to_value(&original).unwrap();
        assert!(value.get("productLineTotalJpy").is_none());
        let restored: Row = serde_json::from_value(value.clone()).unwrap();
        assert!(restored.product_line_total_jpy.is_none());
        assert_eq!(serde_json::to_value(restored).unwrap(), value);
    }
    #[test]
    fn receipt_notice_body_cannot_supply_price_or_acquisition_date() {
        let item = charge_item("900001", "700001", "¥ 100", None, "0");
        let html = charge_receipt(&item, "¥ 100", "");
        let saved = parse(&html, "purchase", "/orders/910001").unwrap().rows;
        for body in [
            "PRIVATE_MESSAGE ¥ -100, quantity 2",
            "注文番号 910001 注文日時 1999/01/01 01:01 お支払金額 ¥ 777777",
        ] {
            let notice = authored_notice(body);
            let with_notice = html.replace("</main>", &format!("{notice}</main>"));
            let parsed = parse(&with_notice, "purchase", "/orders/910001").unwrap();
            assert_bound_product_price(&parsed, 100);
            assert_eq!(parsed.rows[0].acquired_date_text.as_deref(), Some("2026/10/10 09:30"));
            let diagnostic = serde_json::to_value(
                diagnose_receipt_notice_envelope(&with_notice, &saved, "910001").unwrap(),
            ).unwrap();
            assert_eq!(diagnostic["closedNotices"], 1);
            assert!(!diagnostic.to_string().contains("PRIVATE"));
            assert!(!serde_json::to_string(&parsed).unwrap().contains("PRIVATE"));
            let without_summary = format!("<main>{item}{notice}</main>");
            let undated = parse(&without_summary, "purchase", "/orders/910001").unwrap();
            assert!(undated.rows.iter().all(|row| row.acquired_date_text.is_none()));
            assert_bound_product_price(&undated, 100);
        }
    }
    const TEMPLATE_HTML:&str="<main><div class='sheet' data-product-id='900001'><div><a href='https://invented.booth.pm/items/900001'><img src='https://booth.pximg.net/authored.png'></a><a href='https://invented.booth.pm/items/900001'> Authored \n  product </a><a href='https://invented.booth.pm/'>Authored shop</a><div><span>￥１，５００</span> ｘ <small>１</small></div><b>BOOST↑</b></div><div data-test='downloadable-list'><b data-test='filename'>authored.zip</b><a href='https://booth.pm/downloadables/700001?variation_id=800001'>Download</a></div></div><div class='sheet'><div><p><b>注文番号</b></p><div># 910001</div></div><div><p><strong>注文日時</strong></p><div>2026/10/10 09:30</div></div></div><div class='sheet'><p>PRIVATE_UNBOUND_SENTINEL</p><div>PRIVATE_ADDRESS_VALUE</div></div></main>";
    #[test]
    fn receipt_dropdown_proves_owned_variant_without_persisting_extra_url_parameters() {
        let dropdown = serde_json::to_string(&serde_json::json!([
            {"text":"Download","deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?variation_id=800001&client=booth-library-manager","fallbackUrl":"https://booth.pm/","fallbackMessage":""},
            {"text":"Other app","deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?variation_id=800001&client=vroid&type=zip","fallbackUrl":"https://example.invalid/","fallbackMessage":""}
        ])).unwrap();
        let body = format!("<main><div class='sheet sheet--p400'><div class='desktop:flex-1'><div class='text-14'><b><a class='nav' href='https://invented.booth.pm/items/900001'>Authored product (Full Pack)</a></b></div></div><div class='legacy-list-item'><div class='min-w-0'><b>authored.zip</b></div><button data-test='other-downloads-button' data-dropdown-items='{dropdown}'></button></div></div></main>");
        let page = parse(&body, "purchase", "/orders/910001").unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].product_id.as_deref(), Some("900001"));
        assert_eq!(page.rows[0].files.len(), 1);
        assert_eq!(page.rows[0].files[0].id, "700001");
        assert_eq!(page.rows[0].files[0].variant_id.as_deref(), Some("800001"));
        assert_eq!(page.rows[0].files[0].name.as_deref(), Some("authored.zip"));
        assert_eq!(page.rows[0].files[0].source_url, "https://booth.pm/downloadables/700001?variation_id=800001");
        assert_eq!(page.rows[0].manifest_complete, true);
    }
    #[test]
    fn receipt_dropdowns_cover_every_file_without_counting_alternate_methods_twice() {
        let mut controls = String::new();
        for offset in 0..7 {
            let file = 700001 + offset;
            let dropdown = serde_json::to_string(&serde_json::json!([
                {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id=800001&client=booth-library-manager")},
                {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id=800001&client=vroid&type=zip")}
            ])).unwrap();
            controls.push_str(&format!("<div class='legacy-list-item'><div class='min-w-0'><b>authored-{offset}.zip</b></div><div><div><div><div><div><a href='https://booth.pm/downloadables/{file}'>Download</a><button data-test='other-downloads-button' data-dropdown-items='{dropdown}'></button></div></div></div></div></div></div>"));
        }
        let html = format!("<main><div class='sheet'><a href='https://invented.booth.pm/items/900001'>Authored product</a>{controls}</div></main>");
        let page = parse(&html, "purchase", "/orders/910001").unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].files.len(), 7);
        assert!(page.rows[0].files.iter().all(|file| file.variant_id.as_deref() == Some("800001")));
        assert!(page.rows[0].files.iter().all(|file| file.name.is_some() && !file.source_url.contains("deeplink") && !file.source_url.contains("client")));
        assert!(page.rows[0].manifest_complete);
        assert_eq!(page.unresolved, 0);
    }
    #[test]
    fn receipt_control_pairing_is_row_local_and_preserves_distinct_owned_variants() {
        let row = |primary: &str, alternate: &str, variant: &str| {
            let items = serde_json::to_string(&serde_json::json!([
                {"deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{alternate}/deeplink?variation_id={variant}&client=booth-library-manager")}
            ])).unwrap();
            format!("<div class='legacy-list-item'><div class='min-w-0'><b>authored-{variant}.zip</b></div><a href='https://booth.pm/downloadables/{primary}'>Download</a><button data-test='other-downloads-button' data-dropdown-items='{items}'></button></div>")
        };
        let receipt = |rows: String| format!("<main><div class='sheet'><a href='https://invented.booth.pm/items/900001'>Authored product</a>{rows}</div></main>");
        let page = parse(&receipt(row("700001", "700001", "800001") + &row("700001", "700001", "800002")), "purchase", "/orders/910001").unwrap();
        assert!(page.rows[0].manifest_complete);
        assert_eq!(page.rows[0].files.len(), 2);
        assert_eq!(page.rows[0].files.iter().filter_map(|file|file.variant_id.as_deref()).collect::<BTreeSet<_>>(), BTreeSet::from(["800001", "800002"]));
        let mismatched = parse(&receipt(row("700001", "700002", "800001")), "purchase", "/orders/910001").unwrap();
        assert!(!mismatched.rows[0].manifest_complete);
        assert_eq!(mismatched.rows[0].files.len(), 2);
        assert!(mismatched.rows[0].files.iter().any(|file|file.id == "700001" && file.variant_id.is_none()));
    }
    #[test]
    fn library_alternates_bind_only_an_observed_primary_within_its_filename_row() {
        let items = serde_json::to_string(&serde_json::json!([
            {"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?variation_id=800001&client=booth-library-manager"},
            {"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?variation_id=800001&client=vroid&type=zip"}
        ])).unwrap();
        for (class, filename) in [
            ("legacy-list-item", "<div class='min-w-0'><b>authored.zip</b></div>"),
            ("file-row", "<div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>authored.zip</div></div>"),
        ] {
            let html = format!("<main><h1>Library</h1><article data-test='library-item'><a href='https://invented.booth.pm/items/900001'>Authored product</a><div data-test='downloadable-list'><div class='{class}'>{filename}<span data-size-bytes='512'></span><a data-revision='v1' href='https://booth.pm/downloadables/700001'>Download</a><button data-test='other-downloads-button' data-dropdown-items='{items}'></button></div></div></article></main>");
            let page = parse(&html, "library", "/library?page=1").unwrap();
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.rows[0].files.len(), 1);
            let file = &page.rows[0].files[0];
            assert_eq!(file.id, "700001");
            assert_eq!(file.variant_id.as_deref(), Some("800001"));
            assert_eq!(file.name.as_deref(), Some("authored.zip"));
            assert_eq!(file.size_bytes, Some(512));
            assert_eq!(file.revision, "source:v1");
            assert_eq!(file.source_url, "https://booth.pm/downloadables/700001?variation_id=800001");
            assert!(page.rows[0].manifest_complete);
            assert_eq!(page.unresolved, 0);
        }
    }
    #[test]
    fn library_alternates_cannot_add_files_or_guess_ambiguous_variants() {
        let alternate = |pairs: &[(&str, &str)]| {
            let items: Vec<_> = pairs.iter().map(|(file, variant)| serde_json::json!({
                "deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id={variant}&client=vroid")
            })).collect();
            format!("<button data-test='other-downloads-button' data-dropdown-items='{}'></button>", serde_json::to_string(&items).unwrap())
        };
        let primary = "<a data-revision='v1' href='https://booth.pm/downloadables/700001'>Download</a>";
        let file_row = |name: &str, primary: &str, alternate: String| format!("<div class='legacy-list-item'><div class='min-w-0'><b>{name}</b></div>{primary}{alternate}</div>");
        let library = |rows: String| format!("<main><h1>Library</h1><article data-test='library-item'><a href='https://invented.booth.pm/items/900001'>Authored product</a><div data-test='downloadable-list'>{rows}</div></article></main>");
        for pairs in [vec![("700002", "800001")], vec![("700001", "800001"), ("700001", "800002")]] {
            let page = parse(&library(file_row("authored.zip", primary, alternate(&pairs))), "library", "/library?page=1").unwrap();
            assert_eq!(page.rows[0].files.len(), 1);
            assert_eq!(page.rows[0].files[0].id, "700001");
            assert_eq!(page.rows[0].files[0].variant_id, None);
        }
        let unobserved = parse(&library(file_row("authored.zip", "", alternate(&[("700001", "800001")]))), "library", "/library?page=1").unwrap();
        assert!(unobserved.rows[0].files.is_empty());
        assert!(!unobserved.rows[0].manifest_complete);
        let separate = file_row("authored-a.zip", primary, alternate(&[("700001", "800001")]))
            + &file_row("authored-b.zip", primary, alternate(&[("700001", "800002")]));
        let page = parse(&library(separate), "library", "/library?page=1").unwrap();
        assert_eq!(page.rows[0].files.len(), 2);
        assert_eq!(page.rows[0].files.iter().filter_map(|file| file.variant_id.as_deref()).collect::<BTreeSet<_>>(), BTreeSet::from(["800001", "800002"]));
        let known = "<a href='https://booth.pm/downloadables/700001?variation_id=800002'>Download</a>";
        let page = parse(&library(file_row("authored.zip", known, alternate(&[("700001", "800001")]))), "library", "/library?page=1").unwrap();
        assert_eq!(page.rows[0].files.len(), 1);
        assert_eq!(page.rows[0].files[0].variant_id.as_deref(), Some("800002"));
        let ambiguous_names = "<div class='legacy-list-item'><div class='min-w-0'><b>first.zip</b><b>second.zip</b></div>".to_owned() + primary + &alternate(&[("700001", "800001")]) + "</div>";
        let page = parse(&library(ambiguous_names), "library", "/library?page=1").unwrap();
        assert_eq!(page.rows[0].files[0].variant_id, None);
        for filename in [
            "<div class='min-w-0'><b>authored.zip</b></div>",
            "<div class='min-w-0 break-words whitespace-pre-line'><div class='text-14'>authored.zip</div></div>",
        ] {
            let siblings = format!("<div class='legacy-list-item'>{filename}{primary}</div><div class='legacy-list-item'>{}</div>", alternate(&[("700001", "800001")]));
            let page = parse(&library(siblings), "library", "/library?page=1").unwrap();
            assert_eq!(page.rows[0].files.len(), 1);
            assert_eq!(page.rows[0].files[0].variant_id, None);
        }
    }
    #[test]
    fn multitrack_formats_resolve_one_bound_file_per_track() {
        let formats = ["flac", "m4a", "mp3"];
        let menu = |test: &str, items: Vec<serde_json::Value>| {
            format!("<div data-test='{test}' data-dropdown-items='{}'></div>", serde_json::to_string(&items).unwrap())
        };
        let track = |file: &str, path: &str| {
            let download = menu("track-download-button", formats.iter().map(|kind| serde_json::json!(
                {"text":kind,"path":format!("{path}?type={kind}"),"data":{"filename":format!("track-{file}.{kind}")}})).collect());
            let apps = menu("other-downloads-button", formats.iter().map(|kind| serde_json::json!(
                {"text":"App","deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/{file}/deeplink?variation_id=800001&client=booth-library-manager&type={kind}")})).collect());
            download + &apps
        };
        let album = |zipped: &[&str]| {
            let zip = menu("zipped-download-button", formats.iter().map(|kind| serde_json::json!(
                {"text":kind,"basename":"authored-album","zippedFiles":zipped.iter().map(|file| serde_json::json!(
                    {"filename":format!("track-{file}.{kind}"),"prefetchUrl":format!("https://booth.pm/downloadables/{file}/archive-part?type={kind}"),"url":""})).collect::<Vec<_>>()})).collect());
            let apps = menu("other-multitrack-downloads-button", formats.iter().map(|kind| serde_json::json!(
                {"text":"App","deeplinkDownloadableUrl":format!("https://booth.pm/downloadables/700001/deeplink?variation_id=800001&client=booth-library-manager&type={kind}&download_scope=album")})).collect());
            zip + &apps
        };
        let pages = |paths: [&str; 2], zipped: &[&str]| {
            let (mut library, mut receipt) = (album(zipped), album(zipped));
            for (file, path) in ["700001", "700002"].into_iter().zip(paths) {
                let controls = track(file, path);
                library.push_str(&format!("<div class='mt-16'><div class='flex gap-4 items-center'><div class='break-all text-14 text-text-default'>Track title {file}</div></div><div class='flex'>{controls}</div></div>"));
                receipt.push_str(&format!("<div class='legacy-list-item'><div class='min-w-0'><b>Track title {file}</b></div>{controls}</div>"));
            }
            [
                parse(&format!("<main><h1>Library</h1><article data-test='library-item'><a href='https://invented.booth.pm/items/900001'>Authored album</a><div>{library}</div></article></main>"), "library", "/library?page=1").unwrap(),
                parse(&format!("<main><div class='sheet'><a href='https://invented.booth.pm/items/900001'>Authored album</a><div class='u-mb-300'>{receipt}</div></div></main>"), "purchase", "/orders/910001").unwrap(),
            ]
        };
        let tracks = ["https://booth.pm/downloadables/700001", "https://booth.pm/downloadables/700002"];
        for page in pages(tracks, &["700001", "700002"]) {
            assert_eq!(page.unresolved, 0);
            assert_eq!(page.rows[0].error_code, None);
            let files: Vec<_> = page.rows[0].files.iter().map(|file| (file.id.as_str(), file.variant_id.as_deref())).collect();
            assert_eq!(files, [("700001", Some("800001")), ("700002", Some("800001"))]);
            // Each format names its own file, so the track filename stays unknown.
            assert!(page.rows[0].files.iter().all(|file| file.name.is_none() && file.source_url.ends_with("?variation_id=800001")));
            assert!(!page.rows[0].manifest_complete);
        }
        let outside_zip = pages(tracks, &["700001", "700009"]);
        let unknown_format = pages([tracks[0], "https://example.invalid/downloadables/700002"], &["700001", "700002"]);
        for page in outside_zip.into_iter().chain(unknown_format) {
            assert_eq!(page.rows[0].error_code.as_deref(), Some("DIGITAL_CONTROLS_UNRESOLVED"));
        }
    }
    #[test]
    fn purchase_summary_date_enrichment_preserves_native_identity() {
        let before = TEMPLATE_HTML.replace("注文日時", "Unknown label");
        let undated = parse(&before, "purchase", "/orders/910001").unwrap();
        let dated = parse(TEMPLATE_HTML, "purchase", "/orders/910001").unwrap();
        assert_eq!(undated.rows[0].acquired_date_text, None);
        assert_eq!(
            dated.rows[0].acquired_date_text.as_deref(),
            Some("2026/10/10 09:30")
        );
        assert_eq!(
            undated.rows[0].source_record_id,
            dated.rows[0].source_record_id
        );
        assert_eq!(undated.rows[0].product_id, dated.rows[0].product_id);
        assert_eq!(undated.rows[0].order_id, dated.rows[0].order_id);
        assert_eq!(undated.rows[0].files[0].id, dated.rows[0].files[0].id);
        for changed in [
            TEMPLATE_HTML.replace("# 910001", "# 910002"),
            TEMPLATE_HTML.replace("2026/10/10 09:30", "Invalid private value"),
            TEMPLATE_HTML.replace("<p><strong>注文日時</strong></p>", "<p data-product-id='900002'><strong>注文日時</strong></p>"),
            TEMPLATE_HTML.replace("data-product-id='900001'", "data-product-id='900002'"),
            TEMPLATE_HTML.replace("</main>", "<div class='sheet'><div><span>注文番号</span><span>910001</span></div><div><span>注文日時</span><span>2026/10/11</span></div></div></main>"),
        ] {
            let row = parse(&changed, "purchase", "/orders/910001").unwrap().rows.remove(0);
            assert_eq!(row.acquired_date_text, None);
            assert_eq!(row.source_record_id, dated.rows[0].source_record_id);
        }
        // Explicit line identity and duplicate occurrence precedence are unchanged.
        let explicit = TEMPLATE_HTML.replace(
            "class='sheet' data-product-id='900001'",
            "class='sheet' data-product-id='900001' data-line-item-id='920001'",
        );
        let a = parse(
            &explicit.replace("注文日時", "Unknown label"),
            "purchase",
            "/orders/910001",
        )
        .unwrap();
        let b = parse(&explicit, "purchase", "/orders/910001").unwrap();
        assert_eq!(a.rows[0].source_record_id, b.rows[0].source_record_id);
        let card = explicit
            .split("<div class='sheet'><div><p><b>注文番号")
            .next()
            .unwrap()
            .strip_prefix("<main>")
            .unwrap();
        let duplicate = explicit.replace("<main>", &format!("<main>{card}"));
        let duplicate_undated = parse(
            &duplicate.replace("注文日時", "Unknown label"),
            "purchase",
            "/orders/910001",
        )
        .unwrap();
        let duplicate_dated = parse(&duplicate, "purchase", "/orders/910001").unwrap();
        assert_eq!(
            duplicate_undated.rows[0].source_record_id,
            duplicate_dated.rows[0].source_record_id
        );
        assert_eq!(
            duplicate_undated.rows[1].source_record_id,
            duplicate_dated.rows[1].source_record_id
        );
        assert!(duplicate_dated.rows[1]
            .source_record_id
            .ends_with(":occurrence:1"));
    }
    #[test]
    fn purchase_legacy_direct_date_retains_original_id_without_proving_acquisition() {
        let legacy = TEMPLATE_HTML.replace(
            "<div data-test='downloadable-list'>",
            "<time datetime='2026/10/10 09:30'></time><div data-test='downloadable-list'>",
        );
        let without_summary = legacy.replace("注文日時", "Unknown label");
        let a = parse(&without_summary, "purchase", "/orders/910001").unwrap();
        let b = parse(&legacy, "purchase", "/orders/910001").unwrap();
        // This is the previous parser's exact fallback preimage, independent of
        // the new summary date. It protects the existing genuinely dated key.
        let old_id = format!(
            "purchase:{}",
            safety::hash("910001:900001::2026/10/10 09:30:")
        );
        assert_eq!(a.rows[0].source_record_id, old_id);
        assert_eq!(b.rows[0].source_record_id, old_id);
        assert_eq!(a.rows[0].acquired_date_text, None);
        assert_eq!(
            b.rows[0].acquired_date_text.as_deref(),
            Some("2026/10/10 09:30")
        );
        let conflicting =
            legacy.replace("datetime='2026/10/10 09:30'", "datetime='2026/10/11 09:30'");
        let conflict = parse(&conflicting, "purchase", "/orders/910001").unwrap();
        assert_eq!(conflict.rows[0].acquired_date_text, None);
        assert_eq!(
            conflict.rows[0].source_record_id,
            format!(
                "purchase:{}",
                safety::hash("910001:900001::2026/10/11 09:30:")
            )
        );
        let undated = parse(TEMPLATE_HTML, "purchase", "/orders/910001").unwrap();
        assert_eq!(
            undated.rows[0].source_record_id,
            format!("purchase:{}", safety::hash("910001:900001:::"))
        );
        let invalid_manifest = legacy.replace("<b data-test='filename'>authored.zip</b>", "");
        let invalid = parse(&invalid_manifest, "purchase", "/orders/910001").unwrap();
        assert!(!invalid.rows[0].manifest_complete);
        assert_eq!(
            invalid.rows[0].acquired_date_text.as_deref(),
            Some("2026/10/10 09:30")
        );
        assert_eq!(invalid.rows[0].source_record_id, old_id);
        assert_eq!(invalid.rows[0].source_record_id, b.rows[0].source_record_id);
    }
    #[test]
    fn receipt_template_has_closed_shape_and_matches_summary_after_digital_card() {
        let proof = parse(TEMPLATE_HTML, "purchase", "/orders/910001")
            .unwrap()
            .rows;
        let value = serde_json::to_value(
            diagnose_receipt_template(TEMPLATE_HTML, &proof, "910001").unwrap(),
        )
        .unwrap();
        let keys = [
            "version",
            "digitalProofBound",
            "summaryCandidates",
            "summaryOrderNumberTextNodes",
            "summaryOrderedDateTextNodes",
            "summaryRoleCandidates",
            "summaryReceiptNumberMatches",
            "summaryUniqueDatePairs",
            "summarySlashDates",
            "summaryIsoDates",
            "summaryAmbiguousPairs",
            "jpyParents",
            "jpyParentDirectTextNodes",
            "jpyParentMultiplicationTokens",
            "jpyParentQuantityNodes",
            "jpyParentCompactSyntaxRows",
            "jpyParentSameProductLineRows",
            "priceScopeBoostTextNodes",
            "priceScopeExplicitPaidTextNodes",
            "priceScopeSubtotalTextNodes",
            "normalizedTitleLineWitnesses",
        ];
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 21);
        assert!(object.keys().all(|key| keys.contains(&key.as_str())));
        assert_eq!(
            object.values().filter(|value| value.is_boolean()).count(),
            1
        );
        assert!(object
            .values()
            .all(|value| value.is_number() || value.is_boolean()));
        assert_eq!(value["summaryReceiptNumberMatches"], 1);
        assert_eq!(value["summaryUniqueDatePairs"], 1);
        assert_eq!(value["summarySlashDates"], 1);
        assert_eq!(value["jpyParentCompactSyntaxRows"], 1);
        assert_eq!(value["jpyParentSameProductLineRows"], 1);
        assert!(value["normalizedTitleLineWitnesses"].as_u64().unwrap() > 0);
        assert_eq!(value["priceScopeBoostTextNodes"], 1);
        for private in [
            "PRIVATE", "900001", "910001", "700001", "800001", "2026/", "1,500", "https://",
            "Authored",
        ] {
            assert!(!value.to_string().contains(private));
        }
    }
    #[test]
    fn receipt_template_checks_paired_id_before_date_and_rejects_extra_money_tokens() {
        let proof = parse(TEMPLATE_HTML, "purchase", "/orders/910001")
            .unwrap()
            .rows;
        let wrong = TEMPLATE_HTML
            .replace("# 910001", "# 910002")
            .replace("2026/10/10 09:30", "PRIVATE_DATE_VALUE");
        let counts = diagnose_receipt_template(&wrong, &proof, "910001").unwrap();
        assert_eq!(counts.summary_receipt_number_matches, 0);
        assert_eq!(counts.summary_unique_date_pairs, 0);
        assert_eq!(counts.summary_slash_dates, 0);
        assert_eq!(counts.summary_ambiguous_pairs, 1);
        let invalid = TEMPLATE_HTML.replace(" ｘ ", " PRIVATE_TOKEN ｘ ");
        assert_eq!(
            diagnose_receipt_template(&invalid, &proof, "910001")
                .unwrap()
                .jpy_parent_compact_syntax_rows,
            0
        );
        let untrusted = TEMPLATE_HTML.replace(
            "href='https://invented.booth.pm/'",
            "href='https://untrusted.test/'",
        );
        assert_eq!(
            diagnose_receipt_template(&untrusted, &proof, "910001")
                .unwrap()
                .jpy_parent_same_product_line_rows,
            0
        );
        assert_eq!(
            diagnose_receipt_template(&TEMPLATE_HTML.replace("700001", "700002"), &proof, "910001")
                .err()
                .unwrap()
                .to_string(),
            "DIGITAL_PROOF_UNPROVEN"
        );
        let product_summary = TEMPLATE_HTML.replace(
            "<p><b>注文番号</b></p>",
            "<p data-product-id='900002'><b>注文番号</b></p>",
        );
        assert_eq!(
            diagnose_receipt_template(&product_summary, &proof, "910001")
                .unwrap()
                .summary_role_candidates,
            0
        );
        let malformed = TEMPLATE_HTML.replace("# 910001", "prefix910001");
        assert_eq!(
            diagnose_receipt_template(&malformed, &proof, "910001")
                .unwrap()
                .summary_receipt_number_matches,
            0
        );
    }
    const HTML: &str = r#"<main><h1>Library</h1><article data-test='library-item'><a href='https://invented.booth.pm/items/900001'>Invented avatar</a><div data-test='downloadable-list'><div><b data-test='filename'>source.psd</b><a data-test='downloadable' href='https://booth.pm/downloadables/700001?variation_id=800001'>Download</a><a href='https://booth.pm/downloadables/700001?variation_id=800001'>Again</a></div><div><b data-test='filename'>docs.zip</b><a href='https://booth.pm/downloadables/700002'>Download</a></div></div></article><nav class='pagination'><a aria-current='page' href='/library?page=1'>1</a></nav></main>"#;
    #[test]
    fn ordering_requires_provider_selected_control_or_free_history_witness() {
        assert_eq!(
            parse(HTML, "library", "/library?page=1").unwrap().ordering,
            Ordering::Unknown
        );
        let selected = "<select><option selected>入手が新しい順</option><option>ファイル更新が新しい順</option></select>";
        let h = HTML.replace("<main>", &format!("<main>{selected}"));
        assert_eq!(
            parse(&h, "library", "/library?page=1").unwrap().ordering,
            Ordering::AcquiredNewestFirst
        );
        assert_eq!(
            parse(
                &h.replace(
                    "<option selected>入手が新しい順</option><option>",
                    "<option>入手が新しい順</option><option selected>"
                ),
                "library",
                "/library?page=1"
            )
            .unwrap()
            .ordering,
            Ordering::Unknown
        );
        assert_eq!(
            parse(
                &h.replace(
                    "<main>",
                    "<main><input type='search' value='synthetic filter'>"
                ),
                "library",
                "/library?page=1"
            )
            .unwrap()
            .ordering,
            Ordering::Unknown
        );
        let free = HTML
            .replace("Library", "無料ダウンロード")
            .replace("/library?page=1", "/library/free_downloads?page=1");
        assert_eq!(
            parse(&free, "free", "/library/free_downloads?page=1")
                .unwrap()
                .ordering,
            Ordering::DownloadedNewestFirst
        );
        assert_eq!(
            parse(
                &free.replace("無料ダウンロード", "Free"),
                "free",
                "/library/free_downloads?page=1"
            )
            .unwrap()
            .ordering,
            Ordering::Unknown
        );
    }
    #[test]
    fn scoped_names_manifest_and_identity() {
        let p = parse(HTML, "library", "/library?page=1").unwrap();
        assert!(p.terminal);
        assert_eq!(p.rows[0].files.len(), 2);
        assert_eq!(p.rows[0].files[0].name.as_deref(), Some("source.psd"));
        assert!(p.rows[0].manifest_complete);
        assert_eq!(p.rows[0].files[0].revision, "unknown");
    }
    #[test]
    fn no_inferred_terminal_or_manifest() {
        let p = parse(
            &HTML
                .replace("data-test='downloadable-list'", "")
                .replace("aria-current='page'", ""),
            "library",
            "/library?page=1",
        )
        .unwrap();
        assert!(!p.terminal);
        assert!(!p.rows[0].manifest_complete);
    }
    #[test]
    fn ambiguous_names_are_unknown() {
        let p = parse(
            &HTML.replace(
                "<b data-test='filename'>source.psd</b>",
                "<b data-test='filename'>first.psd</b><b data-test='filename'>second.psd</b>",
            ),
            "library",
            "/library?page=1",
        )
        .unwrap();
        assert!(p.rows[0].files[0].name.is_none());
        assert!(!p.rows[0].manifest_complete);
    }
    #[test]
    fn orders_do_not_project_nondigital_content() {
        let h = "<main><div class='sheet'><p>PRIVATE ADDRESS MUST NOT PROJECT</p></div></main>";
        let p = parse(h, "purchase", "/orders/900001").unwrap();
        assert!(p.rows.is_empty());
        assert_eq!(p.unresolved, 0);
        assert!(!serde_json::to_string(&p).unwrap().contains("PRIVATE"));
    }
    #[test]
    fn source_navigation_cannot_finish_other_source() {
        let h = HTML.replace("/library?page=1", "/orders?page=1");
        assert!(!parse(&h, "library", "/library?page=1").unwrap().terminal);
    }
    #[test]
    fn wrapped_order_sheet_resolves_only_its_own_bounded_anchor() {
        let h="<main><h1>Orders</h1><a href='/orders/900001'><div class='sheet'><span>Independently authored order</span></div></a><nav class='pagination'><a aria-current='page' href='/orders'>1</a></nav></main>";
        let page = parse(h, "purchase", "/orders?page=1").unwrap();
        assert_eq!(page.details, vec!["/orders/900001"]);
        assert_eq!(page.unresolved, 0);
        assert!(page.terminal);
        let unrelated="<main><h1>Orders</h1><a href='/orders/900001'><div><div class='sheet'>One</div><div class='sheet'>Two</div></div></a></main>";
        let page = parse(unrelated, "purchase", "/orders?page=1").unwrap();
        assert!(page.details.is_empty());
        assert_eq!(page.unresolved, 2);
        let spoof = h.replace(
            "/orders/900001",
            "https://accounts.booth.pm.evil.test/orders/900001",
        );
        assert!(parse(&spoof, "purchase", "/orders?page=1")
            .unwrap()
            .details
            .is_empty());
    }
    #[test]
    fn canonical_page_one_pager_does_not_turn_navigation_or_spans_into_terminal() {
        let canonical = HTML.replace("href='/library?page=1'", "href='/library'");
        assert!(
            parse(&canonical, "library", "/library?page=1")
                .unwrap()
                .terminal
        );
        assert!(
            !parse(&canonical, "library", "/library?page=2")
                .unwrap()
                .terminal
        );
        let next = canonical.replace(
            "</nav>",
            "<a rel='next' href='/library?page=2'>Next</a></nav>",
        );
        let page = parse(&next, "library", "/library?page=1").unwrap();
        assert!(!page.terminal);
        assert_eq!(page.next, vec!["/library?page=2"]);
        let prev = HTML
            .replace("href='/library?page=1'", "href='/library?page=2'")
            .replace("</nav>", "<a rel='prev' href='/library'>Previous</a></nav>");
        let page = parse(&prev, "library", "/library?page=2").unwrap();
        assert!(page.terminal);
        assert!(page.next.is_empty());
        let navigation = canonical.replace("<nav class='pagination'>", "<nav>");
        assert!(
            !parse(&navigation, "library", "/library?page=1")
                .unwrap()
                .terminal
        );
        let span = canonical.replace(
            "<a aria-current='page' href='/library'>1</a>",
            "<span aria-current='page'>1</span><a rel='prev' href='/library'>Previous</a>",
        );
        assert!(!parse(&span, "library", "/library?page=1").unwrap().terminal);
    }
    #[test]
    fn signed_control_projects_only_stable_ids() {
        let h = HTML.replace(
            "?variation_id=800001",
            "?variation_id=800001&amp;signature=PRIVATE_SIGNING_VALUE",
        );
        let p = parse(&h, "library", "/library?page=1").unwrap();
        assert_eq!(p.rows[0].files[0].variant_id.as_deref(), Some("800001"));
        assert!(!serde_json::to_string(&p)
            .unwrap()
            .contains("PRIVATE_SIGNING"));
    }
    #[test]
    fn added_files_do_not_change_acquisition_identity() {
        let h = HTML.replace("700002", "700099");
        let a = parse(HTML, "library", "/library?page=1").unwrap();
        let b = parse(&h, "library", "/library?page=1").unwrap();
        assert_eq!(a.rows[0].source_record_id, b.rows[0].source_record_id);
        assert_ne!(a.fingerprint, b.fingerprint);
    }
    #[test]
    fn exact_withheld_marker_keeps_unknown_id_but_not_unsupported_identity() {
        let h="<h1>Library</h1><div class='bg-white p-16'><div>この商品は非公開です</div></div><nav class='pagination'><a aria-current='page' href='/library?page=1'>1</a></nav>";
        let p = parse(h, "library", "/library?page=1").unwrap();
        assert_eq!(p.unresolved, 0);
        assert!(p.rows[0].unavailable);
        assert!(p.rows[0].product_id.is_none());
        assert_eq!(
            p.rows[0].error_code.as_deref(),
            Some("BOOTH_WITHHELD_PRODUCT_ID")
        );
        let p = parse(
            &h.replace("<div>この商品", "<div data-item-id='900001'>この商品"),
            "library",
            "/library?page=1",
        )
        .unwrap();
        assert_eq!(p.unresolved, 1);
        assert_eq!(
            p.rows[0].error_code.as_deref(),
            Some("PRODUCT_ID_UNRESOLVED")
        );
    }
}
