//! Original parsers for the source-derived Gumroad buyer/public payloads.
//! Deployed-shape verification and collector wiring are separate from parsing.
//! Bearer content/resolver routes are transient capabilities, never catalogue fields.
use anyhow::{bail, ensure, Result};
use chrono::{DateTime, Utc};
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::BTreeSet;
use url::Url;

const LIMIT: usize = 8 * 1024 * 1024;

/// No Debug or Serialize: the path/query can grant access to private purchased content.
#[derive(Clone)]
pub struct TransientRoute(Url);
impl TransientRoute {
    pub fn url(&self) -> &Url {
        &self.0
    }
}

fn trusted_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|host| host == "gumroad.com" || host.ends_with(".gumroad.com"))
}

fn transient_route(value: &Value, base: &Url) -> Result<Option<TransientRoute>> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => {
            if value.is_empty() {
                return Ok(None);
            }
            let url = base
                .join(value)
                .map_err(|_| anyhow::anyhow!("GUMROAD_ROUTE_UNSUPPORTED"))?;
            ensure!(
                trusted_url(&url) && url.fragment().is_none(),
                "GUMROAD_ROUTE_UNSUPPORTED"
            );
            Ok(Some(TransientRoute(url)))
        }
        _ => bail!("GUMROAD_ROUTE_UNSUPPORTED"),
    }
}

fn file_route(value: &Value, file_id: &str) -> Result<Option<TransientRoute>> {
    let root = Url::parse("https://gumroad.com/").unwrap();
    let route = transient_route(value, &root)?;
    if let Some(route) = &route {
        let url = route.url();
        let path: Vec<_> = url.path().split('/').collect();
        ensure!(
            url.host_str() == Some("gumroad.com")
                && path.len() == 4
                && path[1] == "r"
                && !path[2].is_empty()
                && path[3] == "product_files",
            "GUMROAD_FILE_BINDING_UNPROVEN"
        );
        let query: Vec<_> = url.query_pairs().collect();
        ensure!(
            query.len() == 1 && query[0].0 == "product_file_ids[]" && query[0].1 == file_id,
            "GUMROAD_FILE_BINDING_UNPROVEN"
        );
    }
    Ok(route)
}

fn identifier(value: &Value) -> Result<String> {
    let value = value
        .as_str()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 1024
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'=' | b'+' | b'/')
                })
        })
        .ok_or_else(|| anyhow::anyhow!("GUMROAD_IDENTITY_UNPROVEN"))?;
    Ok(value.to_owned())
}

fn optional_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 4096)
        .map(str::to_owned)
}

fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}

fn needed_items(value: &Value) -> Value {
    let Some(items) = value.as_array() else {
        return Value::Null;
    };
    Value::Array(
        items
            .iter()
            .map(|item| {
                let mut needed = pick(
                    item,
                    &[
                        "type",
                        "id",
                        "name",
                        "file_name",
                        "file_size",
                        "pdf_stamp_enabled",
                        "download_url",
                        "stream_url",
                        "read_url",
                        "external_link_url",
                    ],
                );
                if item.get("children").is_some() {
                    needed["children"] = needed_items(&item["children"]);
                }
                if let Some(subtitles) = item["subtitle_files"].as_array() {
                    needed["subtitle_files"] = Value::Array(
                        subtitles
                            .iter()
                            .map(|item| pick(item, &["download_url"]))
                            .collect(),
                    );
                }
                needed
            })
            .collect(),
    )
}

/// Reduces either SSR or current plain history.state.page to needed observations.
/// The result still contains transient bearer routes; it is not a portable record or diagnostic.
pub fn necessary_page(page: &Value) -> Result<Value> {
    let component = page["component"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("GUMROAD_SOURCE_UNSUPPORTED"))?;
    ensure!(
        page["props"].is_object() && page["url"].is_string(),
        "GUMROAD_SOURCE_UNSUPPORTED"
    );
    // A protected buyer request redirected to the observed login page needs login,
    // while unfamiliar components remain unsupported source shapes.
    ensure!(component != "Logins/New", "AUTH_REQUIRED");
    let props = &page["props"];
    let needed = if component == "Library/Index" {
        let mut needed = pick(props, &["archived_count", "unarchived_count"]);
        // Sole authentication subject: neither current_seller nor display/profile fields bind a buyer.
        needed["buyerIdentity"] = props["logged_in_user"]["id"].clone();
        needed["pagination"] = pick(
            &props["pagination"],
            &["page", "pages", "from", "to", "count"],
        );
        needed["search"] = pick(
            &props["search"],
            &["sort", "query", "creators", "bundles", "show_archived_only"],
        );
        needed["bundles"] = Value::Array(
            props["bundles"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|bundle| pick(bundle, &["id", "label"]))
                .collect(),
        );
        needed["results"] = match props["results"].as_array() {
            Some(rows) => Value::Array(rows.iter().map(|row| {
                let mut product = pick(&row["product"], &["name", "thumbnail_url", "native_type"]);
                product["creator"] = pick(&row["product"]["creator"], &["name"]);
                serde_json::json!({"product":product,"purchase":pick(&row["purchase"], &["id", "is_archived", "download_url", "variants"])})
            }).collect()),
            None => Value::Null,
        };
        needed
    } else if component == "UrlRedirects/DownloadPage" {
        let mut purchase = pick(
            &props["purchase"],
            &[
                "id",
                "receipt_purchase_id",
                "product_id",
                "created_at",
                "product_name",
                "variant_id",
                "product_available",
                "product_long_url",
            ],
        );
        if props["purchase"]["membership"].is_object() {
            purchase["membership"] = serde_json::json!({});
        }
        let bundles = props["content"]["bundle_products"].as_array().map(|items| {
            Value::Array(
                items
                    .iter()
                    .map(|item| pick(item, &["id", "name", "url"]))
                    .collect(),
            )
        });
        serde_json::json!({"purchase":purchase,"content_unavailability_reason_code":props["content_unavailability_reason_code"],"content":{"content_items":needed_items(&props["content"]["content_items"]),"bundle_products":bundles}})
    } else {
        bail!("GUMROAD_SOURCE_UNSUPPORTED");
    };
    Ok(serde_json::json!({"component":component,"url":page["url"],"props":needed}))
}

/// Reads only an explicit Inertia data-page payload; never evaluates scripts or private HTML text.
/// Shared login/user/token/license props are discarded; bearer routes remain transient.
pub fn inertia_page(html: &str) -> Result<Value> {
    ensure!(html.len() <= LIMIT, "GUMROAD_METADATA_TOO_LARGE");
    let document = Html::parse_document(html);
    let selector = Selector::parse("[data-page]").unwrap();
    let mut nodes = document.select(&selector);
    let node = nodes
        .next()
        .ok_or_else(|| anyhow::anyhow!("GUMROAD_SOURCE_UNSUPPORTED"))?;
    ensure!(nodes.next().is_none(), "GUMROAD_SOURCE_AMBIGUOUS");
    let payload = node.value().attr("data-page").unwrap();
    ensure!(payload.len() <= LIMIT, "GUMROAD_METADATA_TOO_LARGE");
    let page: Value =
        serde_json::from_str(payload).map_err(|_| anyhow::anyhow!("GUMROAD_SOURCE_UNSUPPORTED"))?;
    ensure!(
        page["props"].is_object() && page["component"].is_string() && page["url"].is_string(),
        "GUMROAD_SOURCE_UNSUPPORTED"
    );
    necessary_page(&page)
}

#[derive(Clone)]
pub struct LibraryPurchase {
    pub purchase_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub thumbnail_url: Option<String>,
    pub native_type: Option<String>,
    pub variant_name: Option<String>,
    pub archived: bool,
    pub content_route: Option<TransientRoute>,
}

impl LibraryPurchase {
    pub fn excluded_non_asset(&self) -> bool {
        matches!(
            self.native_type.as_deref(),
            Some("physical" | "call" | "coffee" | "commission")
        )
    }
}

pub struct LibraryPage {
    pub buyer_identity: String,
    pub pages: u64,
    pub count: u64,
    pub archived_count: u64,
    pub unarchived_count: u64,
    pub purchases: Vec<LibraryPurchase>,
    pub bundles: Vec<LibraryBundle>,
}
#[derive(Clone)]
pub struct LibraryBundle {
    pub product_id: String,
    pub label: Option<String>,
}

/// Expected page/archive scope must match the returned props; stale Inertia state cannot certify a page.
pub fn parse_library_props(
    props: &Value,
    base: &Url,
    expected_page: u64,
    archived: bool,
) -> Result<LibraryPage> {
    parse_library_scope(props, base, expected_page, archived, None)
}
pub fn parse_library_scope(
    props: &Value,
    base: &Url,
    expected_page: u64,
    archived: bool,
    bundle: Option<&str>,
) -> Result<LibraryPage> {
    ensure!(
        trusted_url(base) && base.path() == "/library",
        "GUMROAD_ROUTE_UNSUPPORTED"
    );
    let number = |value: &Value| {
        value
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("GUMROAD_PAGINATION_UNPROVEN"))
    };
    let pagination = &props["pagination"];
    let page = number(&pagination["page"])?;
    let pages = number(&pagination["pages"])?;
    let count = number(&pagination["count"])?;
    ensure!(
        page == expected_page && page >= 1 && pages >= page && pages <= 10000,
        "GUMROAD_PAGINATION_UNPROVEN"
    );
    ensure!(
        props["search"]["show_archived_only"].as_bool() == Some(archived),
        "GUMROAD_SCOPE_CHANGED"
    );
    ensure!(
        props["search"]["query"].as_str() == Some("")
            && props["search"]["creators"]
                .as_array()
                .is_some_and(Vec::is_empty)
            && props["search"]["bundles"]
                .as_array()
                .is_some_and(|ids| match bundle {
                    None => ids.is_empty(),
                    Some(bundle) => ids.len() == 1 && ids[0].as_str() == Some(bundle),
                }),
        "GUMROAD_SCOPE_CHANGED"
    );
    ensure!(
        matches!(
            props["search"]["sort"].as_str(),
            Some("purchase_date" | "recently_updated")
        ),
        "GUMROAD_SOURCE_UNSUPPORTED"
    );
    let rows = props["results"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("GUMROAD_SOURCE_UNSUPPORTED"))?;
    ensure!(
        rows.len() <= 1000 && (count != 0 || rows.is_empty()) && (count == 0 || !rows.is_empty()),
        "GUMROAD_PAGINATION_UNPROVEN"
    );
    let from = number(&pagination["from"])?;
    let to = number(&pagination["to"])?;
    ensure!(
        if count == 0 {
            from == 0 && to == 0 && pages == 1
        } else {
            from >= 1 && to >= from && to <= count && to - from + 1 == rows.len() as u64
        },
        "GUMROAD_PAGINATION_UNPROVEN"
    );
    let archived_count = number(&props["archived_count"])?;
    let unarchived_count = number(&props["unarchived_count"])?;
    // Bundle parent purchases may be replaced by member cards; tab totals precede that replacement.
    ensure!(
        count
            <= if archived {
                archived_count
            } else {
                unarchived_count
            },
        "GUMROAD_PAGINATION_UNPROVEN"
    );
    let mut seen = BTreeSet::new();
    let mut purchases = Vec::with_capacity(rows.len());
    for row in rows {
        let purchase = &row["purchase"];
        let product = &row["product"];
        let purchase_id = identifier(&purchase["id"])?;
        ensure!(
            seen.insert(purchase_id.clone()),
            "GUMROAD_PURCHASE_AMBIGUOUS"
        );
        ensure!(
            purchase["is_archived"].as_bool() == Some(archived),
            "GUMROAD_SCOPE_CHANGED"
        );
        purchases.push(LibraryPurchase {
            purchase_id,
            archived,
            title: optional_text(&product["name"]),
            author: optional_text(&product["creator"]["name"]),
            thumbnail_url: optional_text(&product["thumbnail_url"]),
            native_type: optional_text(&product["native_type"]),
            variant_name: optional_text(&purchase["variants"]),
            content_route: transient_route(&purchase["download_url"], base)?,
        });
    }
    let buyer_identity = identifier(
        props
            .get("buyerIdentity")
            .unwrap_or(&props["logged_in_user"]["id"]),
    )?;
    let mut bundle_ids = BTreeSet::new();
    let mut bundles = Vec::new();
    for row in props["bundles"].as_array().into_iter().flatten() {
        let product_id = identifier(&row["id"])?;
        ensure!(
            bundle_ids.insert(product_id.clone()),
            "GUMROAD_PURCHASE_AMBIGUOUS"
        );
        bundles.push(LibraryBundle {
            product_id,
            label: optional_text(&row["label"]),
        });
    }
    ensure!(
        bundle.is_none_or(|id| bundle_ids.contains(id)),
        "GUMROAD_BUNDLE_OWNERSHIP_UNPROVEN"
    );
    Ok(LibraryPage {
        buyer_identity,
        pages,
        count,
        archived_count,
        unarchived_count,
        purchases,
        bundles,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAccess {
    Downloadable,
    ViewOnly,
    ExternalReference,
    Unavailable,
}

#[derive(Clone)]
pub struct FileObservation {
    pub file_id: String,
    pub source_product_id: String,
    pub source_purchase_id: String,
    pub name: Option<String>,
    pub folders: Vec<String>,
    pub size_bytes: Option<u64>,
    pub access: FileAccess,
    pub resolver: Option<TransientRoute>,
}

#[derive(Clone)]
pub struct ContentObservation {
    pub purchase_id: String,
    pub receipt_purchase_id: Option<String>,
    pub product_id: String,
    pub variant_id: Option<String>,
    pub title: Option<String>,
    pub acquired_at: Option<DateTime<Utc>>,
    pub product_available: Option<bool>,
    pub has_membership: bool,
    pub files: Vec<FileObservation>,
    pub inventory_complete: bool,
    pub unsupported_items: usize,
    pub public_product_url: Option<Url>,
    pub bundles: Vec<BundleObservation>,
    pub source_bundle_id: Option<String>,
    pub member_purchase_ids: BTreeSet<String>,
}

#[derive(Clone)]
pub struct BundleObservation {
    pub product_id: String,
    pub content_route: Option<TransientRoute>,
}

impl ContentObservation {
    pub fn catalogue_product_id(&self) -> String {
        format!("gumroad:{}", self.product_id)
    }
}

fn file_items(
    items: &[Value],
    folders: &[String],
    base: &Url,
    files: &mut Vec<FileObservation>,
    seen: &mut BTreeSet<String>,
    unresolved: &mut usize,
    depth: usize,
) -> Result<()> {
    ensure!(
        depth <= 32 && files.len() + items.len() <= 10000,
        "GUMROAD_INVENTORY_TOO_LARGE"
    );
    for item in items {
        match item["type"].as_str() {
            Some("folder") => {
                let Some(name) = optional_text(&item["name"]) else {
                    *unresolved += 1;
                    continue;
                };
                let Some(children) = item["children"].as_array() else {
                    *unresolved += 1;
                    continue;
                };
                let mut path = folders.to_vec();
                path.push(name);
                file_items(children, &path, base, files, seen, unresolved, depth + 1)?;
            }
            Some("file") => {
                let Ok(file_id) = identifier(&item["id"]) else {
                    *unresolved += 1;
                    continue;
                };
                if !seen.insert(file_id.clone()) {
                    *unresolved += 1;
                    if let Some(previous) = files.iter_mut().find(|file| file.file_id == file_id) {
                        previous.access = FileAccess::Unavailable;
                        previous.resolver = None;
                    }
                    continue;
                }
                let resolver = match file_route(&item["download_url"], &file_id) {
                    Ok(route) => route,
                    Err(_) => {
                        *unresolved += 1;
                        None
                    }
                };
                let access = if resolver.is_some() {
                    FileAccess::Downloadable
                } else if item["external_link_url"]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
                {
                    FileAccess::ExternalReference
                } else if ["stream_url", "read_url"]
                    .iter()
                    .any(|key| item[*key].as_str().is_some_and(|value| !value.is_empty()))
                {
                    FileAccess::ViewOnly
                } else {
                    FileAccess::Unavailable
                };
                let name = optional_text(&item["file_name"]);
                let has_additional_downloads = item["subtitle_files"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["download_url"].is_string()));
                if access == FileAccess::Unavailable
                    || (access == FileAccess::Downloadable && name.is_none())
                    || has_additional_downloads
                {
                    *unresolved += 1;
                }
                files.push(FileObservation {
                    file_id,
                    source_product_id: String::new(),
                    source_purchase_id: String::new(),
                    name,
                    folders: folders.to_vec(),
                    // content_length is playback duration/page count, not byte length.
                    // Stamping serves a different per-purchase PDF object from the original upload.
                    size_bytes: if item["pdf_stamp_enabled"] == true {
                        None
                    } else {
                        item["file_size"].as_u64()
                    },
                    access,
                    resolver,
                });
            }
            _ => *unresolved += 1,
        }
    }
    Ok(())
}

/// Purchase identity is supplied by the enumerated Library card, never inferred from title or URL.
pub fn parse_content_props(
    props: &Value,
    base: &Url,
    expected_purchase_id: &str,
    expected_product_id: Option<&str>,
) -> Result<ContentObservation> {
    ensure!(trusted_url(base), "GUMROAD_ROUTE_UNSUPPORTED");
    let purchase = &props["purchase"];
    let purchase_id = identifier(&purchase["id"])?;
    let product_id = identifier(&purchase["product_id"])?;
    ensure!(
        purchase_id == expected_purchase_id
            && expected_product_id.is_none_or(|id| id == product_id),
        "GUMROAD_PRODUCT_BINDING_UNPROVEN"
    );
    let optional_id = |value: &Value| {
        if value.is_null() {
            Ok(None)
        } else {
            identifier(value).map(Some)
        }
    };
    let acquired_at = purchase["created_at"]
        .as_str()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let mut files = Vec::new();
    let mut unsupported_items = 0;
    match props["content"]["content_items"].as_array() {
        Some(items) => file_items(
            items,
            &[],
            base,
            &mut files,
            &mut BTreeSet::new(),
            &mut unsupported_items,
            0,
        )?,
        None => unsupported_items += 1,
    }
    let mut bundles = Vec::new();
    if let Some(members) = props["content"]["bundle_products"].as_array() {
        let mut seen = BTreeSet::new();
        for member in members {
            let Ok(product_id) = identifier(&member["id"]) else {
                unsupported_items += 1;
                continue;
            };
            if !seen.insert(product_id.clone()) {
                unsupported_items += 1;
                continue;
            }
            let content_route = transient_route(&member["url"], base).ok().flatten();
            bundles.push(BundleObservation {
                product_id,
                content_route,
            });
            // Parent content alone does not prove each bundle member's inventory.
            unsupported_items += 1;
        }
    }
    if props["content_unavailability_reason_code"]
        .as_str()
        .is_some_and(|value| !value.is_empty())
    {
        unsupported_items += 1;
    }
    let public_product_url = purchase["product_long_url"]
        .as_str()
        .and_then(|value| Url::parse(value).ok())
        .filter(|url| {
            trusted_url(url)
                && url.path().starts_with("/l/")
                && url.query().is_none()
                && url.fragment().is_none()
        });
    for file in &mut files {
        file.source_product_id = product_id.clone();
        file.source_purchase_id = purchase_id.clone();
    }
    Ok(ContentObservation {
        purchase_id,
        product_id,
        receipt_purchase_id: optional_id(&purchase["receipt_purchase_id"])?,
        variant_id: optional_id(&purchase["variant_id"])?,
        title: optional_text(&purchase["product_name"]),
        acquired_at,
        product_available: purchase["product_available"].as_bool(),
        has_membership: purchase["membership"].is_object(),
        inventory_complete: unsupported_items == 0,
        unsupported_items,
        files,
        public_product_url,
        bundles,
        source_bundle_id: None,
        member_purchase_ids: BTreeSet::new(),
    })
}

/// Source units are preserved as price_cents; presentation/conversion belongs to the money contract.
#[derive(Clone, Debug)]
pub struct PublicPrice {
    pub price_cents: u64,
    pub currency_code: String,
    pub pay_what_you_want: bool,
    pub recurring: bool,
}

impl PublicPrice {
    pub fn usd_amount(&self) -> Option<f64> {
        (self.currency_code == "USD" && self.price_cents <= 9_007_199_254_740_991)
            .then(|| self.price_cents as f64 / 100.0)
    }
}

#[derive(Clone, Debug)]
pub struct PublicProductObservation {
    pub product_id: String,
    // Missing/unknown preserves the last observed body; an observed empty string clears it.
    pub description: Option<String>,
    pub current_price: Option<PublicPrice>,
    pub price_formatted: Option<String>,
    pub price_source_currency: Option<String>,
}

fn public_description(raw: &str) -> String {
    const TEXT_LIMIT: usize = 40_000;
    let bounded: String = raw.chars().take(TEXT_LIMIT * 4).collect();
    let fragment = Html::parse_fragment(&bounded);
    let mut text = String::new();
    let mut pending: Vec<_> = fragment.root_element().children()
        .rev().map(|node| (node, false)).collect();
    while let Some((node, closing)) = pending.pop() {
        if closing {
            text.push(' ');
            continue;
        }
        if let Some(part) = node.value().as_text() {
            text.extend(
                part.chars()
                    .filter(|character| !character.is_control() || character.is_whitespace()),
            );
            continue;
        }
        if let Some(element) = node.value().as_element() {
            if matches!(element.name(), "script" | "style" | "template" | "noscript" | "nav") {
                continue;
            }
            if matches!(element.name(),
                "address" | "article" | "aside" | "blockquote" | "br" | "dd" | "details"
                | "div" | "dl" | "dt" | "fieldset" | "figcaption" | "figure" | "footer"
                | "form" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header"
                | "hgroup" | "hr" | "li" | "main" | "ol" | "p" | "pre" | "section"
                | "summary" | "table" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" | "ul"
            ) {
                text.push(' ');
                pending.push((node, true));
            }
        }
        pending.extend(node.children().rev().map(|child| (child, false)));
    }
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(TEXT_LIMIT)
        .collect()
}

pub fn parse_public_product(
    value: &Value,
    expected_product_id: Option<&str>,
) -> Result<PublicProductObservation> {
    ensure!(
        value["api_version"].as_u64() == Some(1),
        "GUMROAD_PUBLIC_SCHEMA_UNSUPPORTED"
    );
    let product_id = identifier(&value["id"])?;
    ensure!(
        expected_product_id.is_none_or(|id| id == product_id),
        "GUMROAD_PRODUCT_BINDING_UNPROVEN"
    );
    let currency = value["currency_code"]
        .as_str()
        .filter(|value| value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_alphabetic()));
    let current_price = match (
        value["price_cents"].as_u64(),
        currency,
        value["is_pay_what_you_want"].as_bool(),
        value["is_recurring_billing"].as_bool(),
    ) {
        (Some(price_cents), Some(currency), Some(pay_what_you_want), Some(recurring)) => {
            Some(PublicPrice {
                price_cents,
                currency_code: currency.to_ascii_uppercase(),
                pay_what_you_want,
                recurring,
            })
        }
        _ => None,
    };
    Ok(PublicProductObservation {
        product_id,
        description: value["description"].as_str().map(public_description),
        current_price,
        price_formatted: value["price_formatted"]
            .as_str()
            .map(str::trim)
            .filter(|text| {
                !text.is_empty()
                    && text.chars().count() <= 256
                    && !text.chars().any(char::is_control)
            })
            .map(str::to_owned),
        price_source_currency: currency.map(str::to_ascii_uppercase),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inertia_login_page_requires_authentication_without_accepting_unknown_pages() {
        let html = |page: Value| {
            let payload = serde_json::to_string(&page)
                .unwrap()
                .replace('&', "&amp;")
                .replace('"', "&quot;");
            format!("<div data-page=\"{payload}\"></div>")
        };
        let login = json!({"component":"Logins/New","url":"/login","props":{}});
        assert_eq!(
            inertia_page(&html(login.clone()))
                .err()
                .unwrap()
                .to_string(),
            "AUTH_REQUIRED"
        );
        let unknown = json!({"component":"Unknown/Page","url":"/unknown","props":{}});
        assert_eq!(
            inertia_page(&html(unknown)).err().unwrap().to_string(),
            "GUMROAD_SOURCE_UNSUPPORTED"
        );
        let mut malformed = login;
        malformed["props"] = Value::Null;
        assert_eq!(
            inertia_page(&html(malformed)).err().unwrap().to_string(),
            "GUMROAD_SOURCE_UNSUPPORTED"
        );
        assert_eq!(
            inertia_page(&" ".repeat(LIMIT + 1))
                .err()
                .unwrap()
                .to_string(),
            "GUMROAD_METADATA_TOO_LARGE"
        );
    }

    fn library() -> Value {
        let results: Vec<Value> = (0..15).map(|index| json!({"product":{"name":"Authored fixture","native_type":"digital"},"purchase":{"id":format!("authored-purchase-{index}"),"is_archived":false,"download_url":"/d/authored-capability","variants":"Standard"}})).collect();
        json!({"results":results,"logged_in_user":{"id":"authored-buyer"},
            "pagination":{"page":1,"pages":38,"count":569,"from":1,"to":15},"archived_count":3,"unarchived_count":574,
            "search":{"sort":"purchase_date","query":"","creators":[],"bundles":[],"show_archived_only":false}})
    }
    fn content() -> Value {
        json!({"purchase":{"id":"authored-purchase","receipt_purchase_id":"authored-purchase","product_id":"authored-product","created_at":"2026-10-08T01:02:03Z","product_available":true},
            "content":{"content_items":[{"type":"folder","id":"authored-folder","name":"Originals","children":[
                {"type":"file","id":"authored-file-one","file_name":"fixture.unitypackage","file_size":42,"pdf_stamp_enabled":false,"content_length":null,"download_url":"/r/authored-capability/product_files?product_file_ids[]=authored-file-one"},
                {"type":"file","id":"authored-file-two","file_name":"guide.pdf","file_size":0,"pdf_stamp_enabled":false,"content_length":7,"download_url":"/r/authored-capability/product_files?product_file_ids[]=authored-file-two"}]}]}})
    }

    #[test]
    fn public_price_keeps_exact_product_and_currency_units_separate_from_paid_evidence() {
        let mut value = json!({"api_version":1,"id":"authored-product","price_cents":19900,"currency_code":"usd","price_formatted":"$199","is_pay_what_you_want":false,"is_recurring_billing":false});
        let public = parse_public_product(&value, Some("authored-product")).unwrap();
        assert_eq!(public.current_price.unwrap().usd_amount(), Some(199.0));
        assert!(parse_public_product(&value, Some("other-product")).is_err());
        value["price_cents"] = json!(0);
        assert_eq!(
            parse_public_product(&value, None)
                .unwrap()
                .current_price
                .unwrap()
                .usd_amount(),
            Some(0.0)
        );
        value["currency_code"] = json!("jpy");
        value["price_cents"] = json!(1000);
        value["price_formatted"] = json!("¥1,000");
        let public = parse_public_product(&value, None).unwrap();
        assert!(public.current_price.unwrap().usd_amount().is_none());
        assert_eq!(public.price_formatted.as_deref(), Some("¥1,000"));
        assert_eq!(public.price_source_currency.as_deref(), Some("JPY"));
        value["currency_code"] = json!("usd");
        value["price_cents"] = json!(9_007_199_254_740_992u64);
        value["price_formatted"] = json!("$1\nscript");
        let public = parse_public_product(&value, None).unwrap();
        assert!(public.current_price.unwrap().usd_amount().is_none());
        assert!(public.price_formatted.is_none());
    }

    #[test]
    fn public_description_observation_distinguishes_unknown_empty_and_visible_text() {
        let mut value = json!({"api_version":1,"id":"authored-product"});
        assert!(parse_public_product(&value, None).unwrap().description.is_none());
        for unknown in [Value::Null, json!({"text":"VRChat"}), json!(42)] {
            value["description"] = unknown;
            assert!(parse_public_product(&value, None).unwrap().description.is_none());
        }
        for empty in ["", " \n\t", "<p></p>"] {
            value["description"] = json!(empty);
            assert_eq!(parse_public_product(&value, None).unwrap().description.as_deref(), Some(""));
        }
        value["description"] = json!("<p>Authored &amp; decoded</p><p>For VR<strong>Chat</strong><br>VRC &#x8863;&#x88c5;</p><a href='https://example.invalid/VRChat' title='VRChat'>Read manual</a><img alt='VRChat'><script>VRChat</script><style>VRC</style><nav>VRChat</nav><template>VRChat</template><!-- VRC -->");
        assert_eq!(
            parse_public_product(&value, None).unwrap().description.as_deref(),
            Some("Authored & decoded For VRChat VRC 衣装 Read manual")
        );
        value["description"] = json!("<a href='https://example.invalid/VRChat' title='VRChat'>Read manual</a><img alt='VRChat'><script>VRChat</script><style>VRC</style><nav>VRChat</nav>");
        assert_eq!(parse_public_product(&value, None).unwrap().description.as_deref(), Some("Read manual"));
        value["description"] = json!("&lt;literal&gt; &amp;copy; VRChat");
        assert_eq!(parse_public_product(&value, None).unwrap().description.as_deref(), Some("<literal> &copy; VRChat"));
        for blocks in ["<p>VR</p><p>Chat</p>", "VR<br>Chat", "VR<hr>Chat"] {
            value["description"] = json!(blocks);
            assert_eq!(parse_public_product(&value, None).unwrap().description.as_deref(), Some("VR Chat"));
        }
        value["description"] = json!("猫".repeat(40_001));
        assert_eq!(parse_public_product(&value, None).unwrap().description.unwrap().chars().count(), 40_000);
    }

    #[test]
    fn buyer_library_rejects_stale_page_filtered_scope_and_duplicate_purchase_identity() {
        let base = Url::parse("https://gumroad.com/library?page=1").unwrap();
        let parsed = parse_library_props(&library(), &base, 1, false).unwrap();
        assert_eq!(parsed.pages, 38);
        assert_eq!(parsed.purchases[0].purchase_id, "authored-purchase-0");
        assert!(parsed.purchases[0].content_route.is_some());
        assert!(parse_library_props(&library(), &base, 2, false).is_err());
        assert!(parse_library_props(&library(), &base, 1, true).is_err());
        let mut filtered = library();
        filtered["search"]["creators"] = json!(["authored-creator"]);
        assert!(parse_library_props(&filtered, &base, 1, false).is_err());
        let mut duplicate = library();
        duplicate["results"][1] = library()["results"][0].clone();
        assert!(parse_library_props(&duplicate, &base, 1, false).is_err());
        let mut truncated = library();
        truncated["results"].as_array_mut().unwrap().pop();
        assert!(parse_library_props(&truncated, &base, 1, false).is_err());
        let mut empty_archive = library();
        empty_archive["results"] = json!([]);
        empty_archive["pagination"] = json!({"page":1,"pages":1,"from":0,"to":0,"count":0});
        empty_archive["archived_count"] = json!(0);
        empty_archive["search"]["show_archived_only"] = json!(true);
        let empty = parse_library_props(&empty_archive, &base, 1, true).unwrap();
        assert_eq!(empty.pages, 1);
        assert!(empty.purchases.is_empty());
        for native_type in ["physical", "call", "coffee", "commission"] {
            let mut non_asset = library();
            non_asset["results"][0]["product"]["native_type"] = json!(native_type);
            let page = parse_library_props(&non_asset, &base, 1, false).unwrap();
            assert!(page.purchases[0].excluded_non_asset());
            assert!(!page.purchases[1].excluded_non_asset());
        }
    }

    #[test]
    fn content_binds_purchase_product_and_every_nested_file_without_using_bearer_identity() {
        let base = Url::parse("https://gumroad.com/d/authored-capability").unwrap();
        let parsed = parse_content_props(
            &content(),
            &base,
            "authored-purchase",
            Some("authored-product"),
        )
        .unwrap();
        assert!(parsed.inventory_complete);
        assert_eq!(parsed.catalogue_product_id(), "gumroad:authored-product");
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[1].size_bytes, Some(0));
        assert_eq!(parsed.files[0].folders, ["Originals"]);
        assert_eq!(
            parsed.acquired_at.unwrap().date_naive().to_string(),
            "2026-10-08"
        );
        assert!(parse_content_props(&content(), &base, "other-purchase", None).is_err());
        assert!(parse_content_props(
            &content(),
            &base,
            "authored-purchase",
            Some("other-product")
        )
        .is_err());
        let mut unknown = content();
        unknown["content"]["content_items"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"unknown-file-control"}));
        let parsed = parse_content_props(&unknown, &base, "authored-purchase", None).unwrap();
        assert!(!parsed.inventory_complete);
        assert_eq!(parsed.files.len(), 2);
        assert!(parsed.acquired_at.is_some());

        let reference = json!({"type":"file","id":"authored-external-reference","file_name":"External reference","external_link_url":"https://example.invalid/authored-reference"});
        let mut mixed = content();
        mixed["content"]["content_items"]
            .as_array_mut()
            .unwrap()
            .push(reference.clone());
        let mixed = parse_content_props(&mixed, &base, "authored-purchase", None).unwrap();
        assert!(mixed.inventory_complete);
        assert_eq!(mixed.unsupported_items, 0);
        assert_eq!(
            mixed
                .files
                .iter()
                .filter(|file| file.access == FileAccess::Downloadable)
                .map(|file| file.file_id.as_str())
                .collect::<Vec<_>>(),
            ["authored-file-one", "authored-file-two"]
        );
        assert_eq!(mixed.files[2].access, FileAccess::ExternalReference);
        assert_eq!(mixed.files[2].name.as_deref(), Some("External reference"));
        assert!(mixed.files[2].resolver.is_none());
        assert_eq!(mixed.acquired_at, parsed.acquired_at);

        let mut external_only = content();
        external_only["content"]["content_items"] = json!([reference]);
        let external_only =
            parse_content_props(&external_only, &base, "authored-purchase", None).unwrap();
        assert!(external_only.inventory_complete);
        assert_eq!(external_only.unsupported_items, 0);
        assert!(external_only
            .files
            .iter()
            .all(|file| file.access == FileAccess::ExternalReference && file.resolver.is_none()));
        assert!(external_only.acquired_at.is_some());
    }

    #[test]
    fn byte_length_uses_file_size_and_stamped_files_keep_identity_without_claiming_original_size() {
        let base = Url::parse("https://gumroad.com/d/authored-capability").unwrap();
        let mut value = content();
        let media = &mut value["content"]["content_items"][0]["children"][0];
        media["file_name"] = json!("authored.mp3");
        media["file_size"] = json!(65536);
        media["content_length"] = json!(240);
        let parsed = parse_content_props(&value, &base, "authored-purchase", None).unwrap();
        assert_eq!(parsed.files[0].size_bytes, Some(65536));
        value["content"]["content_items"][0]["children"][1]["pdf_stamp_enabled"] = json!(true);
        let stamped = parse_content_props(&value, &base, "authored-purchase", None).unwrap();
        assert!(stamped.inventory_complete);
        assert_eq!(stamped.files[0].size_bytes, Some(65536));
        assert!(stamped.files[1].size_bytes.is_none());
        assert_eq!(stamped.files[1].file_id, parsed.files[1].file_id);
        assert_eq!(stamped.files[1].access, FileAccess::Downloadable);
        assert_eq!(stamped.acquired_at, parsed.acquired_at);
    }

    #[test]
    fn unsafe_or_ambiguous_file_controls_keep_bound_date_but_never_complete_inventory() {
        let base = Url::parse("https://gumroad.com/d/authored-capability").unwrap();
        let mut duplicate = content();
        let file = duplicate["content"]["content_items"][0]["children"][0].clone();
        duplicate["content"]["content_items"][0]["children"]
            .as_array_mut()
            .unwrap()
            .push(file);
        let parsed = parse_content_props(&duplicate, &base, "authored-purchase", None).unwrap();
        assert!(!parsed.inventory_complete);
        assert!(parsed.files[0].resolver.is_none());
        assert!(parsed.acquired_at.is_some());
        let mut unsafe_file = content();
        unsafe_file["content"]["content_items"][0]["children"][0]["download_url"] =
            json!("https://gumroad.com.evil.test/fixture.zip");
        let parsed = parse_content_props(&unsafe_file, &base, "authored-purchase", None).unwrap();
        assert!(!parsed.inventory_complete);
        assert!(parsed.files[0].resolver.is_none());
        assert!(parsed.acquired_at.is_some());
        unsafe_file["content"]["content_items"][0]["children"][0]["external_link_url"] =
            json!("https://example.invalid/authored-reference");
        let parsed = parse_content_props(&unsafe_file, &base, "authored-purchase", None).unwrap();
        assert!(!parsed.inventory_complete);
        assert_eq!(parsed.files[0].access, FileAccess::ExternalReference);
        assert!(parsed.files[0].resolver.is_none());
        assert!(parsed.acquired_at.is_some());
        let public = json!({"api_version":1,"id":"authored-product","name":"Fixture","price_cents":0,"currency_code":"usd","is_pay_what_you_want":true,"is_recurring_billing":false,"created_at":"2000-01-01T00:00:00Z"});
        let quote = parse_public_product(&public, Some("authored-product"))
            .unwrap()
            .current_price
            .unwrap();
        assert_eq!(quote.price_cents, 0);
        assert!(quote.pay_what_you_want);
        assert_eq!(quote.currency_code, "USD");
        assert!(parse_public_product(&public, Some("other-product")).is_err());
    }

    #[test]
    fn inertia_allowlist_discards_shared_user_tokens_license_and_unrelated_private_props() {
        let page = json!({"component":"Library/Index","url":"/library","props":library()});
        let mut page = page;
        page["props"]["user"] = json!({"email":"authored-sensitive","token":"authored-sensitive"});
        page["props"]["token"] = json!("authored-sensitive");
        page["props"]["purchase_analytics"] = json!({"value":"authored-sensitive"});
        let needed = necessary_page(&page).unwrap();
        assert!(!needed.to_string().contains("authored-sensitive"));
        let mut page = json!({"component":"UrlRedirects/DownloadPage","url":"/d/authored-capability","props":content()});
        page["props"]["content"]["license"] = json!("authored-sensitive");
        page["props"]["purchase"]["email"] = json!("authored-sensitive");
        assert!(!necessary_page(&page)
            .unwrap()
            .to_string()
            .contains("authored-sensitive"));
    }

    #[test]
    fn bundle_and_empty_controls_never_certify_only_the_parent_inventory() {
        let base = Url::parse("https://gumroad.com/d/authored-capability").unwrap();
        let mut value = content();
        value["content"]["bundle_products"] =
            json!([{"id":"authored-member","name":"Member","url":"/d/authored-member-capability"}]);
        let page = necessary_page(
            &json!({"component":"UrlRedirects/DownloadPage","url":base.as_str(),"props":value}),
        )
        .unwrap();
        let parsed = parse_content_props(&page["props"], &base, "authored-purchase", None).unwrap();
        assert!(!parsed.inventory_complete);
        assert_eq!(parsed.bundles.len(), 1);
        assert_eq!(parsed.files.len(), 2);
        let mut empty = content();
        empty["content"]["content_items"][0]["children"][0]["download_url"] = json!("");
        let parsed = parse_content_props(&empty, &base, "authored-purchase", None).unwrap();
        assert!(!parsed.inventory_complete);
        assert!(parsed.files[0].resolver.is_none());
        let mut impostor = library();
        impostor["logged_in_user"] = Value::Null;
        impostor["current_seller"] = json!({"id":"authored-buyer"});
        assert!(parse_library_props(
            &impostor,
            &Url::parse("https://gumroad.com/library").unwrap(),
            1,
            false
        )
        .is_err());
    }

    #[test]
    fn file_controls_require_one_exact_file_identity_on_the_verified_root_route() {
        let base = Url::parse("https://gumroad.com/d/authored-capability").unwrap();
        for route in [
            "/r/authored-capability/product_files?product_file_ids[]=other-file",
            "/r/authored-capability/product_files?product_file_ids[]=authored-file-one&product_file_ids[]=authored-file-two",
            "/r/authored-capability/product_files?product_file_ids[]=authored-file-one&other=true",
            "/r/authored-capability/product_files",
            "/d/authored-capability/download?product_file_ids[]=authored-file-one",
        ] {
            let mut value = content(); value["content"]["content_items"][0]["children"][0]["download_url"] = json!(route);
            let parsed = parse_content_props(&value, &base, "authored-purchase", None).unwrap();
            assert!(!parsed.inventory_complete);
            assert!(parsed.files[0].resolver.is_none());
            assert!(parsed.acquired_at.is_some());
        }
        let parsed = parse_content_props(&content(), &base, "authored-purchase", None).unwrap();
        assert_eq!(
            parsed.files[0].resolver.as_ref().unwrap().url().path(),
            "/r/authored-capability/product_files"
        );
    }
}
