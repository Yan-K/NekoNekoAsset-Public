//! Original, narrow free-item HTML adapter. Only observed free file controls grant files.
//! Provider behavior reference: official lilToon BOOTH item page, read 2026-10-09.
use super::{
    parser::{File, Ordering, Page},
    safety,
};
use anyhow::{bail, Result};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const NAMES: &str = ".text-14.text-text-default.font-normal.text-left.mb-8";
const CONTROLS: &str = "a[href*='/downloadables/'],[data-href*='/downloadables/']";
const PAUSED_CART: &str = "button.add-cart.disabled[type='button']";
const RESTOCK_NOTICE: &str = ".restock-request-links a.js-add-restock-request,.restock-request-links a.js-remove-restock-request";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Evidence {
    pub product_id: String,
    pub files: Vec<File>,
    pub manifest_complete: bool,
    pub unavailable: bool,
    pub error_code: Option<String>,
}
impl Evidence {
    pub fn unknown(id: &str, code: &str, unavailable: bool) -> Self {
        Self {
            product_id: id.into(),
            files: vec![],
            manifest_complete: false,
            unavailable,
            error_code: Some(code.into()),
        }
    }
    pub fn page(self) -> Result<Page> {
        let fingerprint = safety::hash(
            serde_json::to_vec(&self).map_err(|_| anyhow::anyhow!("FREE_ITEM_INVALID"))?,
        );
        Ok(Page {
            source: "free".into(),
            route: safety::free_item_route(&self.product_id)?,
            rows: vec![],
            next: vec![],
            details: vec![],
            terminal: false,
            unresolved: usize::from(!self.manifest_complete),
            fingerprint,
            is_detail: true,
            ordering: Ordering::Unknown,
            free_item: Some(self),
        })
    }
}
fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("fixed selector")
}
fn text(node: ElementRef<'_>) -> String {
    node.text().collect::<String>().trim().to_owned()
}
fn positive_row_price(row: ElementRef<'_>) -> bool {
    let prices: Vec<_> = row.select(&sel(".variation-price")).collect();
    prices.len() == 1 && {
        let price = text(prices[0]);
        (price.starts_with('¥') || price.ends_with('円'))
            && super::metadata::parse_jpy_price(&serde_json::json!(price))
                .is_some_and(|amount| amount > 0.0)
    }
}
fn non_free_role(row: ElementRef<'_>, id: &str) -> bool {
    row.select(&sel(
        "form button.add-cart[data-product-id],form button.add-gift[data-product-id]",
    ))
    .any(|button| button.value().attr("data-product-id") == Some(id))
        || (positive_row_price(row)
            && (row.select(&sel(PAUSED_CART)).next().is_some()
                || row.select(&sel(RESTOCK_NOTICE)).next().is_some()))
}
fn clean_name(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 4096 && !value.chars().any(char::is_control)
}
fn control(node: ElementRef<'_>) -> Option<(String, String, String)> {
    if node.value().name() != "a" || node.value().attr("data-href").is_some() {
        return None;
    }
    let raw = node.value().attr("href")?;
    if !raw.starts_with("https://booth.pm/downloadables/") {
        return None;
    }
    let (id, variant, canonical) = safety::resolver(raw)?;
    if canonical != raw {
        return None;
    }
    Some((id, variant?, canonical))
}
/// An alternate app menu for one bound file. Its optional `type` names that
/// file's own extension (`zip`, `vroid`, `vroidcustomitem`), never another file.
fn alternate(node: ElementRef<'_>, file: &str, variant: &str, name: &str) -> bool {
    if node.value().attr("data-test") != Some("other-downloads-button")
        || !node
            .value()
            .classes()
            .any(|c| c == "js-download-free-button")
    {
        return false;
    }
    let Some(items) = node
        .value()
        .attr("data-dropdown-items")
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    else {
        return false;
    };
    let Some(items) = items.as_array().filter(|a| !a.is_empty() && a.len() <= 10) else {
        return false;
    };
    items.iter().all(|item| {
        let Some(raw) = item["deeplinkDownloadableUrl"].as_str() else {
            return false;
        };
        let Ok(url) = url::Url::parse(raw) else {
            return false;
        };
        if !raw.starts_with("https://booth.pm/downloadables/")
            || raw.split('?').next()
                != Some(format!("https://booth.pm/downloadables/{file}/deeplink").as_str())
            || !safety::secure(&url)
            || url.host_str() != Some("booth.pm")
            || url.path() != format!("/downloadables/{file}/deeplink")
            || url.fragment().is_some()
        {
            return false;
        }
        let pairs: Vec<_> = url.query_pairs().collect();
        let mut keys = BTreeSet::new();
        pairs.iter().all(|(k, v)| {
            keys.insert(k.to_string())
                && match k.as_ref() {
                    "variation_id" => v.as_ref() == variant,
                    "client" => matches!(v.as_ref(), "booth-library-manager" | "vroid"),
                    "type" => name
                        .rsplit_once('.')
                        .is_some_and(|(_, ext)| !ext.is_empty() && ext.eq_ignore_ascii_case(v.as_ref())),
                    _ => false,
                }
        }) && keys.contains("variation_id")
            && keys.contains("client")
    })
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EntryDiagnostics {
    version: u32,
    identity_bound: bool,
    entry_still_unproven: bool,
    current_parser_complete: bool,
    variation_rows: usize,
    recognized_non_free_rows: usize,
    unrecognized_role_rows: usize,
    static_disabled_rows: usize,
    unavailable_marker_rows: usize,
    positive_price_rows: usize,
    bound_paid_variation_rows: usize,
    disabled_cart_rows: usize,
    restock_notice_rows: usize,
    raw_controls: usize,
    valid_controls: usize,
    free_controls: usize,
    controls_missing_cart_wrap: usize,
    wraps_with_multiple_valid_controls: usize,
    wraps_without_filename: usize,
    wraps_with_multiple_filenames: usize,
    title_agreements: usize,
    inner_single_control_single_filename_scopes_within4_ancestors: usize,
    inner_single_control_single_filename_scopes_within8_ancestors: usize,
    distinct_matched_filename_nodes: usize,
    orphan_controls: usize,
    orphan_names: usize,
    known_alternate_methods: usize,
    unknown_dropdowns: usize,
    expansion_hints: usize,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IdentityDiagnostics {
    version: u32,
    canonical_count: usize,
    og_count: usize,
    root_count: usize,
    list_count: usize,
    lists_within_matching_root: usize,
    root_id_match: bool,
    canonical_exact: bool,
    og_exact: bool,
    canonical_old_bare_same_id: bool,
    og_old_bare_same_id: bool,
    canonical_locale_ja_same_id: bool,
    og_locale_ja_same_id: bool,
    canonical_locale_en_same_id: bool,
    og_locale_en_same_id: bool,
    canonical_shop_host_same_id: bool,
    og_shop_host_same_id: bool,
    canonical_other_shape: bool,
    og_other_shape: bool,
}
fn shop_item_identity(raw: &str, id: &str) -> bool {
    if raw.contains('\\') || raw.chars().any(char::is_control) {
        return false;
    }
    let Some(rest) = raw.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains([':', '@']) {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if !safety::secure(&url)
        || !safety::booth_host(host)
        || matches!(
            host,
            "booth.pm" | "www.booth.pm" | "accounts.booth.pm" | "manage.booth.pm"
        )
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let path = rest.strip_prefix(authority).unwrap_or("");
    [
        format!("/items/{id}"),
        format!("/ja/items/{id}"),
        format!("/en/items/{id}"),
    ]
    .iter()
    .any(|expected| path == expected.as_str())
        && safety::item_id(raw).as_deref() == Some(id)
}
pub(super) fn diagnose_identity(html: &str, id: &str) -> Result<IdentityDiagnostics> {
    let expected = safety::free_item_route(id)?;
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
    // Attribute/count inspection only: no filename selectors, text(), or production file parsing.
    let canonical: Vec<_> = doc.select(&sel("link[rel='canonical']")).collect();
    let og: Vec<_> = doc.select(&sel("meta[property='og:url']")).collect();
    let roots: Vec<_> = doc
        .select(&sel("div#items.market[data-product-id]"))
        .collect();
    let root_match = roots.len() == 1 && roots[0].value().attr("data-product-id") == Some(id);
    let mut counts = IdentityDiagnostics {
        version: 1,
        canonical_count: canonical.len(),
        og_count: og.len(),
        root_count: roots.len(),
        list_count: doc.select(&sel("ul#variations")).count(),
        root_id_match: root_match,
        lists_within_matching_root: if root_match {
            roots[0].select(&sel("ul#variations")).count()
        } else {
            0
        },
        ..IdentityDiagnostics::default()
    };
    let classify = |raw: Option<&str>| {
        let Some(raw) = raw else {
            return (false, false, false, false, false, true);
        };
        let ja = raw == expected;
        let en = raw == format!("https://booth.pm/en/items/{id}");
        let bare = raw == format!("https://booth.pm/items/{id}")
            && safety::observed_product_url(raw).as_deref() == Some(expected.as_str());
        let shop = shop_item_identity(raw, id);
        (ja, bare, ja, en, shop, !(ja || bare || en || shop))
    };
    if canonical.len() == 1 {
        (
            counts.canonical_exact,
            counts.canonical_old_bare_same_id,
            counts.canonical_locale_ja_same_id,
            counts.canonical_locale_en_same_id,
            counts.canonical_shop_host_same_id,
            counts.canonical_other_shape,
        ) = classify(canonical[0].value().attr("href"));
    }
    if og.len() == 1 {
        (
            counts.og_exact,
            counts.og_old_bare_same_id,
            counts.og_locale_ja_same_id,
            counts.og_locale_en_same_id,
            counts.og_shop_host_same_id,
            counts.og_other_shape,
        ) = classify(og[0].value().attr("content"));
    }
    Ok(counts)
}
fn pair_name<'a>(scope: ElementRef<'a>, button: ElementRef<'a>) -> Option<ElementRef<'a>> {
    let raw: Vec<_> = scope.select(&sel(CONTROLS)).collect();
    let names: Vec<_> = scope.select(&sel(NAMES)).collect();
    if raw.len() != 1 || control(raw[0]).is_none() || names.len() != 1 {
        return None;
    }
    let name = text(names[0]);
    (clean_name(&name) && button.value().attr("title").map(str::trim) == Some(name.as_str()))
        .then(|| names[0])
}
fn cart_wrap<'a>(node: ElementRef<'a>, row: ElementRef<'a>) -> Option<ElementRef<'a>> {
    node.ancestors()
        .filter_map(ElementRef::wrap)
        .take_while(|n| n.id() != row.id())
        .find(|n| n.value().classes().any(|c| c == "cart-button-wrap"))
}
pub(super) fn diagnose_entry(html: &str, id: &str) -> Result<EntryDiagnostics> {
    // The production parser's unchanged identity/list gates run before any counts.
    let evidence = parse(html, id)?;
    if matches!(
        evidence.error_code.as_deref(),
        Some("FREE_ITEM_IDENTITY_UNPROVEN" | "FREE_ITEM_LIST_UNPROVEN")
    ) {
        bail!("FREE_ITEM_IDENTITY_UNPROVEN");
    }
    let doc = Html::parse_document(html);
    let root = doc
        .select(&sel("div#items.market[data-product-id]"))
        .next()
        .ok_or_else(|| anyhow::anyhow!("FREE_ITEM_IDENTITY_UNPROVEN"))?;
    let list = root
        .select(&sel("ul#variations"))
        .next()
        .ok_or_else(|| anyhow::anyhow!("FREE_ITEM_IDENTITY_UNPROVEN"))?;
    let root_controls: Vec<_> = root.select(&sel(CONTROLS)).collect();
    let list_controls: Vec<_> = list.select(&sel(CONTROLS)).collect();
    let mut counts = EntryDiagnostics {
        version: 1,
        identity_bound: true,
        entry_still_unproven: evidence.error_code.as_deref() == Some("FREE_ITEM_ENTRY_UNPROVEN"),
        current_parser_complete: evidence.manifest_complete,
        raw_controls: root_controls.len(),
        orphan_controls: root_controls.len().saturating_sub(list_controls.len()),
        ..EntryDiagnostics::default()
    };
    // Structural observations only. No marker or price proves owned-file
    // unavailability; the production role predicate also requires a bound list.
    let paid_role =
        sel("form button.add-cart[data-product-id],form button.add-gift[data-product-id]");
    let disabled = sel("button[disabled],input[disabled],button[aria-disabled='true'],input[aria-disabled='true'],a[aria-disabled='true']");
    let unavailable = sel("[data-test='item-unavailable'],.item-unavailable");
    let controls = sel(CONTROLS);
    let forms = sel("form");
    let variation_inputs = sel("input[name='cart_item[variation_id]']");
    let disabled_cart = sel(PAUSED_CART);
    let restock_notice = sel(RESTOCK_NOTICE);
    for row in list.children().filter_map(ElementRef::wrap) {
        counts.variation_rows += 1;
        let has_control = row.select(&controls).next().is_some();
        let has_paid_role = non_free_role(row, id);
        counts.recognized_non_free_rows += usize::from(!has_control && has_paid_role);
        counts.unrecognized_role_rows += usize::from(!has_control && !has_paid_role);
        counts.static_disabled_rows += usize::from(row.select(&disabled).next().is_some());
        counts.unavailable_marker_rows += usize::from(row.select(&unavailable).next().is_some());
        let positive_price = positive_row_price(row);
        counts.positive_price_rows += usize::from(positive_price);
        counts.bound_paid_variation_rows += usize::from(
            positive_price
                && row.select(&forms).any(|form| {
                    let inputs: Vec<_> = form.select(&variation_inputs).collect();
                    inputs.len() == 1
                        && inputs[0].value().attr("value").is_some_and(safety::numeric)
                        && form
                            .select(&paid_role)
                            .any(|button| button.value().attr("data-product-id") == Some(id))
                }),
        );
        counts.disabled_cart_rows += usize::from(row.select(&disabled_cart).next().is_some());
        counts.restock_notice_rows += usize::from(row.select(&restock_notice).next().is_some());
    }
    let mut seen_wraps = vec![];
    let mut matched_names = vec![];
    for button in list_controls {
        if control(button).is_none() {
            continue;
        }
        counts.valid_controls += 1;
        let labels: Vec<_> = button.select(&sel(".cmd-label")).collect();
        if labels.len() != 1 || text(labels[0]) != "無料ダウンロード" {
            continue;
        }
        counts.free_controls += 1;
        let row = button
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take_while(|n| n.id() != list.id())
            .find(|n| {
                n.value().name() == "li" && n.value().classes().any(|c| c == "variation-item")
            });
        let Some(row) = row else {
            counts.orphan_controls += 1;
            continue;
        };
        let scope = button
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take_while(|n| n.id() != row.id())
            .find(|n| n.value().classes().any(|c| c == "cart-button-wrap"));
        let Some(scope) = scope else {
            counts.controls_missing_cart_wrap += 1;
            continue;
        };
        if !seen_wraps.contains(&scope.id()) {
            seen_wraps.push(scope.id());
            counts.wraps_with_multiple_valid_controls += usize::from(
                scope
                    .select(&sel(CONTROLS))
                    .filter(|n| control(*n).is_some())
                    .count()
                    > 1,
            );
            let names = scope.select(&sel(NAMES)).count();
            counts.wraps_without_filename += usize::from(names == 0);
            counts.wraps_with_multiple_filenames += usize::from(names > 1);
        }
        let title = button.value().attr("title").map(str::trim);
        let agreeing: Vec<_> = scope
            .select(&sel(NAMES))
            .filter(|n| {
                let name = text(*n);
                clean_name(&name) && title == Some(name.as_str())
            })
            .collect();
        counts.title_agreements += usize::from(agreeing.len() == 1);
        if let Some(name) = pair_name(scope, button) {
            if !matched_names.contains(&name.id()) {
                matched_names.push(name.id());
            }
        }
        let mut first_depth = None;
        // ego-tree ancestors() starts at the parent, so keep the first inner scope.
        for (index, inner) in button
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take_while(|n| n.id() != scope.id())
            .take(8)
            .enumerate()
        {
            if let Some(name) = pair_name(inner, button) {
                first_depth = Some(index + 1);
                if !matched_names.contains(&name.id()) {
                    matched_names.push(name.id());
                }
                break;
            }
        }
        counts.inner_single_control_single_filename_scopes_within4_ancestors +=
            usize::from(first_depth.is_some_and(|d| d <= 4));
        counts.inner_single_control_single_filename_scopes_within8_ancestors +=
            usize::from(first_depth.is_some());
    }
    counts.distinct_matched_filename_nodes = matched_names.len();
    counts.orphan_names = root
        .select(&sel(NAMES))
        .filter(|n| !matched_names.contains(&n.id()))
        .count();
    for node in root.select(&sel(
        "[data-dropdown-items],[data-test='other-downloads-button']",
    )) {
        let mut known = false;
        for scope in node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take_while(|n| n.id() != root.id())
            .take(8)
        {
            let controls: Vec<_> = scope.select(&sel(CONTROLS)).collect();
            if controls.len() == 1 {
                if let Some((file, variant, _)) = control(controls[0]) {
                    if pair_name(scope, controls[0])
                        .is_some_and(|name| alternate(node, &file, &variant, &text(name)))
                    {
                        known = true;
                        break;
                    }
                }
            }
        }
        counts.known_alternate_methods += usize::from(known);
        counts.unknown_dropdowns += usize::from(!known);
    }
    counts.expansion_hints=root.select(&sel("[data-test='load-more'],[data-test='show-more'],[data-test='more-downloadables'],[aria-expanded='false']:not([data-test='other-downloads-button']),select")).count();
    Ok(counts)
}
pub(super) fn parse(html: &str, id: &str) -> Result<Evidence> {
    let expected = safety::free_item_route(id)?;
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
    let unknown = |code| Evidence::unknown(id, code, false);
    let canonical: Vec<_> = doc.select(&sel("link[rel='canonical']")).collect();
    let og: Vec<_> = doc.select(&sel("meta[property='og:url']")).collect();
    let roots: Vec<_> = doc
        .select(&sel("div#items.market[data-product-id]"))
        .collect();
    if canonical.len() != 1
        || !canonical[0]
            .value()
            .attr("href")
            .is_some_and(|raw| raw == expected || shop_item_identity(raw, id))
        || og.len() != 1
        || og[0].value().attr("content") != Some(expected.as_str())
        || roots.len() != 1
        || roots[0].value().attr("data-product-id") != Some(id)
    {
        return Ok(unknown("FREE_ITEM_IDENTITY_UNPROVEN"));
    }
    let root = roots[0];
    let lists: Vec<_> = doc.select(&sel("ul#variations")).collect();
    if lists.len() != 1 || !root.descendants().any(|n| n.id() == lists[0].id()) {
        return Ok(unknown("FREE_ITEM_LIST_UNPROVEN"));
    }
    let list = lists[0];
    if std::iter::once(list)
        .chain(list.select(&sel("[data-product-id],[data-item-id]")))
        .any(|n| {
            ["data-product-id", "data-item-id"]
                .iter()
                .any(|attr| n.value().attr(attr).is_some_and(|v| v != id))
        })
    {
        return Ok(unknown("FREE_ITEM_IDENTITY_UNPROVEN"));
    }
    let rows: Vec<_> = list.children().filter_map(ElementRef::wrap).collect();
    if rows.is_empty()
        || rows.iter().any(|n| {
            n.value().name() != "li" || !n.value().classes().any(|c| c == "variation-item")
        })
        || list.select(&sel("li.variation-item")).count() != rows.len()
    {
        return Ok(unknown("FREE_ITEM_LIST_UNPROVEN"));
    }
    if root.select(&sel("[data-test='load-more'],[data-test='show-more'],[data-test='more-downloadables'],button[aria-expanded='false']:not([data-test='other-downloads-button']),[data-test='variation-select'],[data-test='variation-selector'],select[name='variation_id'],select[name='variation']")).next().is_some() { return Ok(unknown("FREE_ITEM_DYNAMIC_LIST")); }
    if list
        .select(&sel(
            "select,[aria-expanded='false']:not([data-test='other-downloads-button'])",
        ))
        .next()
        .is_some()
    {
        return Ok(unknown("FREE_ITEM_DYNAMIC_LIST"));
    }
    let root_controls: Vec<_> = root.select(&sel(CONTROLS)).collect();
    let list_controls: Vec<_> = list.select(&sel(CONTROLS)).collect();
    if root_controls.len() != list_controls.len() {
        return Ok(unknown("FREE_ITEM_ORPHAN_CONTROL"));
    }
    let mut files = BTreeMap::new();
    let mut accepted_names = vec![];
    let mut accepted_labels = vec![];
    let mut accepted_alternates = vec![];
    for row in &rows {
        if row.select(&sel(CONTROLS)).next().is_none() && !non_free_role(*row, id) {
            return Ok(unknown("FREE_ITEM_ROLE_UNPROVEN"));
        }
        let mut processed_wraps = vec![];
        for outer_button in row.select(&sel(CONTROLS)) {
            let Some(scope) = cart_wrap(outer_button, *row) else {
                return Ok(unknown("FREE_ITEM_ENTRY_UNPROVEN"));
            };
            if processed_wraps.contains(&scope.id()) {
                continue;
            }
            processed_wraps.push(scope.id());
            let controls: Vec<_> = scope.select(&sel(CONTROLS)).collect();
            let names: Vec<_> = scope.select(&sel(NAMES)).collect();
            if controls.is_empty()
                || controls.len() != names.len()
                || controls
                    .iter()
                    .chain(names.iter())
                    .any(|node| cart_wrap(*node, *row).map(|wrap| wrap.id()) != Some(scope.id()))
                || scope
                    .select(&sel(".cart-button-wrap,button.add-cart,button.add-gift"))
                    .next()
                    .is_some()
            {
                return Ok(unknown("FREE_ITEM_ENTRY_UNPROVEN"));
            }
            let mut names_by_text = BTreeMap::new();
            for node in names {
                let name = text(node);
                if !clean_name(&name) || names_by_text.insert(name, node).is_some() {
                    return Ok(unknown("FREE_ITEM_FILENAME_UNPROVEN"));
                }
            }
            let mut consumed_names = vec![];
            let mut wrap_keys = vec![];
            for button in controls {
                let Some((file_id, variant_id, source_url)) = control(button) else {
                    return Ok(unknown("FREE_ITEM_INVALID_CONTROL"));
                };
                if row
                    .value()
                    .attr("data-variation-id")
                    .is_some_and(|v| v != variant_id)
                {
                    return Ok(unknown("FREE_ITEM_IDENTITY_UNPROVEN"));
                }
                let Some(name) = button.value().attr("title").map(str::trim) else {
                    return Ok(unknown("FREE_ITEM_FILENAME_UNPROVEN"));
                };
                let Some(node) = names_by_text.get(name) else {
                    return Ok(unknown("FREE_ITEM_FILENAME_UNPROVEN"));
                };
                if consumed_names.contains(&node.id()) {
                    return Ok(unknown("FREE_ITEM_FILENAME_UNPROVEN"));
                }
                consumed_names.push(node.id());
                let key = (file_id.clone(), variant_id.clone());
                if files.contains_key(&key) {
                    return Ok(unknown("FREE_ITEM_CONFLICTING_ENTRIES"));
                }
                accepted_names.push(node.id());
                // Exact product/variant/resolver and filename binding prove this
                // offered file. Button wording and optional labels are display UI.
                accepted_labels.extend(button.select(&sel(".cmd-label")).map(|label| label.id()));
                wrap_keys.push((key.clone(), name));
                files.insert(
                    key,
                    File {
                        id: file_id,
                        variant_id: Some(variant_id),
                        name: Some(name.to_owned()),
                        size_bytes: None,
                        source_url,
                        revision: "unknown".into(),
                        format: None,
                    },
                );
            }
            if consumed_names.len() != names_by_text.len() {
                return Ok(unknown("FREE_ITEM_FILENAME_UNPROVEN"));
            }
            for other in scope.select(&sel(
                "[data-dropdown-items],[data-test='other-downloads-button']",
            )) {
                if cart_wrap(other, *row).map(|wrap| wrap.id()) != Some(scope.id())
                    || wrap_keys
                        .iter()
                        .filter(|((file, variant), name)| alternate(other, file, variant, name))
                        .count()
                        != 1
                {
                    return Ok(unknown("FREE_ITEM_UNKNOWN_DROPDOWN"));
                }
                accepted_alternates.push(other.id());
            }
        }
    }
    if root
        .select(&sel(NAMES))
        .any(|n| !accepted_names.contains(&n.id()))
        || root
            .select(&sel(".cmd-label"))
            .any(|n| text(n) == "無料ダウンロード" && !accepted_labels.contains(&n.id()))
    {
        return Ok(unknown("FREE_ITEM_ORPHAN_ENTRY"));
    }
    if root
        .select(&sel(
            "[data-dropdown-items],[data-test='other-downloads-button']",
        ))
        .any(|n| !accepted_alternates.contains(&n.id()))
    {
        return Ok(unknown("FREE_ITEM_UNKNOWN_DROPDOWN"));
    }
    for node in std::iter::once(list).chain(list.select(&sel(
        "[data-file-count],[data-downloadable-count],[data-total-count]",
    ))) {
        let mut keys = BTreeSet::new();
        for button in node.select(&sel(CONTROLS)) {
            let Some((file, variant, _)) = control(button) else {
                return Ok(unknown("FREE_ITEM_INVALID_CONTROL"));
            };
            keys.insert((file, variant));
        }
        for attr in [
            "data-file-count",
            "data-downloadable-count",
            "data-total-count",
        ] {
            if node
                .value()
                .attr(attr)
                .is_some_and(|v| v.parse::<usize>().ok() != Some(keys.len()))
            {
                return Ok(unknown("FREE_ITEM_COUNT_UNPROVEN"));
            }
        }
    }
    if files.is_empty() {
        // This settles only the current public free offer. It never certifies
        // owned-file membership or revokes a previously observed entitlement.
        if root_controls.is_empty()
            && rows
                .iter()
                .all(|row| positive_row_price(*row) && non_free_role(*row, id))
        {
            return Ok(Evidence::unknown(id, "FREE_OFFER_UNAVAILABLE", true));
        }
        return Ok(unknown("FREE_FILE_CONTROLS_UNAVAILABLE"));
    }
    Ok(Evidence {
        product_id: id.into(),
        files: files.into_values().collect(),
        manifest_complete: true,
        unavailable: false,
        error_code: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Independently authored structural fixture, no provider page or script copied.
    const HTML: &str = r#"<html><head><link rel='canonical' href='https://booth.pm/ja/items/900001'><meta property='og:url' content='https://booth.pm/ja/items/900001'></head><body><div id='items' class='market' data-product-id='900001'><ul id='variations'><li class='variation-item'><div class='cart-button-wrap'><a title='authored.zip' href='https://booth.pm/downloadables/700001?variation_id=800001'><span class='cmd-label'>無料ダウンロード</span></a><div class='text-14 text-text-default font-normal text-left mb-8'>authored.zip</div></div></li><li class='variation-item'><form><button class='add-cart' data-product-id='900001'>Authored donation</button></form></li></ul></div></body></html>"#;
    #[test]
    fn entry_diagnostics_count_bound_roles_and_static_markers_without_certifying_unavailability() {
        let gift = "<li class='variation-item'><form><button class='add-gift' data-product-id='900001' disabled>PRIVATE_ROLE_SENTINEL</button></form></li>";
        let unknown = "<li class='variation-item'><form><button class='authored-unknown' aria-disabled='true'>PRIVATE_ROLE_SENTINEL</button></form><span class='item-unavailable'>PRIVATE_ROLE_SENTINEL</span></li>";
        let html = HTML.replace("</ul>", &format!("{gift}{unknown}</ul>"));
        let evidence = parse(&html, "900001").unwrap();
        assert_eq!(
            evidence.error_code.as_deref(),
            Some("FREE_ITEM_ROLE_UNPROVEN")
        );
        assert!(!evidence.manifest_complete);
        assert!(!evidence.unavailable);
        let value = serde_json::to_value(diagnose_entry(&html, "900001").unwrap()).unwrap();
        assert_eq!(value["variationRows"], 4);
        assert_eq!(value["recognizedNonFreeRows"], 2);
        assert_eq!(value["unrecognizedRoleRows"], 1);
        assert_eq!(value["staticDisabledRows"], 2);
        assert_eq!(value["unavailableMarkerRows"], 1);
        assert_eq!(value["rawControls"], 1);
        assert_eq!(value["validControls"], 1);
        assert_eq!(value["currentParserComplete"], false);
        assert!(value
            .as_object()
            .unwrap()
            .values()
            .all(|v| v.is_number() || v.is_boolean()));
        let encoded = value.to_string();
        for private in [
            "PRIVATE_ROLE_SENTINEL",
            "900001",
            "900002",
            "700001",
            "800001",
            "https://",
        ] {
            assert!(!encoded.contains(private));
        }

        let start = HTML.find("<li class='variation-item'>").unwrap();
        let end = start + HTML[start..].find("</li>").unwrap() + "</li>".len();
        let paid_only = format!("{}{}", &HTML[..start], &HTML[end..]);
        let evidence = parse(&paid_only, "900001").unwrap();
        assert_eq!(
            evidence.error_code.as_deref(),
            Some("FREE_FILE_CONTROLS_UNAVAILABLE")
        );
        assert!(!evidence.manifest_complete);
        assert!(!evidence.unavailable);
        let counts = serde_json::to_value(diagnose_entry(&paid_only, "900001").unwrap()).unwrap();
        assert_eq!(counts["variationRows"], 1);
        assert_eq!(counts["recognizedNonFreeRows"], 1);
        assert_eq!(counts["unrecognizedRoleRows"], 0);
        assert_eq!(counts["rawControls"], 0);
        assert_eq!(counts["staticDisabledRows"], 0);
        assert_eq!(counts["unavailableMarkerRows"], 0);
        assert_eq!(counts["currentParserComplete"], false);

        let paid = paid_only.replace(
            "<form><button",
            "<div class='variation-price'>¥ 1,200</div><form><input name='cart_item[variation_id]' value='800003'><button",
        );
        let counts = serde_json::to_value(diagnose_entry(&paid, "900001").unwrap()).unwrap();
        assert_eq!(counts["positivePriceRows"], 1);
        assert_eq!(counts["boundPaidVariationRows"], 1);
        for amount in ["¥ 0", "¥ 1,200–2,000", "$ 1,200", "1200", "¥ 12,00"] {
            let invalid = paid.replace("¥ 1,200", amount);
            let counts = serde_json::to_value(diagnose_entry(&invalid, "900001").unwrap()).unwrap();
            assert_eq!(counts["positivePriceRows"], 0, "{amount}");
            assert_eq!(counts["boundPaidVariationRows"], 0, "{amount}");
        }
        for input in [
            "",
            "<input name='cart_item[variation_id]' value='not-numeric'>",
            "<input name='cart_item[variation_id]' value='800003'><input name='cart_item[variation_id]' value='800004'>",
        ] {
            let invalid = paid.replace(
                "<input name='cart_item[variation_id]' value='800003'>",
                input,
            );
            let counts =
                serde_json::to_value(diagnose_entry(&invalid, "900001").unwrap()).unwrap();
            assert_eq!(counts["positivePriceRows"], 1);
            assert_eq!(counts["boundPaidVariationRows"], 0);
        }
        let closed_rows = "<li class='variation-item'><div class='variation-price'>¥ 500</div><button class='add-cart disabled' type='button'>PRIVATE_ROLE_SENTINEL</button></li><li class='variation-item'><div class='variation-price'>¥ 700</div><div class='restock-request-links'><a class='js-add-restock-request'>PRIVATE_ROLE_SENTINEL</a><a class='js-remove-restock-request'>PRIVATE_ROLE_SENTINEL</a></div></li>";
        let closed = HTML.replace("</ul>", &format!("{closed_rows}</ul>"));
        let evidence = parse(&closed, "900001").unwrap();
        assert!(evidence.error_code.is_none());
        assert!(evidence.manifest_complete);
        assert_eq!(evidence.files.len(), 1);
        assert!(!evidence.unavailable);
        let counts = serde_json::to_value(diagnose_entry(&closed, "900001").unwrap()).unwrap();
        assert_eq!(counts["positivePriceRows"], 2);
        assert_eq!(counts["boundPaidVariationRows"], 0);
        assert_eq!(counts["disabledCartRows"], 1);
        assert_eq!(counts["restockNoticeRows"], 1);
        assert_eq!(counts["staticDisabledRows"], 0);
        assert_eq!(counts["recognizedNonFreeRows"], 3);
        assert_eq!(counts["unrecognizedRoleRows"], 0);
        assert!(!counts.to_string().contains("PRIVATE_ROLE_SENTINEL"));
    }
    #[test]
    fn identity_diagnostics_have_a_closed_shape_even_without_matching_root() {
        let html = HTML
            .replace("authored.zip", "PRIVATE_SOURCE_SENTINEL.zip")
            .replace("data-product-id='900001'", "data-product-id='900002'");
        let value = serde_json::to_value(diagnose_identity(&html, "900001").unwrap()).unwrap();
        let keys = [
            "version",
            "canonicalCount",
            "ogCount",
            "rootCount",
            "listCount",
            "listsWithinMatchingRoot",
            "rootIdMatch",
            "canonicalExact",
            "ogExact",
            "canonicalOldBareSameId",
            "ogOldBareSameId",
            "canonicalLocaleJaSameId",
            "ogLocaleJaSameId",
            "canonicalLocaleEnSameId",
            "ogLocaleEnSameId",
            "canonicalShopHostSameId",
            "ogShopHostSameId",
            "canonicalOtherShape",
            "ogOtherShape",
        ];
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), keys.len());
        assert!(object.keys().all(|key| keys.contains(&key.as_str())));
        assert!(object.values().all(|v| v.is_number() || v.is_boolean()));
        let encoded = value.to_string();
        for private in [
            "PRIVATE_SOURCE_SENTINEL",
            "900001",
            "900002",
            "700001",
            "800001",
            "https://",
        ] {
            assert!(!encoded.contains(private));
        }
        assert_eq!(value["rootIdMatch"], false);
        assert_eq!(value["listsWithinMatchingRoot"], 0);
        assert_eq!(value["listCount"], 1);
        assert_eq!(value["canonicalExact"], true);
    }
    #[test]
    fn identity_url_shapes_are_same_id_only_and_do_not_relax_production() {
        let original = "https://booth.pm/ja/items/900001";
        for (url, shape) in [
            (original, "LocaleJaSameId"),
            ("https://booth.pm/items/900001", "OldBareSameId"),
            ("https://booth.pm/en/items/900001", "LocaleEnSameId"),
            (
                "https://synthetic-shop.booth.pm/items/900001",
                "ShopHostSameId",
            ),
            (
                "https://synthetic-shop.booth.pm/ja/items/900001",
                "ShopHostSameId",
            ),
            (
                "https://synthetic-shop.booth.pm/en/items/900001",
                "ShopHostSameId",
            ),
            ("https://booth.pm/ja/items/900002", "OtherShape"),
            ("https://www.booth.pm/items/900001", "OtherShape"),
            ("https://accounts.booth.pm/items/900001", "OtherShape"),
            (
                "https://synthetic-shop.booth.pm/items/900001?x=1",
                "OtherShape",
            ),
            (
                "https://synthetic-shop.booth.pm:443/items/900001",
                "OtherShape",
            ),
            (
                "https://synthetic-shop.booth.pm/ja/../items/900001",
                "OtherShape",
            ),
            (
                "https://synthetic-shop.booth.pm/items/900001/",
                "OtherShape",
            ),
            ("https://synthetic-shop.booth.pm/items/900002", "OtherShape"),
            (
                "https://synthetic-shop.booth.pm.evil.test/items/900001",
                "OtherShape",
            ),
            ("http://synthetic-shop.booth.pm/items/900001", "OtherShape"),
        ] {
            let html = HTML.replace(original, url);
            let value = serde_json::to_value(diagnose_identity(&html, "900001").unwrap()).unwrap();
            assert_eq!(value[format!("canonical{shape}")], true, "{url}");
            assert_eq!(value[format!("og{shape}")], true, "{url}");
            assert_eq!(value["canonicalExact"], url == original);
            assert_eq!(
                parse(&html, "900001").unwrap().manifest_complete,
                url == original
            );
        }
        let duplicate = HTML.replace(
            "</head>",
            "<link rel='canonical' href='https://booth.pm/ja/items/900001'></head>",
        );
        let value = serde_json::to_value(diagnose_identity(&duplicate, "900001").unwrap()).unwrap();
        assert_eq!(value["canonicalCount"], 2);
        assert_eq!(value["canonicalExact"], false);
        let absent = HTML.replace("<ul id='variations'>", "<ul id='other'>");
        assert_eq!(diagnose_identity(&absent, "900001").unwrap().list_count, 0);
        for (markup, error) in [
            ("<input type='password'>", "AUTH_REQUIRED"),
            ("<div id='challenge-form'></div>", "AUTH_CHALLENGE"),
        ] {
            assert_eq!(
                diagnose_identity(markup, "900001")
                    .err()
                    .unwrap()
                    .to_string(),
                error
            );
        }
    }
    #[test]
    fn entry_diagnostics_are_closed_counts_and_never_serialize_source_values() {
        let html = HTML.replace("authored.zip", "PRIVATE_SOURCE_SENTINEL.zip");
        let value = serde_json::to_value(diagnose_entry(&html, "900001").unwrap()).unwrap();
        let keys = [
            "version",
            "identityBound",
            "entryStillUnproven",
            "currentParserComplete",
            "variationRows",
            "recognizedNonFreeRows",
            "unrecognizedRoleRows",
            "staticDisabledRows",
            "unavailableMarkerRows",
            "positivePriceRows",
            "boundPaidVariationRows",
            "disabledCartRows",
            "restockNoticeRows",
            "rawControls",
            "validControls",
            "freeControls",
            "controlsMissingCartWrap",
            "wrapsWithMultipleValidControls",
            "wrapsWithoutFilename",
            "wrapsWithMultipleFilenames",
            "titleAgreements",
            "innerSingleControlSingleFilenameScopesWithin4Ancestors",
            "innerSingleControlSingleFilenameScopesWithin8Ancestors",
            "distinctMatchedFilenameNodes",
            "orphanControls",
            "orphanNames",
            "knownAlternateMethods",
            "unknownDropdowns",
            "expansionHints",
        ];
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), keys.len());
        assert!(object.keys().all(|key| keys.contains(&key.as_str())));
        assert!(object.values().all(|v| v.is_number() || v.is_boolean()));
        let encoded = value.to_string();
        for private in [
            "PRIVATE_SOURCE_SENTINEL",
            "900001",
            "700001",
            "800001",
            "https://",
        ] {
            assert!(!encoded.contains(private));
        }
        assert_eq!(value["rawControls"], 1);
        assert_eq!(value["titleAgreements"], 1);
        assert_eq!(value["currentParserComplete"], true);
        assert_eq!(
            value["innerSingleControlSingleFilenameScopesWithin4Ancestors"],
            0
        );
        assert_eq!(
            value["innerSingleControlSingleFilenameScopesWithin8Ancestors"],
            0
        );
        assert_eq!(
            diagnose_entry(&html, "900002").err().unwrap().to_string(),
            "FREE_ITEM_IDENTITY_UNPROVEN"
        );
    }
    #[test]
    fn entry_diagnostic_keeps_inner_scope_counts_for_proven_shared_manifest() {
        let original = HTML
            .split_once("<li class='variation-item'>")
            .unwrap()
            .1
            .split_once("</li>")
            .unwrap()
            .0;
        let first = original
            .strip_prefix("<div class='cart-button-wrap'>")
            .unwrap()
            .strip_suffix("</div>")
            .unwrap();
        let second = first
            .replace("700001", "700002")
            .replace("800001", "800002")
            .replace("authored.zip", "second.psd");
        let multi =
            format!("<div class='cart-button-wrap'><div>{first}</div><div>{second}</div></div>");
        let html = HTML.replacen(original, &multi, 1);
        assert!(parse(&html, "900001").unwrap().manifest_complete);
        let value = serde_json::to_value(diagnose_entry(&html, "900001").unwrap()).unwrap();
        assert_eq!(value["entryStillUnproven"], false);
        assert_eq!(value["currentParserComplete"], true);
        assert_eq!(value["wrapsWithMultipleValidControls"], 1);
        assert_eq!(value["wrapsWithMultipleFilenames"], 1);
        assert_eq!(value["titleAgreements"], 2);
        assert_eq!(
            value["innerSingleControlSingleFilenameScopesWithin4Ancestors"],
            2
        );
        assert_eq!(
            value["innerSingleControlSingleFilenameScopesWithin8Ancestors"],
            2
        );
        assert_eq!(value["distinctMatchedFilenameNodes"], 2);
        assert_eq!(value["orphanNames"], 0);
    }
    fn shared_wrap_html() -> String {
        shared_wrap_html_with_files(4)
    }
    fn shared_wrap_html_with_files(count: usize) -> String {
        // Reordered independent filename nodes and same-file menus in one wrap.
        let controls = (1..=count).map(|i| format!("<a title='authored-{i}.zip' href='https://booth.pm/downloadables/70000{i}?variation_id=80000{i}'><span class='cmd-label'>無料ダウンロード</span></a>")).collect::<String>();
        let names = (1..=count).rev().map(|i| format!("<div class='text-14 text-text-default font-normal text-left mb-8'>authored-{i}.zip</div>")).collect::<String>();
        let menus = (1..=count).map(|i| format!(r#"<div class='js-download-free-button' data-test='other-downloads-button' data-dropdown-items='[{{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/70000{i}/deeplink?client=booth-library-manager&variation_id=80000{i}"}}]'></div>"#)).collect::<String>();
        let original = HTML
            .split_once("<li class='variation-item'>")
            .unwrap()
            .1
            .split_once("</li>")
            .unwrap()
            .0;
        HTML.replacen(
            original,
            &format!("<div class='cart-button-wrap'>{controls}{names}{menus}</div>"),
            1,
        )
    }
    #[test]
    fn settled_free_offer_absence_requires_positive_paid_rows_and_all_guards() {
        let start = HTML.find("<li class='variation-item'>").unwrap();
        let end = start + HTML[start..].find("</li>").unwrap() + "</li>".len();
        let paid = format!("{}{}", &HTML[..start], &HTML[end..]).replace(
            "<form><button",
            "<div class='variation-price'>¥ 1,200</div><form><button",
        );
        for html in [
            paid.clone(),
            paid.replace("class='add-cart'", "class='add-gift'"),
            paid.replace("<form><button class='add-cart' data-product-id='900001'>Authored donation</button></form>", "<button class='add-cart disabled' type='button'>Authored paused</button>"),
            paid.replace("<form><button class='add-cart' data-product-id='900001'>Authored donation</button></form>", "<div class='restock-request-links'><a class='js-add-restock-request'>Authored notification</a></div>"),
        ] {
            let evidence = parse(&html, "900001").unwrap();
            assert_eq!(evidence.error_code.as_deref(), Some("FREE_OFFER_UNAVAILABLE"));
            assert!(evidence.unavailable);
            assert!(!evidence.manifest_complete);
            assert!(evidence.files.is_empty());
            assert_eq!(evidence.page().unwrap().unresolved, 1);
        }
        for price in ["¥ 0", "¥ 1,200–2,000", "$ 1,200", "1200", "¥ 12,00"] {
            let evidence = parse(&paid.replace("¥ 1,200", price), "900001").unwrap();
            assert_eq!(
                evidence.error_code.as_deref(),
                Some("FREE_FILE_CONTROLS_UNAVAILABLE")
            );
            assert!(!evidence.unavailable);
            assert!(!evidence.manifest_complete);
        }
        for (html, code) in [
            (paid.replace("<div class='variation-price'>¥ 1,200</div>", ""), "FREE_FILE_CONTROLS_UNAVAILABLE"),
            (paid.replace("</li></ul>", "<div class='variation-price'>¥ 1,200</div></li></ul>"), "FREE_FILE_CONTROLS_UNAVAILABLE"),
            (paid.replace("class='add-cart'", "class='authored-unknown'"), "FREE_ITEM_ROLE_UNPROVEN"),
            (paid.replace("</ul>", "<li class='variation-item'><div class='variation-price'>¥ 0</div><form><button class='add-cart' data-product-id='900001'>Authored unoffered free row</button></form></li></ul>"), "FREE_FILE_CONTROLS_UNAVAILABLE"),
            (paid.replace("</div></body>", "<div class='text-14 text-text-default font-normal text-left mb-8'>orphan.zip</div></div></body>"), "FREE_ITEM_ORPHAN_ENTRY"),
            (paid.replace("</div></body>", "<span class='cmd-label'>無料ダウンロード</span></div></body>"), "FREE_ITEM_ORPHAN_ENTRY"),
            (paid.replace("</div></body>", "<a href='https://booth.pm/downloadables/700009?variation_id=800009'>Authored orphan</a></div></body>"), "FREE_ITEM_ORPHAN_CONTROL"),
            (paid.replace("</div></body>", "<div data-dropdown-items='[]'></div></div></body>"), "FREE_ITEM_UNKNOWN_DROPDOWN"),
            (paid.replace("<ul id='variations'>", "<ul id='variations' data-file-count='1'>"), "FREE_ITEM_COUNT_UNPROVEN"),
            (paid.replace("</div></body>", "<button aria-expanded='false'></button></div></body>"), "FREE_ITEM_DYNAMIC_LIST"),
            (paid.replace("data-product-id='900001'>Authored donation", "data-product-id='900002'>Authored donation"), "FREE_ITEM_IDENTITY_UNPROVEN"),
            (paid.replace("</ul>", "<li class='variation-item'></li></ul>"), "FREE_ITEM_ROLE_UNPROVEN"),
            (paid.replace("<li class='variation-item'><div class='variation-price'>¥ 1,200</div><form><button class='add-cart' data-product-id='900001'>Authored donation</button></form></li>", ""), "FREE_ITEM_LIST_UNPROVEN"),
        ] {
            let evidence = parse(&html, "900001").unwrap();
            assert_eq!(evidence.error_code.as_deref(), Some(code));
            assert!(!evidence.unavailable);
            assert!(!evidence.manifest_complete);
        }
        let free = parse(HTML, "900001").unwrap();
        assert!(free.manifest_complete);
        assert!(!free.unavailable);
        assert_eq!(free.files.len(), 1);
    }
    #[test]
    fn priced_paused_or_restock_rows_preserve_all_proven_shared_wrap_files() {
        let base = shared_wrap_html_with_files(7);
        let expected = parse(&base, "900001").unwrap();
        assert!(expected.manifest_complete);
        assert_eq!(expected.files.len(), 7);
        let expected_files = serde_json::to_value(&expected.files).unwrap();
        for role in [
            "<button class='add-cart disabled' type='button'>Authored paused offer</button>",
            "<div class='restock-request-links'><a class='js-add-restock-request'>Authored notification</a><a class='js-remove-restock-request'>Authored removal</a></div>",
        ] {
            let row = format!("<li class='variation-item'><div class='variation-price'>¥ 1,200</div>{role}</li>");
            let html = base.replace("</ul>", &format!("{row}</ul>"));
            let evidence = parse(&html, "900001").unwrap();
            assert!(evidence.manifest_complete);
            assert!(!evidence.unavailable);
            assert!(evidence.error_code.is_none());
            assert_eq!(serde_json::to_value(&evidence.files).unwrap(), expected_files);
            for price in ["¥ 0", "¥ 1,200–2,000", "$ 1,200", "1200", "¥ 12,00"] {
                let invalid = html.replace("¥ 1,200", price);
                let evidence = parse(&invalid, "900001").unwrap();
                assert_eq!(evidence.error_code.as_deref(), Some("FREE_ITEM_ROLE_UNPROVEN"), "{price}");
                assert!(!evidence.manifest_complete);
                assert!(!evidence.unavailable);
            }
            for invalid in [
                html.replace("class='variation-price'>¥ 1,200", "class='authored-other-price'>¥ 1,200"),
                html.replace("</li></ul>", "<div class='variation-price'>¥ 1,200</div></li></ul>"),
                html.replace(role, "<button class='authored-unknown'>Authored unknown role</button>"),
            ] {
                assert_eq!(parse(&invalid, "900001").unwrap().error_code.as_deref(), Some("FREE_ITEM_ROLE_UNPROVEN"));
            }
            for (invalid, code) in [
                (html.replace("</div></body>", "<div class='text-14 text-text-default font-normal text-left mb-8'>orphan.zip</div></div></body>"), "FREE_ITEM_ORPHAN_ENTRY"),
                (html.replace("</div></body>", "<a href='https://booth.pm/downloadables/700009?variation_id=800009'>Authored orphan</a></div></body>"), "FREE_ITEM_ORPHAN_CONTROL"),
                (html.replace("</div></body>", "<div data-dropdown-items='[]'></div></div></body>"), "FREE_ITEM_UNKNOWN_DROPDOWN"),
                (html.replace("<ul id='variations'>", "<ul id='variations' data-file-count='8'>"), "FREE_ITEM_COUNT_UNPROVEN"),
                (html.replace("</div></body>", "<button aria-expanded='false'></button></div></body>"), "FREE_ITEM_DYNAMIC_LIST"),
            ] {
                let evidence = parse(&invalid, "900001").unwrap();
                assert_eq!(evidence.error_code.as_deref(), Some(code));
                assert!(!evidence.manifest_complete);
            }
        }
    }
    #[test]
    fn proven_download_controls_do_not_depend_on_display_labels() {
        let original = "<span class='cmd-label'>無料ダウンロード</span>";
        for html in [HTML.to_owned(), shared_wrap_html()] {
            let expected = parse(&html, "900001").unwrap();
            assert!(expected.manifest_complete);
            let expected_files = serde_json::to_value(&expected.files).unwrap();
            for label in [
                "<span class='cmd-label'>ダウンロード</span>",
                "<span class='cmd-label'>Download</span>",
                "",
                "<span class='cmd-label'>無料ダウンロード</span><span class='cmd-label'>Download</span>",
                "<span class='cmd-label'>authored purchase</span>",
                "<span class='cmd-label'>Authored unavailable</span>",
            ] {
                let evidence = parse(&html.replace(original, label), "900001").unwrap();
                assert!(evidence.manifest_complete);
                assert!(evidence.error_code.is_none());
                assert!(!evidence.unavailable);
                assert_eq!(serde_json::to_value(&evidence.files).unwrap(), expected_files);
                assert_eq!(evidence.page().unwrap().unresolved, 0);
            }
        }
    }
    #[test]
    fn shared_cart_wrap_requires_a_complete_filename_control_bijection() {
        let html = shared_wrap_html();
        let proof = parse(&html, "900001").unwrap();
        assert!(proof.manifest_complete);
        assert_eq!(proof.files.len(), 4);
        for i in 1..=4 {
            let file = proof
                .files
                .iter()
                .find(|f| f.id == format!("70000{i}"))
                .unwrap();
            assert_eq!(
                file.variant_id.as_deref(),
                Some(format!("80000{i}").as_str())
            );
            assert_eq!(
                file.name.as_deref(),
                Some(format!("authored-{i}.zip").as_str())
            );
            assert_eq!(file.revision, "unknown");
        }
        let name_node = "<div class='text-14 text-text-default font-normal text-left mb-8'>authored-4.zip</div>";
        for bad in [
            html.replace("title='authored-2.zip'", "title='authored-1.zip'"),
            html.replace("authored-2.zip</div>", "authored-1.zip</div>"),
            html.replace("title='authored-1.zip'", "title='authored-2.zip'"),
            html.replace("title='authored-1.zip'", ""),
            html.replace(name_node, ""),
            html.replace(name_node, &format!("{name_node}{name_node}")),
            html.replace("title='authored-4.zip'", "title='unmatched.zip'"),
            html.replace("700002?variation_id=800002", "700001?variation_id=800001"),
            html.replace(
                "700001?variation_id=800001",
                "700001?token=private&variation_id=800001",
            ),
            html.replace(
                name_node,
                &format!("<div class='cart-button-wrap'>{name_node}</div>"),
            ),
            html.replace(
                "<a title='authored-1.zip'",
                "<div class='cart-button-wrap'><a title='authored-1.zip'",
            )
            .replacen("</a>", "</a></div>", 1),
            html.replace(
                "</ul>",
                "<button class='add-cart' data-product-id='900001'></button></ul>",
            )
            .replace(
                "authored-4.zip</div>",
                "authored-4.zip</div><button class='add-cart' data-product-id='900001'></button>",
            ),
        ] {
            let proof = parse(&bad, "900001").unwrap();
            assert!(!proof.manifest_complete);
            assert!(proof.files.is_empty());
        }
        // Equal counts and two distinct names must not let A/A titles consume A twice.
        let ambiguous = html.replace("title='authored-2.zip'", "title='authored-1.zip'");
        assert_eq!(
            parse(&ambiguous, "900001").unwrap().error_code.as_deref(),
            Some("FREE_ITEM_FILENAME_UNPROVEN")
        );
    }
    #[test]
    fn shared_wrap_menus_cannot_grant_or_cross_file_scopes() {
        let html = shared_wrap_html();
        let menu = r#"<div class='js-download-free-button' data-test='other-downloads-button' data-dropdown-items='[{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?client=booth-library-manager&variation_id=800001"}]'></div>"#;
        assert!(html.contains(menu));
        let two_same_file_methods = html.replacen("}]'", r#"},{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?client=vroid&variation_id=800001&type=zip"}]'"#, 1);
        assert!(
            parse(&two_same_file_methods, "900001")
                .unwrap()
                .manifest_complete
        );
        for bad in [
            html.replace("700001/deeplink", "700099/deeplink"),
            html.replace("variation_id=800001\"", "variation_id=800099\""),
            html.replace("client=booth-library-manager", "client=unknown"),
            html.replace("client=booth-library-manager", "client=vroid&type=unknown"),
            html.replace("client=booth-library-manager", "client=vroid&client=booth-library-manager"),
            html.replace("variation_id=800001\"", "no_variant=800001\""),
            html.replace("js-download-free-button", "unknown-method"),
            html.replace("client=booth-library-manager", "token=private&client=booth-library-manager"),
            html.replace("700001/deeplink", "700001/../700001/deeplink"),
            html.replace("700001/deeplink", "%37%30%30%30%30%31/deeplink"),
            html.replace("https://booth.pm/downloadables/700001/deeplink", "https://evil.test/downloadables/700001/deeplink"),
            html.replace(menu, &format!("<div class='cart-button-wrap'>{menu}</div>")),
            html.replace(menu, "").replace("</ul>", &format!("</ul>{menu}")),
            html.replace("data-dropdown-items='[{", "data-dropdown-items='not-json[{"),
            html.replacen("}]'", r#"},{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700002/deeplink?client=booth-library-manager&variation_id=800002"}]'"#, 1),
        ] {
            let proof = parse(&bad, "900001").unwrap();
            assert!(!proof.manifest_complete);
            assert!(proof.files.is_empty());
        }
        let separate = HTML.replace("</ul>", &format!("<li class='variation-item'><div class='cart-button-wrap'><a title='other.zip' href='https://booth.pm/downloadables/700002?variation_id=800002'><span class='cmd-label'>無料ダウンロード</span></a><div class='text-14 text-text-default font-normal text-left mb-8'>other.zip</div>{menu}</div></li></ul>"));
        assert_eq!(
            parse(&separate, "900001").unwrap().error_code.as_deref(),
            Some("FREE_ITEM_UNKNOWN_DROPDOWN")
        );
    }
    #[test]
    fn canonical_shop_alias_keeps_exact_og_product_and_list_guards() {
        let expected = "https://booth.pm/ja/items/900001";
        for alias in [
            "https://synthetic-shop.booth.pm/items/900001",
            "https://synthetic-shop.booth.pm/ja/items/900001",
            "https://synthetic-shop.booth.pm/en/items/900001",
        ] {
            let html = HTML.replacen(expected, alias, 1);
            assert!(parse(&html, "900001").unwrap().manifest_complete);
            for bad in [
                html.replace(
                    "content='https://booth.pm/ja/items/900001'",
                    &format!("content='{alias}'"),
                ),
                html.replace("data-product-id='900001'", "data-product-id='900002'"),
                html.replace("<ul id='variations'>", "<ul id='other'>"),
                html.replace(
                    "<ul id='variations'>",
                    "<ul id='variations' data-item-id='900002'>",
                ),
                html.replace(
                    "</head>",
                    &format!("<link rel='canonical' href='{alias}'></head>"),
                ),
            ] {
                assert!(!parse(&bad, "900001").unwrap().manifest_complete);
            }
        }
        for alias in [
            "https://booth.pm/items/900001",
            "https://booth.pm/en/items/900001",
            "https://www.booth.pm/items/900001",
            "https://accounts.booth.pm/items/900001",
            "https://manage.booth.pm/items/900001",
            "https://synthetic-shop.booth.pm/items/900002",
            "https://synthetic-shop.booth.pm:443/items/900001",
            "https://user@synthetic-shop.booth.pm/items/900001",
            "https://synthetic-shop.booth.pm/items/900001?q=1",
            "https://synthetic-shop.booth.pm/items/900001#x",
            "https://synthetic-shop.booth.pm/ja/../items/900001",
            "https://synthetic-shop.booth.pm/items/%39%30%30%30%30%31",
            "https://synthetic-shop.booth.pm/items/900001/",
            "https://synthetic-shop.booth.pm.evil.test/items/900001",
            "http://synthetic-shop.booth.pm/items/900001",
        ] {
            let proof = parse(&HTML.replacen(expected, alias, 1), "900001").unwrap();
            assert!(!proof.manifest_complete);
            assert_eq!(
                proof.error_code.as_deref(),
                Some("FREE_ITEM_IDENTITY_UNPROVEN")
            );
        }
    }
    #[test]
    fn strict_free_controls_bind_identity_names_and_variants_without_acquisitions() {
        let second="<li class='variation-item'><div class='cart-button-wrap'><a title='docs.psd' href='https://booth.pm/downloadables/700002?variation_id=800002'><span class='cmd-label'>無料ダウンロード</span></a><div class='text-14 text-text-default font-normal text-left mb-8'>docs.psd</div></div></li>";
        let evidence = parse(&HTML.replace("</ul>", &format!("{second}</ul>")), "900001").unwrap();
        assert!(evidence.manifest_complete);
        assert_eq!(evidence.files.len(), 2);
        assert!(evidence.files.iter().all(|f| f.revision == "unknown"));
        let page = evidence.page().unwrap();
        assert!(page.rows.is_empty());
        assert!(page.is_detail);
        assert!(!page.terminal);
        assert_eq!(page.unresolved, 0);
    }
    #[test]
    fn unknown_or_incomplete_free_controls_never_certify_an_empty_manifest() {
        let first_row = HTML
            .split_once("<li class='variation-item'>")
            .unwrap()
            .1
            .split_once("</li>")
            .unwrap()
            .0;
        let duplicate = format!("<li class='variation-item'>{first_row}</li>");
        for html in [
            HTML.replace("</ul>", &format!("{duplicate}</ul>")),
            HTML.replace("<body>", "<body><div id='items' class='market' data-product-id='900001'></div>"),
            HTML.replace("</ul>", "</ul><a title='orphan.zip' href='https://booth.pm/downloadables/700003?variation_id=800003'><span class='cmd-label'>無料ダウンロード</span></a>"),
            HTML.replace("<li class='variation-item'>", "<li class='variation-item' data-variation-id='800009'>"),
            HTML.replace("<li class='variation-item'>", "<li class='variation-item' data-product-id='900002'>"),
            HTML.replace("</ul>", "<li class='variation-item'><select name='unrecognized'></select></li></ul>"),
            HTML.replace("data-product-id='900001'","data-product-id='900002'"),
            HTML.replace("<ul id='variations'>","<ul id='variations'><ul id='variations'></ul>"),
            HTML.replace("authored.zip</div>","other.zip</div>"),
            HTML.replace("?variation_id=800001", "?token=PRIVATE_TOKEN&variation_id=800001"),
            HTML.replace("?variation_id=800001", ""),
            HTML.replace("</ul>", "<li class='variation-item'><span class='cmd-label'>無料ダウンロード</span></li></ul>"),
            HTML.replace("</ul>", "<li class='variation-item'><div class='text-14 text-text-default font-normal text-left mb-8'>orphan.zip</div></li></ul>"),
            HTML.replace("</ul>", "<button aria-expanded='false'></button></ul>"),
            HTML.replace("<ul id='variations'>", "<ul id='variations' data-file-count='2'>"),
            HTML.replace("</ul>", "<div data-dropdown-items='[]'></div></ul>"),
            HTML.replace("https://booth.pm/downloadables/700001?variation_id=800001", "https://evil.test/downloadables/700001?variation_id=800001"),
            HTML.replace("<a title=", "<a data-href='https://booth.pm/downloadables/700001' title="),
            HTML.replace("</head>", "<link rel='canonical' href='https://booth.pm/ja/items/900001'></head>"),
            HTML.replace("<li class='variation-item'>", "<li>"),
        ] {
            let evidence=parse(&html,"900001").unwrap(); assert!(!evidence.manifest_complete); assert!(evidence.files.is_empty()); assert!(evidence.error_code.is_some()); assert_eq!(evidence.page().unwrap().unresolved,1);
        }
        let absent = HTML.replace("<ul id='variations'>", "<ul id='changed'>");
        assert!(!parse(&absent, "900001").unwrap().manifest_complete);
        assert!(parse(HTML, "900002").unwrap().files.is_empty());
    }
    #[test]
    fn variation_list_identity_cannot_contradict_expected_product() {
        for attribute in ["data-product-id", "data-item-id"] {
            let conflict = HTML.replace(
                "<ul id='variations'>",
                &format!("<ul id='variations' {attribute}='900002'>"),
            );
            let evidence = parse(&conflict, "900001").unwrap();
            assert!(!evidence.manifest_complete);
            assert!(evidence.files.is_empty());
            assert_eq!(
                evidence.error_code.as_deref(),
                Some("FREE_ITEM_IDENTITY_UNPROVEN")
            );
            let matching = HTML.replace(
                "<ul id='variations'>",
                &format!("<ul id='variations' {attribute}='900001'>"),
            );
            assert!(parse(&matching, "900001").unwrap().manifest_complete);
        }
    }
    #[test]
    fn dropdown_outside_variation_list_cannot_hide_an_unobserved_file() {
        let orphan = r#"<div data-dropdown-items='[{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700099/deeplink?client=booth-library-manager&variation_id=800099"}]'></div>"#;
        let html = HTML.replace("</ul>", &format!("</ul>{orphan}"));
        let evidence = parse(&html, "900001").unwrap();
        assert!(!evidence.manifest_complete);
        assert!(evidence.files.is_empty());
        assert_eq!(
            evidence.error_code.as_deref(),
            Some("FREE_ITEM_UNKNOWN_DROPDOWN")
        );
        assert_eq!(evidence.page().unwrap().unresolved, 1);
    }
    #[test]
    fn alternative_methods_are_bound_to_the_same_observed_file() {
        let method = r#"<div class='js-download-free-button' data-test='other-downloads-button' data-dropdown-items='[{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700001/deeplink?client=booth-library-manager&variation_id=800001"}]'></div>"#;
        let html = HTML.replace("authored.zip</div>", &format!("authored.zip</div>{method}"));
        assert!(parse(&html, "900001").unwrap().manifest_complete);
        let counts = serde_json::to_value(diagnose_entry(&html, "900001").unwrap()).unwrap();
        assert_eq!(counts["knownAlternateMethods"], 1);
        assert_eq!(counts["unknownDropdowns"], 0);
        assert!(
            !parse(
                &html.replace("700001/deeplink", "700002/deeplink"),
                "900001"
            )
            .unwrap()
            .manifest_complete
        );
        assert!(
            !parse(
                &html.replace("variation_id=800001\"", "variation_id=800002\""),
                "900001"
            )
            .unwrap()
            .manifest_complete
        );
    }
    #[test]
    fn vroid_methods_type_only_their_own_file() {
        let menu = |file: &str, kind: &str| format!(r#"<div class='js-download-free-button' data-test='other-downloads-button' data-dropdown-items='[{{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/{file}/deeplink?client=booth-library-manager&variation_id=800001"}},{{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/{file}/deeplink?client=vroid&type={kind}&variation_id=800001"}}]'></div>"#);
        let with = |name: &str, kind: &str| {
            HTML.replace("authored.zip", name)
                .replace(&format!("{name}</div>"), &format!("{name}</div>{}", menu("700001", kind)))
        };
        for (name, kind) in [
            ("authored.vroidcustomitem", "vroidcustomitem"),
            ("authored.vroid", "vroid"),
            ("Authored.VROID", "vroid"),
        ] {
            let evidence = parse(&with(name, kind), "900001").unwrap();
            assert!(evidence.manifest_complete && evidence.error_code.is_none());
            assert_eq!(evidence.files.len(), 1);
            assert_eq!(evidence.files[0].name.as_deref(), Some(name));
            let counts = serde_json::to_value(diagnose_entry(&with(name, kind), "900001").unwrap()).unwrap();
            assert_eq!(counts["knownAlternateMethods"], 1);
            assert_eq!(counts["unknownDropdowns"], 0);
        }
        for (name, kind) in [
            ("authored.vroidcustomitem", "zip"),
            ("authored.vroidcustomitem", "vroid"),
            ("authored.vroid", ""),
            ("authored", "authored"),
            ("authored.", ""),
        ] {
            let evidence = parse(&with(name, kind), "900001").unwrap();
            assert_eq!(evidence.error_code.as_deref(), Some("FREE_ITEM_UNKNOWN_DROPDOWN"), "{name} {kind}");
            assert!(evidence.files.is_empty());
        }
        // In a shared wrap the type follows the menu's own file, not a neighbour.
        let shared = shared_wrap_html_with_files(2).replace("authored-2.zip", "authored-2.vroidcustomitem");
        let typed = |kind: &str| shared.replace("variation_id=800002\"}]", &format!(r#"variation_id=800002"}},{{"deeplinkDownloadableUrl":"https://booth.pm/downloadables/700002/deeplink?client=vroid&type={kind}&variation_id=800002"}}]"#));
        assert_eq!(parse(&typed("vroidcustomitem"), "900001").unwrap().files.len(), 2);
        assert_eq!(parse(&typed("zip"), "900001").unwrap().error_code.as_deref(), Some("FREE_ITEM_UNKNOWN_DROPDOWN"));
    }
}
