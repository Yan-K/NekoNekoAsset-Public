//! Original Jinxxy buyer-page parser. Apollo transport is data, never executable code.
//! Only needed observations leave this module; licenses and unrelated caches are discarded.
use super::{
    http::{build_client, MetadataHttp},
    session, DownloadTicket, StoreProvider,
};
use crate::catalogue::Catalogue;
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use url::Url;

const LIMIT: usize = 16 * 1024 * 1024;
const MARKER: &str = "(window[Symbol.for(\"ApolloSSRDataTransport\")]";
const FILE_HOST: &str = "jinxxy-products.62bb56dad0a3633d04ee066263e8507d.r2.cloudflarestorage.com";
fn public_download_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && octets[0] != 0
                && octets[0] < 224
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 198 && matches!(octets[1], 18 | 19))
                && !(octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_download_ip(mapped.into());
            }
            let segments = ip.segments();
            // Native public unicast only; exclude documentation and transition ranges.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] <= 0x1ff)
                && !(segments[0] == 0x2001 && segments[1] == 0xdb8)
                && segments[0] != 0x2002
        }
    }
}
fn compiled_destination(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && match url.host() {
            Some(url::Host::Ipv4(ip)) => public_download_ip(ip.into()),
            Some(url::Host::Ipv6(ip)) => public_download_ip(ip.into()),
            Some(url::Host::Domain(host)) => {
                host.contains('.')
                    && !host.ends_with('.')
                    && !host.ends_with(".localhost")
                    && !host.ends_with(".local")
                    && !host.ends_with(".internal")
            }
            None => false,
        }
}
// No Debug/Serialize: this capability is consumed once and never enters records or IPC.
struct CompiledCapability {
    url: Url,
    authorization: reqwest::header::HeaderValue,
}
fn compiled_available(file: &Value) -> bool {
    file["file_meta"]["meta_type"] == "t3b_meta"
        && file["file_meta"]["asset_bundles"]
            .as_object()
            .is_some_and(|bundles| {
                bundles
                    .values()
                    .any(|bundle| bundle.is_object() || bundle == true)
            })
}
fn compiled_request(inventory: &str, file: &FileObservation) -> Result<Value> {
    identity(&json!(inventory))?;
    identity(&json!(file.id))?;
    ensure!(
        file.compiled && file.compiled_available,
        "JINXXY_COMPILED_DOWNLOAD_UNAVAILABLE"
    );
    // Ownership and selected original IDs were checked against the fresh buyer detail.
    Ok(json!({"operationName":"downloadT3bFile", "query":
        "mutation downloadT3bFile($inventory_id: ID!, $file_ids: [ID!]!) { inventory_item(id: $inventory_id) { downloadT3bFile(file_ids: $file_ids) { url token } } }",
        "variables":{"inventory_id":inventory,"file_ids":[file.id]}}))
}
fn compiled_capability(response: &Value) -> Result<CompiledCapability> {
    ensure!(
        response["errors"].is_null() || response["errors"].as_array().is_some_and(Vec::is_empty),
        "JINXXY_DOWNLOAD_UNPROVEN"
    );
    let row = &response["data"]["inventory_item"];
    let capability = &row["downloadT3bFile"];
    let url = capability["url"]
        .as_str()
        .and_then(|value| Url::parse(value).ok())
        .filter(compiled_destination)
        .ok_or_else(|| anyhow::anyhow!("UNSAFE_DOWNLOAD_URL"))?;
    let token = capability["token"]
        .as_str()
        .filter(|token| !token.is_empty() && token.len() <= 16_384)
        .ok_or_else(|| anyhow::anyhow!("JINXXY_DOWNLOAD_UNPROVEN"))?;
    let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow::anyhow!("JINXXY_DOWNLOAD_UNPROVEN"))?;
    authorization.set_sensitive(true);
    Ok(CompiledCapability { url, authorization })
}
impl CompiledCapability {
    async fn ticket(self) -> Result<DownloadTicket> {
        use std::net::ToSocketAddrs;
        let host = self.url.host_str().unwrap().to_owned();
        let lookup = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let addresses = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::task::spawn_blocking(move || {
                (lookup.as_str(), 443)
                    .to_socket_addrs()
                    .map(|addresses| addresses.collect::<Vec<_>>())
            }),
        )
        .await
        .map_err(|_| anyhow::anyhow!("NETWORK_UNAVAILABLE"))?
        .map_err(|_| anyhow::anyhow!("NETWORK_UNAVAILABLE"))?
        .map_err(|_| anyhow::anyhow!("NETWORK_UNAVAILABLE"))?;
        ensure!(
            !addresses.is_empty()
                && addresses
                    .iter()
                    .all(|address| public_download_ip(address.ip())),
            "UNSAFE_DOWNLOAD_URL"
        );
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, self.authorization);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(30))
            .resolve_to_addrs(&host, &addresses)
            .build()
            .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))?;
        let expected_origin = Url::parse(&self.url.origin().ascii_serialization())
            .map_err(|_| anyhow::anyhow!("UNSAFE_DOWNLOAD_URL"))?;
        Ok(DownloadTicket {
            provider: StoreProvider::Jinxxy,
            url: self.url,
            client,
            expected_size: None,
            expected_origin: Some(expected_origin),
        })
    }
}
fn file_destination(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(FILE_HOST)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.path() != "/"
}
fn resolved_location(status: reqwest::StatusCode, location: Option<&str>) -> Result<Url> {
    ensure!(
        status == reqwest::StatusCode::FOUND,
        "JINXXY_DOWNLOAD_UNAVAILABLE"
    );
    let url = Url::parse(location.ok_or_else(|| anyhow::anyhow!("JINXXY_DOWNLOAD_UNPROVEN"))?)
        .map_err(|_| anyhow::anyhow!("JINXXY_DOWNLOAD_UNPROVEN"))?;
    ensure!(file_destination(&url), "UNSAFE_DOWNLOAD_URL");
    Ok(url)
}

/// Apollo emits the JavaScript primitive undefined. Replace tokens outside JSON strings,
/// retaining all string bytes and escapes. No other JavaScript syntax is accepted.
fn normalize_undefined(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let (mut quoted, mut escaped, mut index) = (false, false, 0);
    let ident =
        |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || byte >= 128;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            index += 1;
        } else if byte == b'"' {
            quoted = true;
            output.push(byte);
            index += 1;
        } else if bytes[index..].starts_with(b"undefined")
            && (index == 0 || !ident(bytes[index - 1]))
            && (index + 9 == bytes.len() || !ident(bytes[index + 9]))
        {
            output.extend_from_slice(b"null");
            index += 9;
        } else {
            output.push(byte);
            index += 1;
        }
    }
    // Input is valid UTF-8 and replacement only changes complete ASCII tokens.
    String::from_utf8(output).unwrap()
}

pub(crate) fn transport_objects(script: &str) -> Result<Vec<Value>> {
    let mut remaining = script.trim();
    let mut result = Vec::new();
    if !remaining.starts_with(MARKER) {
        return Ok(result);
    }
    while !remaining.is_empty() {
        ensure!(
            remaining.starts_with(MARKER),
            "JINXXY_TRANSPORT_UNSUPPORTED"
        );
        let open = remaining
            .find('{')
            .ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?;
        let wrapper: String = remaining[..open]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        ensure!(
            wrapper == "(window[Symbol.for(\"ApolloSSRDataTransport\")]??=[]).push(",
            "JINXXY_TRANSPORT_UNSUPPORTED"
        );
        let (mut depth, mut quoted, mut escaped, mut end) = (0usize, false, false, None);
        for (offset, byte) in remaining.as_bytes()[open..].iter().copied().enumerate() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
            } else {
                match byte {
                    b'"' => quoted = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth
                            .checked_sub(1)
                            .ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?;
                        if depth == 0 {
                            end = Some(open + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let end = end.ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?;
        let value = serde_json::from_str(&normalize_undefined(&remaining[open..end]))
            .map_err(|_| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?;
        result.push(value);
        remaining = remaining[end..]
            .trim_start()
            .strip_prefix(')')
            .ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?
            .trim_start();
        remaining = remaining.strip_prefix(';').unwrap_or(remaining).trim();
    }
    Ok(result)
}

fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .filter(|v| !v.is_array() && !v.is_object())
                    .map(|v| ((*key).to_owned(), v.clone()))
            })
            .collect(),
    )
}
fn identity(value: &Value) -> Result<String> {
    value
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 20
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.parse::<u64>().is_ok_and(|id| id > 0)
        })
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("JINXXY_IDENTITY_UNPROVEN"))
}
fn optional_identity(value: &Value) -> Result<Option<String>> {
    if value.is_null() {
        Ok(None)
    } else {
        identity(value).map(Some)
    }
}
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty() && text.len() <= 16_384)
        .map(str::to_owned)
}
fn date(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
}
fn public_url(value: &Value) -> Option<String> {
    let url = Url::parse(value.as_str()?).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then(|| url.to_string())
}
fn product_url(item: &Value) -> Option<String> {
    let seller = item["user"]["username"].as_str()?;
    let slug = item["url"].as_str()?;
    let segment = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !matches!(value, "." | "..")
            && !value
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | '?' | '#' | '%'))
    };
    if !segment(seller) || !segment(slug) {
        return None;
    }
    let mut url = Url::parse("https://jinxxy.com/").ok()?;
    url.path_segments_mut().ok()?.extend([seller, slug]);
    Some(url.to_string())
}
fn usd_minor_amount(value: &Value, currency: &Value) -> Option<f64> {
    if currency != "USD" {
        return None;
    }
    value
        .as_u64()
        .filter(|minor| *minor <= 9_007_199_254_740_991)
        .map(|minor| minor as f64 / 100.0)
}
fn needed_inventory(row: &Value, detail: bool) -> Value {
    let mut result = pick(
        row,
        &["id", "imported_from", "imported_purchased_at", "created_at"],
    );
    result["order"] = pick(&row["order"], &["id"]);
    // Keep only whether it is a gift, never sender/recipient metadata.
    result["isGift"] = json!(!row["gift"].is_null() && row["gift"] != false);
    let mut item = pick(
        &row["item"],
        &[
            "id",
            "name",
            "url",
            "created_at",
            "version",
            "is_deleted",
            "visibility",
            "base_price",
            "currency_code",
            "description",
        ],
    );
    item["user"] = pick(&row["item"]["user"], &["id", "username"]);
    item["cover"] = pick(&row["item"]["cover"], &["url"]);
    if detail {
        item["files"] = match row["item"]["files"].as_array() {
            Some(files) => Value::Array(
                files
                    .iter()
                    .map(|file| {
                        let mut selected = pick(
                            file,
                            &[
                                "id",
                                "file_name",
                                "size",
                                "updated_at",
                                "content_type",
                                "extension",
                            ],
                        );
                        selected["file_meta"] = pick(&file["file_meta"], &["meta_type"]);
                        if let Some(bundles) = file["file_meta"]["asset_bundles"].as_object() {
                            selected["file_meta"]["asset_bundles"] = Value::Object(
                                bundles
                                    .iter()
                                    .map(|(platform, bundle)| {
                                        (platform.clone(), json!(bundle.is_object()))
                                    })
                                    .collect(),
                            );
                        }
                        selected
                    })
                    .collect(),
            ),
            None => Value::Null,
        };
    }
    result["item"] = item;
    result["listing"] = pick(&row["listing"], &["service", "name", "url"]);
    result
}

fn needed_data(data: &Value) -> Value {
    let mut result = json!({});
    if let Some(user) = data.get("user").filter(|user| user.is_object()) {
        result["user"] = pick(user, &["id", "username"]);
    }
    if let Some(page) = data.get("inventory_items").filter(|v| v.is_object()) {
        let mut selected = pick(page, &["page", "page_count"]);
        selected["payload"] = match page["payload"].as_array() {
            Some(rows) => Value::Array(
                rows.iter()
                    .map(|row| needed_inventory(row, false))
                    .collect(),
            ),
            None => Value::Null,
        };
        result["inventory_items"] = selected;
    }
    if let Some(row) = data.get("inventory_item").filter(|v| v.is_object()) {
        result["inventory_item"] = needed_inventory(row, true);
    }
    result
}

/// Read every transport script. Loading/undefined payloads cannot conceal later ready data.
/// Only explicit query data paths are read; arbitrary Apollo cache objects are ignored.
pub fn buyer_data(html: &str) -> Result<Vec<Value>> {
    ensure!(html.len() <= LIMIT, "JINXXY_METADATA_TOO_LARGE");
    let document = Html::parse_document(html);
    let mut result = Vec::new();
    for script in document.select(&Selector::parse("script").unwrap()) {
        for transport in transport_objects(&script.inner_html())? {
            if let Some(queries) = transport["rehydrate"].as_object() {
                for query in queries.values() {
                    let needed = needed_data(&query["data"]);
                    if needed.as_object().is_some_and(|value| !value.is_empty()) {
                        result.push(needed);
                    }
                }
            }
            if let Some(events) = transport["events"].as_array() {
                for event in events {
                    let needed = needed_data(&event["value"]["data"]);
                    if needed.as_object().is_some_and(|value| !value.is_empty()) {
                        result.push(needed);
                    }
                }
            }
        }
    }
    ensure!(!result.is_empty(), "JINXXY_SOURCE_UNSUPPORTED");
    Ok(result)
}

#[derive(Default)]
pub struct OrderPayment {
    pub paid_at: Option<DateTime<Utc>>,
    pub paid_price: Option<f64>,
    pub paid_currency: Option<String>,
}

/// Flight is decoded as JSON literals only. References, executable expressions and
/// unrelated React records cannot become receipt evidence.
fn flight_records(html: &str) -> Result<Vec<Value>> {
    ensure!(html.len() <= LIMIT, "JINXXY_METADATA_TOO_LARGE");
    let document = Html::parse_document(html);
    let mut flight = String::new();
    const PUSH: &str = "self.__next_f.push(";
    for script in document.select(&Selector::parse("script").unwrap()) {
        let content = script.inner_html();
        let mut remaining = content.trim();
        if !remaining.starts_with(PUSH) {
            continue;
        }
        while let Some(literal) = remaining.strip_prefix(PUSH) {
            let mut decoder = serde_json::Deserializer::from_str(literal).into_iter::<Value>();
            let value = decoder
                .next()
                .transpose()
                .map_err(|_| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?
                .ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?;
            remaining = literal[decoder.byte_offset()..]
                .trim_start()
                .strip_prefix(')')
                .ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?
                .trim_start();
            remaining = remaining
                .strip_prefix(';')
                .unwrap_or(remaining)
                .trim_start();
            if value[0] == 1 {
                let chunk = value[1]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("JINXXY_TRANSPORT_UNSUPPORTED"))?;
                ensure!(
                    flight.len() + chunk.len() <= LIMIT,
                    "JINXXY_METADATA_TOO_LARGE"
                );
                flight.push_str(chunk);
            }
        }
        ensure!(remaining.is_empty(), "JINXXY_TRANSPORT_UNSUPPORTED");
    }
    Ok(flight
        .lines()
        .filter_map(|line| {
            let (id, body) = line.split_once(':')?;
            if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            serde_json::from_str(body).ok()
        })
        .collect())
}

fn needed_orders(value: &Value, result: &mut Vec<Value>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "order" && child.is_object() {
                    let mut order =
                        pick(child, &["id", "paid_at", "payment_status", "currency_code"]);
                    order["order_items"] = Value::Array(
                        child["order_items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|line| {
                                let mut selected = pick(line, &["target_type", "gross_price"]);
                                selected["inventory_item"] = pick(&line["inventory_item"], &["id"]);
                                selected["inventory_item"]["item"] =
                                    pick(&line["inventory_item"]["item"], &["id"]);
                                selected
                            })
                            .collect(),
                    );
                    result.push(order);
                } else {
                    needed_orders(child, result);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                needed_orders(child, result);
            }
        }
        _ => {}
    }
}

pub fn parse_order_payment(
    html: &str,
    order_id: &str,
    inventory: &str,
    product: &str,
) -> Result<OrderPayment> {
    let mut orders = Vec::new();
    for record in flight_records(html)? {
        needed_orders(&record, &mut orders);
    }
    let orders: BTreeSet<_> = orders
        .into_iter()
        .filter(|order| order["id"] == order_id)
        .map(|order| order.to_string())
        .collect();
    if orders.len() != 1 {
        return Ok(OrderPayment::default());
    }
    let order: Value = serde_json::from_str(orders.iter().next().unwrap())?;
    if order["payment_status"] != "PAID" {
        return Ok(OrderPayment::default());
    }
    let lines: Vec<_> = order["order_items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|line| {
            line["target_type"] == "DIGITAL_PRODUCT"
                && line["inventory_item"]["id"] == inventory
                && line["inventory_item"]["item"]["id"] == product
        })
        .collect();
    if lines.is_empty() {
        return Ok(OrderPayment::default());
    }
    // Only USD minor units have been confirmed against the buyer/public display.
    let paid_price = (lines.len() == 1)
        .then(|| usd_minor_amount(&lines[0]["gross_price"], &order["currency_code"]))
        .flatten();
    Ok(OrderPayment {
        paid_at: date(&order["paid_at"]),
        paid_currency: paid_price.map(|_| "USD".to_owned()),
        paid_price,
    })
}

fn acquisition_date(
    detail: &InventoryDetail,
    entry: &InventoryEntry,
    payment: Option<&OrderPayment>,
) -> Option<DateTime<Utc>> {
    detail
        .imported_purchased_at
        .or(entry.created_at)
        .or_else(|| payment.and_then(|payment| payment.paid_at))
}

/// The route must be the authenticated inventory navigation's own Profile link.
/// A product seller link or a public page's arbitrary user object is not buyer proof.
pub fn protected_profile_route(html: &str) -> Result<Url> {
    ensure!(html.len() <= LIMIT, "JINXXY_METADATA_TOO_LARGE");
    let document = Html::parse_document(html);
    let base = Url::parse("https://jinxxy.com/").unwrap();
    let mut routes = BTreeSet::new();
    for anchor in document.select(&Selector::parse("nav a[href]").unwrap()) {
        if !matches!(
            anchor.text().collect::<String>().trim(),
            "Profile" | "个人资料"
        ) {
            continue;
        }
        let mut route = base
            .join(anchor.value().attr("href").unwrap_or(""))
            .map_err(|_| anyhow::anyhow!("JINXXY_IDENTITY_UNPROVEN"))?;
        ensure!(
            route.scheme() == "https"
                && route.port_or_known_default() == Some(443)
                && matches!(route.host_str(), Some("jinxxy.com" | "zh.jinxxy.com"))
                && route.username().is_empty()
                && route.password().is_none()
                && route.query().is_none()
                && route.fragment().is_none(),
            "JINXXY_IDENTITY_UNPROVEN"
        );
        let segments: Vec<_> = route.path_segments().into_iter().flatten().collect();
        ensure!(
            segments.len() == 1 && !segments[0].is_empty(),
            "JINXXY_IDENTITY_UNPROVEN"
        );
        route
            .set_host(Some("jinxxy.com"))
            .map_err(|_| anyhow::anyhow!("JINXXY_IDENTITY_UNPROVEN"))?;
        routes.insert(route.to_string());
    }
    ensure!(routes.len() == 1, "JINXXY_IDENTITY_UNPROVEN");
    Ok(Url::parse(routes.iter().next().unwrap()).unwrap())
}
pub fn profile_identity(data: &[Value], profile: &Url) -> Result<String> {
    let mut subjects = BTreeSet::new();
    for user in data.iter().filter_map(|row| row.get("user")) {
        let id = identity(&user["id"])?;
        let username = user["username"]
            .as_str()
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 128
                    && !name
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
            })
            .ok_or_else(|| anyhow::anyhow!("JINXXY_IDENTITY_UNPROVEN"))?;
        let mut expected = Url::parse("https://jinxxy.com/").unwrap();
        expected.set_path(&format!("/{username}"));
        ensure!(&expected == profile, "JINXXY_IDENTITY_UNPROVEN");
        subjects.insert(id);
    }
    ensure!(subjects.len() == 1, "JINXXY_IDENTITY_UNPROVEN");
    Ok(subjects.into_iter().next().unwrap())
}

type PreparedInventory = (InventoryDetail, Option<DateTime<Utc>>, Option<OrderPayment>);

fn validate_selected_inventory(job: &Value, products: &BTreeSet<String>) -> Result<()> {
    if job["type"] == "jinxxyUpdate" {
        ensure!(
            job["productIds"]
                .as_array()
                .is_some_and(|ids| !ids.is_empty()
                    && ids.iter().all(|id| id
                        .as_str()
                        .and_then(|id| id.strip_prefix("jinxxy:"))
                        .is_some_and(|id| products.contains(id)))),
            "JINXXY_PRODUCT_BINDING_UNPROVEN"
        );
    }
    Ok(())
}

fn stage_observed_group(
    batch: &mut crate::booth::RecordBatch,
    account: &str,
    details: &[PreparedInventory],
) -> Result<String> {
    let previous_memberships: BTreeMap<_, _> = details
        .iter()
        .map(|(detail, _, _)| {
            let entitlement = format!("jinxxy:{account}:{}", detail.inventory_id);
            Ok((entitlement.clone(), batch.get("entitlement", &entitlement)?))
        })
        .collect::<Result<_>>()?;
    let product = if details.iter().all(|(detail, _, _)| detail.inventory_complete) {
        stage_complete_group(batch, account, details)?
    } else {
        let first = details.first().ok_or_else(|| anyhow::anyhow!("JINXXY_PRODUCT_BINDING_UNPROVEN"))?;
        ensure!(details.iter().all(|(detail, _, _)| detail.product_id == first.0.product_id), "JINXXY_PRODUCT_BINDING_UNPROVEN");
        for (detail, acquired, payment) in details {
            stage_detail(batch, account, detail, *acquired, payment.as_ref())?;
        }
        format!("jinxxy:{}", first.0.product_id)
    };
    for (entitlement, previous) in previous_memberships {
        let mut value = batch
            .get("entitlement", &entitlement)?
            .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?;
        let unchanged = previous.is_some_and(|previous| {
            previous["manifestComplete"] == true && previous["fileIds"] == value["fileIds"]
        });
        value["manifestComplete"] = json!(unchanged);
        batch.stage("entitlement", &entitlement, value)?;
    }
    Ok(product)
}

fn stage_complete_group(
    batch: &mut crate::booth::RecordBatch,
    account: &str,
    details: &[PreparedInventory],
) -> Result<String> {
    let first = details
        .first()
        .ok_or_else(|| anyhow::anyhow!("JINXXY_PRODUCT_BINDING_UNPROVEN"))?;
    ensure!(
        details
            .iter()
            .all(|(detail, _, _)| detail.product_id == first.0.product_id),
        "JINXXY_PRODUCT_BINDING_UNPROVEN"
    );
    ensure!(
        details
            .iter()
            .all(|(detail, _, _)| detail.inventory_complete),
        "JINXXY_MANIFEST_INCOMPLETE"
    );
    for (detail, acquired, payment) in details {
        stage_detail(batch, account, detail, *acquired, payment.as_ref())?;
    }
    Ok(format!("jinxxy:{}", first.0.product_id))
}

pub struct InventoryEntry {
    pub inventory_id: String,
    pub product_id: Option<String>,
    pub order_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub source_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub imported_from: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub is_gift: bool,
}
pub struct InventoryPage {
    pub page: u64,
    pub pages: u64,
    pub items: Vec<InventoryEntry>,
}
fn last<'a>(data: &'a [Value], key: &str) -> Result<&'a Value> {
    data.iter()
        .rev()
        .find_map(|data| data.get(key))
        .ok_or_else(|| anyhow::anyhow!("JINXXY_SOURCE_UNSUPPORTED"))
}
fn pagination(page: &Value, expected: u64) -> Result<(u64, u64)> {
    let actual = page["page"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("JINXXY_PAGINATION_UNPROVEN"))?;
    let pages = page["page_count"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("JINXXY_PAGINATION_UNPROVEN"))?;
    ensure!(
        actual == expected && actual >= 1 && pages >= 1 && actual <= pages && pages <= 10_000,
        "JINXXY_PAGINATION_UNPROVEN"
    );
    Ok((actual, pages))
}
pub fn parse_inventory(data: &[Value], expected_page: u64) -> Result<InventoryPage> {
    let value = last(data, "inventory_items")?;
    let (page, pages) = pagination(value, expected_page)?;
    let rows = value["payload"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("JINXXY_SOURCE_UNSUPPORTED"))?;
    ensure!(
        !rows.is_empty() || (page == 1 && pages == 1),
        "JINXXY_PAGINATION_UNPROVEN"
    );
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for row in rows {
        let inventory_id = identity(&row["id"])?;
        ensure!(
            seen.insert(inventory_id.clone()),
            "JINXXY_DUPLICATE_INVENTORY"
        );
        items.push(InventoryEntry {
            inventory_id,
            product_id: optional_identity(&row["item"]["id"])?,
            order_id: optional_identity(&row["order"]["id"])?,
            title: text(&row["item"]["name"]).or_else(|| text(&row["listing"]["name"])),
            author: text(&row["item"]["user"]["username"]),
            source_url: product_url(&row["item"]),
            thumbnail_url: public_url(&row["item"]["cover"]["url"]),
            imported_from: text(&row["imported_from"]),
            created_at: date(&row["created_at"]),
            is_gift: row["isGift"] == true,
        });
    }
    Ok(InventoryPage { page, pages, items })
}
#[derive(Clone)]
pub struct FileObservation {
    pub id: String,
    pub name: String,
    pub size_bytes: Option<u64>,
    pub updated_at: Option<DateTime<Utc>>,
    pub compiled: bool,
    pub compiled_available: bool,
}
fn source_file_name(path: &str) -> Result<&str> {
    ensure!(
        !path.is_empty()
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && path.split('/').all(|segment| !segment.is_empty()
                && !matches!(segment, "." | "..")
                && !segment.contains(':')),
        "JINXXY_FILE_PATH_UNSUPPORTED"
    );
    Ok(path.rsplit('/').next().unwrap())
}
fn file_revision(file: &FileObservation) -> String {
    format!(
        "{}:{}:{}:{}",
        file.id,
        file.name,
        file.updated_at
            .map(|at| at.to_rfc3339())
            .unwrap_or_default(),
        file.size_bytes
            .map(|size| size.to_string())
            .unwrap_or_default()
    )
}
pub struct InventoryDetail {
    pub inventory_id: String,
    pub product_id: String,
    pub order_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub source_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub description: Option<String>,
    pub current_price: Option<f64>,
    pub is_gift: bool,
    pub imported_purchased_at: Option<DateTime<Utc>>,
    pub deleted: bool,
    pub files: Vec<FileObservation>,
    pub inventory_complete: bool,
}
pub fn parse_detail(
    data: &[Value],
    expected_inventory: &str,
    expected_product: Option<&str>,
) -> Result<InventoryDetail> {
    let value = last(data, "inventory_item")?;
    let inventory_id = identity(&value["id"])?;
    let product_id = identity(&value["item"]["id"])?;
    ensure!(
        inventory_id == expected_inventory && expected_product.is_none_or(|id| id == product_id),
        "JINXXY_PRODUCT_BINDING_UNPROVEN"
    );
    let item = &value["item"];
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut complete = item["files"].is_array();
    if let Some(rows) = item["files"].as_array() {
        for file in rows {
            let id = identity(&file["id"])?;
            ensure!(seen.insert(id.clone()), "JINXXY_DUPLICATE_FILE");
            let Some(name) = text(&file["file_name"]) else {
                complete = false;
                continue;
            };
            source_file_name(&name)?;
            files.push(FileObservation {
                id,
                name,
                size_bytes: file["size"].as_u64(),
                updated_at: date(&file["updated_at"]),
                compiled: file["extension"] == "3b" || file["file_meta"]["meta_type"] == "t3b_meta",
                compiled_available: compiled_available(file),
            });
        }
    }
    Ok(InventoryDetail {
        inventory_id,
        product_id,
        order_id: optional_identity(&value["order"]["id"])?,
        title: text(&item["name"]),
        author: text(&item["user"]["username"]),
        source_url: product_url(item),
        thumbnail_url: public_url(&item["cover"]["url"]),
        description: item["description"]
            .as_str()
            .filter(|raw| raw.len() <= LIMIT)
            .map(super::public_metadata::jinxxy_description)
            .filter(|description| !description.is_empty()),
        is_gift: value["isGift"] == true,
        current_price: usd_minor_amount(&item["base_price"], &item["currency_code"]),
        imported_purchased_at: date(&value["imported_purchased_at"]),
        deleted: item["is_deleted"] == true,
        inventory_complete: complete
            && !files.is_empty()
            && files
                .iter()
                .all(|file| !file.compiled || file.compiled_available),
        files,
    })
}

/// The stable first-party endpoint is reconstructed on demand, never a signed asset URL.
pub fn inventory_file_endpoint(inventory: &str, file: &str) -> Result<Url> {
    let inventory = identity(&json!(inventory))?;
    let file = identity(&json!(file))?;
    Ok(Url::parse(&format!(
        "https://api.jinxxy.com/v1/downloads/inventory/{inventory}/files/{file}"
    ))
    .unwrap())
}

fn stage_detail(
    batch: &mut crate::booth::RecordBatch<'_>,
    account: &str,
    detail: &InventoryDetail,
    acquired_at: Option<DateTime<Utc>>,
    payment: Option<&OrderPayment>,
) -> Result<String> {
    let product_id = format!("jinxxy:{}", detail.product_id);
    let observed = Utc::now().to_rfc3339();
    let mut product = batch
        .get("product", &product_id)?
        .unwrap_or_else(|| json!({"id":product_id}));
    product["provider"] = json!("jinxxy");
    product["store"] = json!("Jinxxy");
    if let Some(title) = &detail.title {
        product["title"] = json!(title);
        product["sourceTitle"] = json!(title);
    }
    if let Some(author) = &detail.author {
        product["shop"] = json!(author);
    }
    if let Some(url) = &detail.source_url {
        product["sourceUrl"] = json!(url);
    }
    if let Some(url) = &detail.thumbnail_url {
        product["previewUrl"] = json!(url);
    }
    if let Some(description) = &detail.description {
        product["sourceDescription"] = json!(description);
    }
    if let Some(price) = detail.current_price {
        product["price"] = json!(price);
        product["priceCurrency"] = json!("USD");
        product["priceBasis"] = json!("jinxxyBasePrice");
    }
    product["observedAt"] = json!(observed);
    batch.stage("product", &product_id, product)?;
    let mut file_ids = Vec::new();
    for file in &detail.files {
        let id = format!("jinxxy:{account}:{}:{}", detail.inventory_id, file.id);
        file_ids.push(id.clone());
        let revision = file_revision(file);
        batch.stage("remote_file", &id, json!({
            "id":id,"provider":"jinxxy","productId":product_id,"accountId":account,
            "sourceFileId":file.id,"inventoryId":detail.inventory_id,"name":source_file_name(&file.name)?,
            "sourceRelativePath":if file.compiled { None } else { Some(file.name.as_str()) },
            "deliveryKind":if file.compiled { "jinxxyCompiled3b" } else { "file" },
            "sizeBytes":if file.compiled { None } else { file.size_bytes },"sourceSizeBytes":file.size_bytes,"revision":revision,"revisionBasis":"jinxxyFileMetadata",
            "requiresContentVerification":file.updated_at.is_none(),"source":"library","observedAt":observed
        }))?;
    }
    let entitlement = format!("jinxxy:{account}:{}", detail.inventory_id);
    if !detail.inventory_complete {
        let previous = batch.get("entitlement", &entitlement)?;
        let mut required: BTreeSet<String> = file_ids.into_iter().collect();
        if let Some(previous) = previous {
            required.extend(
                previous["fileIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
        }
        file_ids = required.into_iter().collect();
    }
    batch.stage("entitlement", &entitlement, json!({
        "id":entitlement,"provider":"jinxxy","accountId":account,"productId":product_id,
        "inventoryId":detail.inventory_id,"source":"library","fileIds":file_ids,
        "manifestComplete":detail.inventory_complete,"currentManifest":true,
        "manifestBasis":"jinxxyBuyerInventoryFileList","unavailable":detail.deleted&&detail.files.is_empty(),"observedAt":observed
    }))?;
    let mut acquisition = batch
        .get("acquisition", &entitlement)?
        .unwrap_or_else(|| json!({"id":entitlement}));
    acquisition["productId"] = json!(product_id);
    acquisition["provider"] = json!("jinxxy");
    acquisition["accountId"] = json!(account);
    acquisition["source"] = json!(if detail.is_gift { "gift" } else { "library" });
    acquisition["sourceRecordId"] = json!(detail.inventory_id);
    if let Some(order) = &detail.order_id {
        acquisition["orderId"] = json!(order);
    }
    if let Some(acquired) = acquired_at {
        acquisition["acquiredAt"] = json!(acquired.to_rfc3339());
    }
    if let Some((price, currency)) =
        payment.and_then(|payment| payment.paid_price.zip(payment.paid_currency.as_deref()))
    {
        acquisition["paidPrice"] = json!(price);
        acquisition["paidCurrency"] = json!(currency);
        acquisition["paidPriceBasis"] = json!("productLineTotal");
    }
    acquisition["observedAt"] = json!(observed);
    batch.stage("acquisition", &entitlement, acquisition)?;
    Ok(product_id)
}

#[derive(Clone)]
pub struct JinxxyService {
    inner: Arc<Inner>,
}
struct Inner {
    catalogue: Catalogue,
    http: MetadataHttp,
    started: AtomicBool,
    enqueue_gate: std::sync::Mutex<()>,
    running: std::sync::Mutex<BTreeSet<String>>,
    execution: tokio::sync::Mutex<()>,
    downloads: std::sync::Mutex<BTreeMap<String, DownloadSession>>,
}
#[derive(Clone)]
struct DownloadSession {
    client: reqwest::Client,
    jar: Arc<reqwest::cookie::Jar>,
    origin: Url,
    identity: String,
    verified_at: Instant,
    manifests: BTreeMap<String, DownloadManifest>,
}
#[derive(Clone)]
struct DownloadManifest {
    product: String,
    observed_at: Instant,
    files: Vec<FileObservation>,
}
fn bound_account(record: &Value) -> Option<&str> {
    (record["store"] == "Jinxxy"
        && record["identityVerified"] == true
        && record["disconnected"] != true
        && record["status"] == "connected"
        && record["state"] == "connected")
        .then(|| {
            record["identityHash"]
                .as_str()
                .filter(|value| !value.is_empty())
        })
        .flatten()
}
impl DownloadSession {
    fn valid(&self, record: &Value, now: Instant) -> bool {
        bound_account(record) == Some(self.identity.as_str())
            && now.saturating_duration_since(self.verified_at) < Duration::from_secs(120)
    }
    fn snapshot(&self) -> Self {
        Self {
            client: self.client.clone(),
            jar: self.jar.clone(),
            origin: self.origin.clone(),
            identity: self.identity.clone(),
            verified_at: self.verified_at,
            manifests: BTreeMap::new(),
        }
    }
}
impl DownloadManifest {
    fn valid(&self, product: &str, file: &Value, now: Instant) -> bool {
        self.product == product
            && now.saturating_duration_since(self.observed_at) < Duration::from_secs(60)
            && self.files.iter().any(|observed| {
                file["sourceFileId"] == observed.id
                    && file["revision"].as_str() == Some(file_revision(observed).as_str())
            })
    }
}
fn fixed_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    if text.len() <= 80
        && text
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
    {
        text
    } else {
        "JINXXY_SYNC_FAILED".into()
    }
}
fn account_arg(args: &Value) -> Result<String> {
    uuid::Uuid::parse_str(args["accountId"].as_str().unwrap_or(""))
        .map(|id| id.to_string())
        .map_err(|_| anyhow::anyhow!("INVALID_ACCOUNT_ID"))
}
fn inventory_page_url(page: u64) -> Url {
    inventory_page_at(&Url::parse("https://jinxxy.com/").unwrap(), page)
}
fn inventory_page_at(origin: &Url, page: u64) -> Url {
    let mut url = origin.join("/my/inventory").unwrap();
    url.query_pairs_mut().append_pair("page", &page.to_string());
    url
}
fn native_origin(url: &Url) -> Option<Url> {
    super::http::metadata_destination(StoreProvider::Jinxxy, url)
        .then(|| Url::parse(&url.origin().ascii_serialization()).ok())
        .flatten()
}
fn english_origin(url: &Url) -> Result<Url> {
    native_origin(url)
        .filter(|origin| origin.host_str() == Some("jinxxy.com"))
        .ok_or_else(|| anyhow::anyhow!("JINXXY_SOURCE_UNSUPPORTED"))
}
impl JinxxyService {
    pub fn new(catalogue: Catalogue) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                catalogue,
                http: MetadataHttp::new(StoreProvider::Jinxxy),
                started: AtomicBool::new(false),
                enqueue_gate: std::sync::Mutex::new(()),
                running: std::sync::Mutex::new(BTreeSet::new()),
                execution: tokio::sync::Mutex::new(()),
                downloads: std::sync::Mutex::new(BTreeMap::new()),
            }),
        })
    }
    pub async fn start(&self, app: &AppHandle) -> Result<()> {
        let _work = self.inner.catalogue.begin_catalogue_work()?;
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        for mut job in self.inner.catalogue.list("job")? {
            if job["store"] == "Jinxxy"
                && matches!(job["state"].as_str(), Some("running" | "queued"))
            {
                if self.inner.catalogue.library_root().is_some()
                    && (job["originDeviceId"] != self.inner.catalogue.device_id()
                        || (job["type"] != "jinxxyLogin"
                            && !super::has_local_session(
                                &self.inner.catalogue,
                                &self.account(&account_arg(&job)?)?,
                            )))
                {
                    job["state"] = json!("needsAction");
                    job["errorCode"] = json!("RETRY_REQUIRED");
                    job["updatedAt"] = json!(Utc::now().to_rfc3339());
                    let id = job["id"].as_str().unwrap_or("").to_owned();
                    self.inner.catalogue.put("job", &id, job)?;
                    continue;
                }
                if job["type"] == "jinxxyLogin" {
                    self.finish_job(&mut job, "needsAction", Some("RETRY_REQUIRED"))?;
                    session::discard_unverified_account(&self.inner.catalogue, StoreProvider::Jinxxy, &account_arg(&job)?)?;
                    continue;
                } else {
                    job["state"] = json!("queued");
                }
                let id = job["id"].as_str().unwrap_or("").to_owned();
                self.inner.catalogue.put("job", &id, job)?;
            }
        }
        let owner = Arc::downgrade(&self.inner);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                if let Some((service, mut job, _work)) = Self::next_worker_job(&owner) {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                    let _work = _work;
                    let _execution = if job["type"] == "jinxxyLogin" { None } else { Some(service.inner.execution.lock().await) };
                    let result = service.execute(&app, &mut job).await;
                    if let Err(error) = result {
                        let code = fixed_error(&error);
                        if matches!(
                            code.as_str(),
                            "AUTH_REQUIRED"
                                | "PERMISSION_DENIED"
                                | "STORE_IDENTITY_CHANGED"
                                | "JINXXY_IDENTITY_UNPROVEN"
                                | "JINXXY_PRODUCT_BINDING_UNPROVEN"
                        ) {
                            if let Ok(account) = account_arg(&job) {
                                service.clear_downloads(&account, None);
                            }
                        }
                        let state = if code == "CANCELLED" {
                            "cancelled"
                        } else if matches!(
                            code.as_str(),
                            "AUTH_REQUIRED"
                                | "JINXXY_IDENTITY_UNPROVEN"
                                | "STORE_IDENTITY_CHANGED"
                                | "JINXXY_MANIFEST_INCOMPLETE"
                                | "JINXXY_PRODUCT_BINDING_UNPROVEN"
                        ) {
                            "needsAction"
                        } else {
                            "failed"
                        };
                        // Keep the terminal result pending locally until its transaction
                        // is durable; a database error must not strand a running job.
                        while let Err(error) = service.save_job(&app, &mut job, state, Some(&code))
                        {
                            if fixed_error(&error) == "JOB_UNAVAILABLE" {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                    if job["type"] == "jinxxyLogin" {
                        if let Ok(account) = account_arg(&job) {
                            if let Err(error) = session::finish_login(
                                &app,
                                StoreProvider::Jinxxy,
                                &account,
                                job["state"] == "completed",
                            )
                            .await
                            {
                                eprintln!(
                                    "Jinxxy login window close failed: {}",
                                    fixed_error(&error)
                                );
                            }
                            if let Err(error) =
                                session::discard_unverified_account(&service.inner.catalogue, StoreProvider::Jinxxy, &account)
                            {
                                eprintln!("Jinxxy login cleanup failed: {}", fixed_error(&error));
                            }
                            let _ = app.emit(
                                "nna:changed",
                                if job["state"] == "completed" {
                                    json!({"scope":"booth","sessionChanged":true})
                                } else {
                                    json!({"scope":"booth"})
                                },
                            );
                        }
                    }
                    if let Ok(mut running) = service.inner.running.lock() {
                        running.remove(&Self::worker_key(&job));
                    }
                    });
                }
                // Idle workers must not retain an old Setup library after its
                // services are replaced. Active jobs keep the service and permit
                // above until their terminal result and login cleanup finish.
                if owner.strong_count() == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
        Ok(())
    }
    fn next_worker_job(
        owner: &std::sync::Weak<Inner>,
    ) -> Option<(Self, Value, crate::catalogue::CatalogueWorkPermit)> {
        let service = Self {
            inner: owner.upgrade()?,
        };
        // Acquire before selection so switching cannot leave a previously
        // selected job running against a catalogue that has been replaced.
        let work = service.inner.catalogue.begin_catalogue_work().ok()?;
        let mut running = service.inner.running.lock().ok()?;
        let job = service
            .inner
            .catalogue
            .list("job")
            .ok()?
            .into_iter()
            .filter(|job| job["store"] == "Jinxxy" && job["state"] == "queued" && !running.contains(&Self::worker_key(job)))
            .min_by_key(|job| u8::from(job["type"] != "jinxxyLogin"))?;
        running.insert(Self::worker_key(&job));
        drop(running);
        Some((service, job, work))
    }
    fn worker_key(job: &Value) -> String {
        job["accountId"].as_str().map(|account| format!("account:{account}"))
            .unwrap_or_else(|| format!("job:{}", job["id"].as_str().unwrap_or("")))
    }
    fn account(&self, id: &str) -> Result<Value> {
        let account = self
            .inner
            .catalogue
            .get("account", id)?
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
        ensure!(account["store"] == "Jinxxy", "STORE_ACCOUNT_MISMATCH");
        Ok(account)
    }
    fn clear_downloads(&self, account: &str, inventory: Option<&str>) {
        if let Ok(mut cache) = self.inner.downloads.lock() {
            if let Some(inventory) = inventory {
                if let Some(session) = cache.get_mut(account) {
                    session.manifests.remove(inventory);
                }
            } else {
                cache.remove(account);
            }
        }
    }
    async fn download_session(&self, app: &AppHandle, account: &str) -> Result<DownloadSession> {
        let record = self.account(account)?;
        let identity = bound_account(&record)
            .ok_or_else(|| anyhow::anyhow!("AUTH_REQUIRED"))?
            .to_owned();
        {
            let mut cache = self
                .inner
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            if let Some(session) = cache
                .get(account)
                .filter(|session| session.valid(&record, Instant::now()))
            {
                return Ok(session.snapshot());
            }
            cache.remove(account);
        }
        let (client, _, jar, origin) = self.verify(app, account, false, None).await?;
        ensure!(
            bound_account(&self.account(account)?) == Some(identity.as_str()),
            "STORE_IDENTITY_CHANGED"
        );
        let session = DownloadSession {
            client,
            jar,
            origin,
            identity,
            verified_at: Instant::now(),
            manifests: BTreeMap::new(),
        };
        let mut cache = self
            .inner
            .downloads
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        if cache.len() >= 16 {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, session)| session.verified_at)
                .map(|(id, _)| id.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(account.to_owned(), session.clone());
        Ok(session)
    }
    fn coverage_record(
        &self,
        job: &Value,
        state: &str,
        error: Option<&str>,
        mut value: Value,
    ) -> Value {
        // Only a recorded completion timestamp proves a prior successful run.
        if value["lastSuccessfulAt"].is_null()
            && value["status"] == "complete"
            && value["completedAt"].is_string()
        {
            value["lastSuccessfulAt"] = value["completedAt"].clone();
        }
        let status = match state {
            "running" => "running",
            "complete" | "completed" => "complete",
            "cancelled" => "cancelled",
            _ if error == Some("VERIFICATION_PAGE_LIMIT") => "incomplete",
            _ if error == Some("AUTH_REQUIRED") => "auth-required",
            _ if error.is_some_and(|code| code.ends_with("_UNSUPPORTED")) => "unsupported",
            _ => "failed",
        };
        if let Some(fields) = value.as_object_mut() {
            fields.remove("state");
            fields.remove("complete");
        }
        let now = Utc::now().to_rfc3339();
        value["store"] = json!("Jinxxy");
        value["status"] = json!(status);
        value["coverageComplete"] = json!(status == "complete");
        value["errorCode"] = json!(error);
        value["updatedAt"] = json!(now);
        if status != "running" {
            value["completedAt"] = json!(now);
        }
        if status == "complete" {
            value["lastSuccessfulAt"] = value["completedAt"].clone();
            value["productCount"] = json!(job["completedProducts"].as_u64().unwrap_or(0));
        }
        value
    }
    fn coverage(&self, job: &Value, state: &str, error: Option<&str>) -> Result<()> {
        if job["type"] != "jinxxySync" {
            return Ok(());
        }
        let account = account_arg(job)?;
        let id = format!("jinxxy:{account}:inventory");
        let value = self
            .inner
            .catalogue
            .get("source_coverage", &id)?
            .unwrap_or_else(
                || json!({"id":id,"provider":"jinxxy","accountId":account,"source":"library"}),
            );
        self.inner.catalogue.put(
            "source_coverage",
            &id,
            self.coverage_record(job, state, error, value),
        )
    }
    fn finish_job(&self, job: &mut Value, state: &str, error: Option<&str>) -> Result<()> {
        if error == Some("AUTH_REQUIRED") && self.inner.catalogue.library_root().is_some() {
            session::forget(
                &self.inner.catalogue.device_dir(),
                StoreProvider::Jinxxy,
                &account_arg(job)?,
            )?;
        }
        let id = job["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("INVALID_JOB"))?
            .to_owned();
        let account_id = account_arg(job)?;
        for _ in 0..3 {
            let mut batch = crate::booth::RecordBatch::new(&self.inner.catalogue);
            let latest = batch
                .get("job", &id)?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            let cancelled = latest["cancelRequested"] == true || latest["state"] == "cancelled";
            let state = if cancelled { "cancelled" } else { state };
            let error = if cancelled { Some("CANCELLED") } else { error };
            let mut terminal = job.clone();
            terminal["supportLastStage"] = json!(crate::support::operation_stage(job["progress"]["stage"].as_str().unwrap_or("")));
            let now = Utc::now().to_rfc3339();
            terminal["state"] = json!(state);
            terminal["errorCode"] = json!(error);
            terminal["updatedAt"] = json!(now);
            terminal["completedAt"] = json!(now);
            if !terminal["progress"].is_object() {
                terminal["progress"] = json!({});
            }
            terminal["progress"]["stage"] = json!(state);
            if cancelled {
                terminal["cancelRequested"] = json!(true);
            }
            if job["type"] == "jinxxySync" {
                let coverage_id = format!("jinxxy:{account_id}:inventory");
                let previous = batch.get("source_coverage", &coverage_id)?.unwrap_or_else(|| json!({"id":coverage_id,"provider":"jinxxy","accountId":account_id,"source":"library"}));
                let coverage = self.coverage_record(&terminal, state, error, previous);
                batch.stage("source_coverage", &coverage_id, coverage)?;
            }
            if matches!(job["type"].as_str(), Some("jinxxySync" | "jinxxyUpdate")) {
                if let Some(mut account) = batch.get("account", &account_id)? {
                    account["syncState"] = json!(if state == "completed" {
                        "complete"
                    } else {
                        "incomplete"
                    });
                    account["lastSyncErrorCode"] = json!(error);
                    if error == Some("AUTH_REQUIRED")
                        && self.inner.catalogue.library_root().is_none()
                    {
                        account["state"] = json!("expired");
                        account["status"] = json!("expired");
                    }
                    if state == "completed" {
                        account["lastSync"] = json!(now);
                        account["lastSyncAt"] = json!(now);
                    }
                    batch.stage("account", &account_id, account)?;
                }
            }
            batch.stage("job", &id, terminal.clone())?;
            if batch.try_commit()?.is_some() {
                *job = terminal;
                if matches!(job["type"].as_str(), Some("jinxxySync" | "jinxxyUpdate")) {
                    self.inner.catalogue.refresh_avatar_defaults()?;
                }
                return Ok(());
            }
        }
        anyhow::bail!("CONCURRENT_UPDATE")
    }
    fn save_job(
        &self,
        app: &AppHandle,
        job: &mut Value,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        if matches!(state, "completed" | "cancelled" | "failed" | "needsAction") {
            self.finish_job(job, state, error)?;
            let scope = if matches!(job["type"].as_str(), Some("jinxxySync" | "jinxxyUpdate")) {
                "catalogue"
            } else {
                "jobs"
            };
            let _ = app.emit("nna:changed", json!({"scope":scope}));
            return Ok(());
        }
        job["state"] = json!(state);
        job["errorCode"] = json!(error);
        job["updatedAt"] = json!(Utc::now().to_rfc3339());
        let id = job["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("INVALID_JOB"))?
            .to_owned();
        // Preserve cancellation even when it races this progress transaction.
        for attempt in 0..3 {
            let mut batch = crate::booth::RecordBatch::new(&self.inner.catalogue);
            if state == "running" {
                let latest = batch
                    .get("job", &id)?
                    .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                if job["cancelRequested"] != true
                    && (latest["cancelRequested"] == true || latest["state"] == "cancelled")
                {
                    anyhow::bail!("CANCELLED");
                }
            }
            batch.stage("job", &id, job.clone())?;
            if batch.try_commit()?.is_some() {
                break;
            }
            ensure!(attempt < 2, "CONCURRENT_UPDATE");
        }
        let _ = app.emit("nna:changed", json!({"scope":"jobs"}));
        Ok(())
    }
    fn check_cancelled(&self, id: &str) -> Result<()> {
        ensure!(
            !self
                .inner
                .catalogue
                .get("job", id)?
                .is_none_or(|job| job["cancelRequested"] == true),
            "CANCELLED"
        );
        Ok(())
    }
    async fn client(
        &self,
        app: &AppHandle,
        account: &str,
        login: bool,
    ) -> Result<(
        reqwest::Client,
        Arc<reqwest::cookie::Jar>,
        Url,
        tauri::WebviewWindow,
    )> {
        self.account(account)?;
        let window = if login {
            session::existing_window(app, StoreProvider::Jinxxy, account)?
                .ok_or_else(|| anyhow::anyhow!("CANCELLED"))?
        } else {
            session::window(
                app,
                &self.inner.catalogue.device_dir(),
                StoreProvider::Jinxxy,
                account,
                inventory_page_url(1),
                false,
            )
            .await?
        };
        let jar = session::jar(window.clone(), StoreProvider::Jinxxy).await?;
        let origin = window
            .url()
            .ok()
            .and_then(|url| native_origin(&url))
            .unwrap_or_else(|| Url::parse("https://jinxxy.com/").unwrap());
        Ok((
            build_client(StoreProvider::Jinxxy, jar.clone())?,
            jar,
            origin,
            window,
        ))
    }
    async fn page(&self, client: &reqwest::Client, url: Url) -> Result<Vec<Value>> {
        let (status, body) = self.inner.http.get(client, url).await?;
        ensure!(status.is_success(), "JINXXY_SOURCE_UNAVAILABLE");
        buyer_data(&body)
    }
    async fn verify(
        &self,
        app: &AppHandle,
        account: &str,
        allow_connect: bool,
        login_job: Option<&Value>,
    ) -> Result<(reqwest::Client, Vec<Value>, Arc<reqwest::cookie::Jar>, Url)> {
        let mut verified = None;
        for attempt in 0..2 {
            let result = async {
                let (client, jar, origin, window) =
                    self.client(app, account, login_job.is_some()).await?;
                let (status, html, final_url) = self
                    .inner
                    .http
                    .get_with_url(&client, inventory_page_at(&origin, 1))
                    .await?;
                ensure!(status.is_success(), "JINXXY_SOURCE_UNAVAILABLE");
                let origin =
                    native_origin(&final_url).ok_or_else(|| anyhow::anyhow!("UNSAFE_REDIRECT"))?;
                Ok((client, jar, origin, html, window))
            }
            .await;
            match result {
                Ok(result) => {
                    verified = Some(result);
                    break;
                }
                Err(error) if attempt == 0 && fixed_error(&error) == "AUTH_REQUIRED" => {
                    // A new hidden native profile can finish loading its cookies after creation.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(error) => return Err(error),
            }
        }
        let (mut client, mut jar, mut origin, mut html, window) =
            verified.ok_or_else(|| anyhow::anyhow!("AUTH_REQUIRED"))?;
        let mut data = buyer_data(&html)?;
        parse_inventory(&data, 1)?;
        if origin.host_str() != Some("jinxxy.com") {
            // Normalize only after protected buyer data confirms login. The native
            // site preference changes; auth-cookie domains and buyer binding do not.
            self.clear_downloads(account, None);
            session::jinxxy_english(window.clone()).await?;
            jar = session::jar(window.clone(), StoreProvider::Jinxxy).await?;
            client = build_client(StoreProvider::Jinxxy, jar.clone())?;
            let (status, english_html, final_url) = self
                .inner
                .http
                .get_with_url(&client, inventory_page_url(1))
                .await?;
            ensure!(status.is_success(), "JINXXY_SOURCE_UNAVAILABLE");
            origin = english_origin(&final_url)?;
            html = english_html;
            data = buyer_data(&html)?;
            parse_inventory(&data, 1)?;
        }
        let profile = protected_profile_route(&html)?;
        let profile_data = self.page(&client, profile.clone()).await?;
        let subject = profile_identity(&profile_data, &profile)?;
        let subject = hex::encode(Sha256::digest(subject.as_bytes()));
        let mut batch = crate::booth::RecordBatch::new(&self.inner.catalogue);
        let mut record = batch
            .get("account", account)?
            .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UNAVAILABLE"))?;
        let previous = record.clone();
        if let Some(job) = login_job {
            ensure!(
                session::existing_window(app, StoreProvider::Jinxxy, account)?.is_some(),
                "CANCELLED"
            );
            let id = job["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("INVALID_JOB"))?;
            ensure!(
                batch
                    .get("job", id)?
                    .is_some_and(|job| job["cancelRequested"] != true
                        && matches!(job["state"].as_str(), Some("queued" | "running"))),
                "CANCELLED"
            );
        }
        if !allow_connect {
            ensure!(
                bound_account(&record) == Some(subject.as_str()),
                "STORE_IDENTITY_CHANGED"
            );
        }
        ensure!(
            record["identityHash"]
                .as_str()
                .is_none_or(|previous| previous == subject),
            "STORE_IDENTITY_CHANGED"
        );
        let unverified = record["identityVerified"] != true
            && record["authenticatedAt"].as_str().is_none_or(str::is_empty)
            && record["identityHash"].as_str().is_none_or(str::is_empty);
        if login_job.is_some() {
            session::remember(window.clone(), &self.inner.catalogue.device_dir(), StoreProvider::Jinxxy, account).await?;
            if session::existing_window(app, StoreProvider::Jinxxy, account)?.is_none() {
                if unverified { session::forget(&self.inner.catalogue.device_dir(), StoreProvider::Jinxxy, account)?; }
                anyhow::bail!("CANCELLED");
            }
        }
        record["identityHash"] = json!(subject);
        record["identityVerified"] = json!(true);
        record["disconnected"] = json!(false);
        record["status"] = json!("connected");
        record["state"] = json!("connected");
        if login_job.is_some() {
            record["authenticatedAt"] = json!(Utc::now().to_rfc3339());
        }
        if record != previous {
            batch.stage("account", account, record)?;
            match batch.try_commit() {
                Ok(Some(_)) => {}
                result => {
                    if login_job.is_some() && unverified { session::forget(&self.inner.catalogue.device_dir(), StoreProvider::Jinxxy, account)?; }
                    result?;
                    anyhow::bail!("CONCURRENT_UPDATE");
                }
            }
            let _ = app.emit("nna:changed", json!({"scope":"booth"}));
        }
        Ok((client, data, jar, origin))
    }
    pub async fn command(&self, app: &AppHandle, action: &str, args: Value) -> Result<Value> {
        let _work = self.inner.catalogue.begin_catalogue_work()?;
        match action {
            "login" | "account_login" => {
                let account = account_arg(&args)?;
                self.account(&account)?;
                let active = self.inner.catalogue.list("job")?.into_iter().find(|job| {
                    job["accountId"] == account
                        && matches!(job["state"].as_str(), Some("queued" | "running"))
                });
                ensure!(
                    active
                        .as_ref()
                        .is_none_or(|job| job["type"] == "jinxxyLogin"),
                    "ACCOUNT_BUSY"
                );
                self.clear_downloads(&account, None);
                let opening = session::window(
                    app,
                    &self.inner.catalogue.device_dir(),
                    StoreProvider::Jinxxy,
                    &account,
                    Url::parse("https://jinxxy.com/my/inventory").unwrap(),
                    true,
                )
                .await;
                if let Err(error) = opening {
                    session::discard_unverified_account(&self.inner.catalogue, StoreProvider::Jinxxy, &account)?;
                    return Err(error);
                }
                self.enqueue(app, &account, "jinxxyLogin", None)
            }
            "sync" => {
                ensure!(
                    matches!(args["source"].as_str(), None | Some("all" | "library")),
                    "INVALID_SOURCE"
                );
                ensure!(
                    matches!(
                        args["mode"].as_str(),
                        None | Some("incremental" | "full" | "forced")
                    ),
                    "INVALID_SYNC_MODE"
                );
                self.enqueue(app, &account_arg(&args)?, "jinxxySync", None)
            }
            "cancel_sync" | "retry_sync" => {
                let id = args["jobId"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("INVALID_JOB"))?;
                let mut job = self
                    .inner
                    .catalogue
                    .get("job", id)?
                    .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                ensure!(job["store"] == "Jinxxy", "STORE_ACCOUNT_MISMATCH");
                if action == "cancel_sync" {
                    let mut cancelled_queued = false;
                    for attempt in 0..4 {
                        let mut batch = crate::booth::RecordBatch::new(&self.inner.catalogue);
                        let mut latest = batch.get("job", id)?.ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
                        if !matches!(latest["state"].as_str(), Some("queued" | "running")) { break; }
                        let queued = latest["state"] == "queued";
                        latest["cancelRequested"] = json!(true);
                        latest["updatedAt"] = json!(Utc::now().to_rfc3339());
                        if queued { latest["state"] = json!("cancelled"); }
                        batch.stage("job", id, latest.clone())?;
                        if batch.try_commit()?.is_some() { job = latest; cancelled_queued = queued; break; }
                        ensure!(attempt < 3, "CONCURRENT_UPDATE");
                    }
                    if cancelled_queued { self.save_job(app, &mut job, "cancelled", Some("CANCELLED"))?; }
                    let _ = app.emit("nna:changed", json!({"scope":"jobs"}));
                    if cancelled_queued && job["type"] == "jinxxyLogin" {
                        let account = account_arg(&job)?;
                        session::finish_login(app, StoreProvider::Jinxxy, &account, false).await?;
                        session::discard_unverified_account(&self.inner.catalogue, StoreProvider::Jinxxy, &account)?;
                        let _ = app.emit("nna:changed", json!({"scope":"booth"}));
                    }
                } else {
                    ensure!(job["state"] != "running", "JOB_ACTIVE");
                    if job["type"] == "jinxxyLogin" {
                        let account = account_arg(&job)?;
                        self.clear_downloads(&account, None);
                        session::window(
                            app,
                            &self.inner.catalogue.device_dir(),
                            StoreProvider::Jinxxy,
                            &account,
                            inventory_page_url(1),
                            true,
                        )
                        .await?;
                    }
                    crate::support::reset_attempt(&mut job);
                    job["cancelRequested"] = json!(false);
                    job["originDeviceId"] = json!(self.inner.catalogue.device_id());
                    self.save_job(app, &mut job, "queued", None)?;
                }
                Ok(json!({"jobId":id}))
            }
            "account_remove" | "account_logout" => {
                let account = account_arg(&args)?;
                self.account(&account)?;
                let remove_products = if action == "account_remove" {
                    Some(
                        args["removeProducts"]
                            .as_bool()
                            .ok_or_else(|| anyhow::anyhow!("INVALID_ACCOUNT_ACTION"))?,
                    )
                } else {
                    None
                };
                ensure!(
                    !self
                        .inner
                        .catalogue
                        .list("job")?
                        .iter()
                        .any(|job| job["accountId"] == account
                            && matches!(job["state"].as_str(), Some("queued" | "running"))),
                    "ACCOUNT_BUSY"
                );
                self.clear_downloads(&account, None);
                let window = session::window(
                    app,
                    &self.inner.catalogue.device_dir(),
                    StoreProvider::Jinxxy,
                    &account,
                    Url::parse("https://jinxxy.com/my/inventory").unwrap(),
                    false,
                )
                .await?;
                session::forget(
                    &self.inner.catalogue.device_dir(),
                    StoreProvider::Jinxxy,
                    &account,
                )?;
                session::disconnect(window).await?;
                if let Some(remove_products) = remove_products {
                    let result = self
                        .inner
                        .catalogue
                        .remove_account(&account, remove_products)?;
                    let _ = app.emit("nna:changed", json!({"scope":"catalogue"}));
                    return Ok(result);
                }
                let mut record = self.account(&account)?;
                if self.inner.catalogue.library_root().is_none() {
                    record["status"] = json!("disconnected");
                    record["state"] = json!("disconnected");
                    record["identityVerified"] = json!(false);
                    record["disconnected"] = json!(true);
                    self.inner.catalogue.put("account", &account, record)?;
                }
                self.clear_downloads(&account, None);
                let _ = app.emit("nna:changed", json!({"scope":"booth"}));
                Ok(json!({"accountId":account}))
            }
            "update_products" => {
                let ids = args["productIds"]
                    .as_array()
                    .or_else(|| args["ids"].as_array())
                    .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))?;
                ensure!(!ids.is_empty(), "INVALID_PRODUCT_IDS");
                let selected: BTreeSet<_> = ids
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .filter(|id| id.starts_with("jinxxy:"))
                            .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_IDS"))
                    })
                    .collect::<Result<_>>()?;
                let mut accounts: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                let mut matched = BTreeSet::new();
                let scoped_account = args
                    .get("accountId")
                    .map(|_| account_arg(&args))
                    .transpose()?;
                if let Some(account) = scoped_account.as_ref() {
                    ensure!(
                        bound_account(&self.account(account)?).is_some(),
                        "ACCOUNT_UNAVAILABLE"
                    );
                }
                for row in self.inner.catalogue.list("entitlement")? {
                    let (Some(product), Some(account)) =
                        (row["productId"].as_str(), row["accountId"].as_str())
                    else {
                        continue;
                    };
                    if row["provider"] == "jinxxy" && selected.contains(product) {
                        if scoped_account
                            .as_ref()
                            .is_some_and(|scope| scope != account)
                            || !self.inner.catalogue.get("account", account)?.is_some_and(
                                |record| {
                                    bound_account(&record).is_some()
                                        && super::has_local_session(&self.inner.catalogue, &record)
                                },
                            )
                        {
                            continue;
                        }
                        matched.insert(product.to_owned());
                        accounts
                            .entry(account.to_owned())
                            .or_default()
                            .insert(product.to_owned());
                    }
                }
                ensure!(matched.len() == selected.len(), "ACCOUNT_UNAVAILABLE");
                let mut jobs = Vec::new();
                for (account, products) in accounts {
                    jobs.push(self.enqueue(
                        app,
                        &account,
                        "jinxxyUpdate",
                        Some(products.into_iter().map(Value::String).collect()),
                    )?);
                }
                let job_ids: Vec<_> = jobs.iter().map(|job| job["jobId"].clone()).collect();
                Ok(json!({"jobId":job_ids.first(),"jobIds":job_ids}))
            }
            _ => anyhow::bail!("UNKNOWN_STORE_ACTION"),
        }
    }
    fn enqueue(
        &self,
        app: &AppHandle,
        account: &str,
        kind: &str,
        products: Option<Vec<Value>>,
    ) -> Result<Value> {
        let _gate = self
            .inner
            .enqueue_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
        self.account(account)?;
        if let Some(job) = self.inner.catalogue.list("job")?.into_iter().find(|job| {
            job["accountId"] == account
                && job["store"] == "Jinxxy"
                && matches!(job["state"].as_str(), Some("queued" | "running"))
        }) {
            ensure!(
                job["type"] == kind && job["productIds"] == json!(products),
                "ACCOUNT_BUSY"
            );
            return Ok(json!({"jobId":job["id"]}));
        }
        let id = format!("jinxxy:{}", uuid::Uuid::new_v4());
        let mut job = json!({"id":id,"type":kind,"store":"Jinxxy","accountId":account,"productIds":products,"createdAt":Utc::now().to_rfc3339(),"originDeviceId":self.inner.catalogue.device_id()});
        let record = self.account(account)?;
        job["accountName"] = record["name"].clone();
        self.save_job(app, &mut job, "queued", None)?;
        Ok(json!({"jobId":id}))
    }
    async fn execute(&self, app: &AppHandle, job: &mut Value) -> Result<()> {
        let id = job["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("INVALID_JOB"))?
            .to_owned();
        let account = account_arg(job)?;
        self.check_cancelled(&id)?;
        job["progress"] = json!({"stage":"collecting","pages":0,"sources":1,"products":0,"metadataCompleted":0,"metadataTotal":0});
        self.coverage(job, "running", None)?;
        self.save_job(app, job, "running", None)?;
        if job["type"] == "jinxxyLogin" {
            for attempt in 0..180 {
                self.check_cancelled(&id)?;
                match self.verify(app, &account, true, Some(job)).await {
                    Ok(_) => {
                        self.save_job(app, job, "completed", None)?;
                        return Ok(());
                    }
                    Err(error)
                        if matches!(fixed_error(&error).as_str(), "AUTH_REQUIRED")
                            && attempt < 179 =>
                    {
                        self.save_job(app, job, "running", Some("AUTH_REQUIRED"))?;
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    }
                    Err(error) => return Err(error),
                }
            }
            anyhow::bail!("AUTH_REQUIRED");
        }
        let (client, first, _, origin) = self.verify(app, &account, true, None).await?;
        let checkpoint_id = format!("jinxxy:{account}:inventory");
        let mut page = parse_inventory(&first, 1)?;
        let pages = page.pages;
        let page_limit = crate::verification::source_page_limit().map(|v| v as u64);
        let observed_pages = page_limit.map_or(pages, |limit| pages.min(limit));
        if let Some(limit) = page_limit {
            job["verification"] =
                json!({"pageLimit":limit,"sourcePages":{"inventory":0},"capped":false});
        }
        let first_signature = hex::encode(Sha256::digest(
            last(&first, "inventory_items")?.to_string().as_bytes(),
        ));
        let mut observed = BTreeSet::new();
        let mut product_keys = BTreeSet::new();
        let mut completed_products = BTreeSet::new();
        let mut incomplete_products = BTreeSet::new();
        let mut finished: BTreeSet<String> = self
            .inner
            .catalogue
            .get("checkpoint", &checkpoint_id)?
            .filter(|checkpoint| {
                checkpoint["jobId"] == id && checkpoint["scopeSignature"] == first_signature
            })
            .map(|checkpoint| {
                checkpoint["inventoryIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut count = 0usize;
        let mut metadata_completed = 0usize;
        let mut public_presentations = BTreeMap::<String, Result<Value, String>>::new();
        loop {
            job["progress"]["stage"] = json!("collecting");
            job["progress"]["pages"] = json!(page.page);
            if page_limit.is_some() {
                job["verification"]["sourcePages"]["inventory"] = json!(page.page);
            }
            job["progress"]["metadataTotal"] = json!(if job["type"] == "jinxxyUpdate" {
                metadata_completed
            } else {
                observed.len() + page.items.len()
            });
            self.save_job(app, job, "running", None)?;
            for entry in page.items {
                ensure!(
                    observed.insert(entry.inventory_id.clone()),
                    "JINXXY_DUPLICATE_INVENTORY"
                );
                self.check_cancelled(&id)?;
                let ready_detail = if entry.product_id.is_none() {
                    let data = self
                        .page(
                            &client,
                            origin.join(&format!("/my/inventory/{}", entry.inventory_id))?,
                        )
                        .await?;
                    Some(parse_detail(&data, &entry.inventory_id, None)?)
                } else {
                    None
                };
                let product_key = entry
                    .product_id
                    .clone()
                    .or_else(|| {
                        ready_detail
                            .as_ref()
                            .map(|detail| detail.product_id.clone())
                    })
                    .ok_or_else(|| anyhow::anyhow!("JINXXY_PRODUCT_BINDING_UNPROVEN"))?;
                if job["type"] == "jinxxyUpdate"
                    && !job["productIds"].as_array().is_some_and(|ids| {
                        ids.iter()
                            .any(|id| id.as_str() == Some(format!("jinxxy:{product_key}").as_str()))
                    })
                {
                    continue;
                }
                product_keys.insert(product_key.clone());
                if job["type"] == "jinxxyUpdate" {
                    // Only matching selected grants contribute to this denominator.
                    job["progress"]["metadataTotal"] = json!(metadata_completed + 1);
                }
                if finished.contains(&entry.inventory_id) {
                    completed_products.insert(product_key);
                    count = completed_products.len();
                    metadata_completed += 1;
                    continue;
                }
                let group = vec![(entry, ready_detail)];
                let mut details = Vec::new();
                let mut receipt_error = None;
                for (entry, ready_detail) in group {
                    self.check_cancelled(&id)?;
                    let detail = if let Some(detail) = ready_detail {
                        detail
                    } else {
                        let data = self
                            .page(
                                &client,
                                origin.join(&format!("/my/inventory/{}", entry.inventory_id))?,
                            )
                            .await?;
                        parse_detail(&data, &entry.inventory_id, entry.product_id.as_deref())?
                    };
                    ensure!(
                        detail
                            .order_id
                            .as_ref()
                            .zip(entry.order_id.as_ref())
                            .is_none_or(|(a, b)| a == b),
                        "JINXXY_PRODUCT_BINDING_UNPROVEN"
                    );
                    // Incomplete grants publish known facts additively without
                    // certifying membership or blocking the remaining inventory.
                    if !detail.inventory_complete {
                        incomplete_products.insert(detail.product_id.clone());
                    }
                    let payment =
                        if let Some(order) = detail.order_id.as_ref().or(entry.order_id.as_ref()) {
                            let result = async {
                                let (status, html) = self
                                    .inner
                                    .http
                                    .get(&client, origin.join(&format!("/my/orders/{order}"))?)
                                    .await?;
                                ensure!(status.is_success(), "JINXXY_RECEIPT_UNAVAILABLE");
                                parse_order_payment(
                                    &html,
                                    order,
                                    &detail.inventory_id,
                                    &detail.product_id,
                                )
                            }
                            .await;
                            match result {
                                Ok(payment) => Some(payment),
                                Err(error) => {
                                    receipt_error = Some(fixed_error(&error));
                                    None
                                }
                            }
                        } else {
                            None
                        };
                    let acquired = acquisition_date(&detail, &entry, payment.as_ref());
                    details.push((detail, acquired, payment));
                    metadata_completed += 1;
                    job["progress"]["stage"] = json!("enriching");
                    job["progress"]["metadataCompleted"] = json!(metadata_completed);
                    self.save_job(app, job, "running", None)?;
                }
                job["progress"]["stage"] = json!("publishing");
                let detail = &details[0].0;
                if !public_presentations.contains_key(&detail.product_id) {
                    if let Some(source) = detail
                        .source_url
                        .as_deref()
                        .and_then(|url| Url::parse(url).ok())
                    {
                        self.check_cancelled(&id)?;
                        let presentation = super::public_metadata::fetch_jinxxy_presentation(
                            &self.inner.http,
                            source,
                            &detail.product_id,
                        )
                        .await
                        .map_err(|error| fixed_error(&error));
                        public_presentations.insert(detail.product_id.clone(), presentation);
                    }
                }
                self.save_job(app, job, "running", None)?;
                for attempt in 0..3 {
                    self.check_cancelled(&id)?;
                    let mut batch = crate::booth::RecordBatch::new(&self.inner.catalogue);
                    // This expected job row also closes cancellation during commit.
                    let current_job = batch.get("job", &id)?;
                    ensure!(
                        current_job.is_some_and(
                            |j| j["cancelRequested"] != true && j["state"] != "cancelled"
                        ),
                        "CANCELLED"
                    );
                    let product = stage_observed_group(&mut batch, &account, &details)?;
                    let mut source = batch
                        .get("product", &product)?
                        .ok_or_else(|| anyhow::anyhow!("PRODUCT_UNAVAILABLE"))?;
                    if let Some(presentation) = public_presentations.get(&details[0].0.product_id) {
                        match presentation {
                            Ok(presentation) => {
                                source
                                    .as_object_mut()
                                    .ok_or_else(|| anyhow::anyhow!("PRODUCT_INVALID"))?
                                    .extend(
                                        presentation
                                            .as_object()
                                            .ok_or_else(|| {
                                                anyhow::anyhow!("JINXXY_SOURCE_UNSUPPORTED")
                                            })?
                                            .clone(),
                                    );
                                source["publicPresentationErrorCode"] = Value::Null;
                            }
                            Err(error) => source["publicPresentationErrorCode"] = json!(error),
                        }
                    }
                    source["receiptRefreshState"] = json!(if receipt_error.is_some() {
                        "unavailable"
                    } else if details.iter().any(|(_, _, payment)| payment
                        .as_ref()
                        .is_some_and(|p| p.paid_price.is_some() || p.paid_at.is_some()))
                    {
                        "observed"
                    } else {
                        "unknown"
                    });
                    source["receiptRefreshErrorCode"] = json!(receipt_error);
                    batch.stage("product", &product, source)?;
                    let next_finished: BTreeSet<_> = finished
                        .iter()
                        .cloned()
                        .chain(
                            details
                                .iter()
                                .filter(|(detail, _, _)| detail.inventory_complete)
                                .map(|(detail, _, _)| detail.inventory_id.clone()),
                        )
                        .collect();
                    // Prepared facts contain no raw pages or download capabilities.
                    let mut checkpoint = json!({"id":checkpoint_id,"provider":"jinxxy","accountId":account,"jobId":id,"scopeSignature":first_signature,"inventoryIds":next_finished,"updatedAt":Utc::now().to_rfc3339()});
                    if page_limit.is_some() {
                        // Keep observed pre-commit facts only in the bounded debug
                        // verifier. Production checkpoints retain no audit copy.
                        let previous = self.inner.catalogue.get("checkpoint", &checkpoint_id)?;
                        let mut expected = BTreeMap::new();
                        if let Some(previous) = previous.filter(|p| p["jobId"] == id) {
                            for record in previous["verificationRecords"]
                                .as_array()
                                .into_iter()
                                .flatten()
                            {
                                if let (Some(kind), Some(key)) =
                                    (record[0].as_str(), record[1].as_str())
                                {
                                    expected.insert(
                                        (kind.to_owned(), key.to_owned()),
                                        record[2].clone(),
                                    );
                                }
                            }
                        }
                        for (kind, key, value) in batch.staged_records() {
                            if matches!(
                                kind.as_str(),
                                "product" | "acquisition" | "entitlement" | "remote_file"
                            ) {
                                expected.insert((kind, key), value);
                            }
                        }
                        checkpoint["verificationRecords"] = json!(expected
                            .into_iter()
                            .map(|((kind, key), value)| (kind, key, value))
                            .collect::<Vec<_>>());
                    }
                    let mut prepared = json!({"id":checkpoint_id,"provider":"jinxxy","accountId":account,"jobId":id,"scopeSignature":first_signature,"inventoryIds":finished,"preparedRecords":batch.staged_records(),"updatedAt":Utc::now().to_rfc3339()});
                    if page_limit.is_some() {
                        prepared["verificationRecords"] = checkpoint["verificationRecords"].clone();
                    }
                    self.inner
                        .catalogue
                        .put("checkpoint", &checkpoint_id, prepared)?;
                    batch.stage("checkpoint", &checkpoint_id, checkpoint)?;
                    if batch.try_commit()?.is_some() {
                        finished = next_finished;
                        for (detail, _, _) in &details {
                            self.clear_downloads(&account, Some(&detail.inventory_id));
                        }
                        let _ = app.emit(
                            "nna:changed",
                            json!({"scope":"catalogue","productIds":[product]}),
                        );
                        break;
                    }
                    ensure!(attempt < 2, "CONCURRENT_UPDATE");
                }
                completed_products.insert(product_key);
                count = completed_products.len();
                job["completedProducts"] = json!(count);
                job["progress"]["products"] = json!(count);
                self.save_job(app, job, "running", None)?;
            }
            if page.page >= observed_pages {
                break;
            }
            self.check_cancelled(&id)?;
            let next = page.page + 1;
            let data = self.page(&client, inventory_page_at(&origin, next)).await?;
            page = parse_inventory(&data, next)?;
            ensure!(page.pages == pages, "JINXXY_SCOPE_CHANGED");
        }
        self.check_cancelled(&id)?;
        let final_first = self.page(&client, inventory_page_at(&origin, 1)).await?;
        ensure!(
            hex::encode(Sha256::digest(
                last(&final_first, "inventory_items")?
                    .to_string()
                    .as_bytes()
            )) == first_signature,
            "JINXXY_SCOPE_CHANGED"
        );
        if observed_pages != pages {
            // Do not turn a bounded verification into authoritative membership.
            // Every grant on the observed pages has already finished enrichment.
            job["verification"]["capped"] = json!(true);
            self.save_job(app, job, "running", None)?;
            anyhow::bail!("VERIFICATION_PAGE_LIMIT");
        }
        validate_selected_inventory(job, &product_keys)?;
        // Only the verified listing closure certifies whole-product membership.
        // Each product's current owned grants change atomically, with cancellation
        // checked in the same expected-row transaction as the certification.
        for product in &completed_products {
            if incomplete_products.contains(product) { continue; }
            self.certify_product_membership(
                job,
                &account,
                &format!("jinxxy:{product}"),
                &finished,
            )?;
            let _ = app.emit(
                "nna:changed",
                json!({"scope":"catalogue","productIds":[format!("jinxxy:{product}")]}),
            );
        }
        job["completedProducts"] = json!(count);
        job["progress"]["products"] = json!(count);
        job["progress"]["metadataCompleted"] = json!(metadata_completed);
        job["progress"]["metadataTotal"] = json!(metadata_completed);
        job["progress"]["stage"] = json!("finalizing");
        self.save_job(app, job, "running", None)?;
        self.check_cancelled(&id)?;
        ensure!(incomplete_products.is_empty(), "JINXXY_MANIFEST_INCOMPLETE");
        self.save_job(app, job, "completed", None)
    }
    fn certify_product_membership(
        &self,
        job: &Value,
        account: &str,
        product: &str,
        finished: &BTreeSet<String>,
    ) -> Result<()> {
        for _ in 0..3 {
            let mut batch = crate::booth::RecordBatch::new(&self.inner.catalogue);
            let current = batch
                .get(
                    "job",
                    job["id"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?,
                )?
                .ok_or_else(|| anyhow::anyhow!("JOB_UNAVAILABLE"))?;
            ensure!(
                current["state"] != "cancelled" && current["cancelRequested"] != true,
                "CANCELLED"
            );
            for mut grant in batch.related("entitlement", product)? {
                if grant["accountId"] != account
                    || !grant["inventoryId"]
                        .as_str()
                        .is_some_and(|id| finished.contains(id))
                {
                    continue;
                }
                grant["manifestComplete"] = json!(true);
                let id = grant["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("CHECKPOINT_INVALID"))?
                    .to_owned();
                batch.stage("entitlement", &id, grant)?;
            }
            if batch.try_commit()?.is_some() {
                return Ok(());
            }
        }
        anyhow::bail!("CONCURRENT_UPDATE")
    }
    pub async fn download_ticket(&self, app: &AppHandle, file_id: &str) -> Result<DownloadTicket> {
        let file = self
            .inner
            .catalogue
            .get("remote_file", file_id)?
            .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
        ensure!(file["provider"] == "jinxxy", "STORE_ACCOUNT_MISMATCH");
        let account = account_arg(&file)?;
        let result = self
            .cached_download_ticket(app, file_id, &file, &account)
            .await;
        if let Err(error) = &result {
            match fixed_error(error).as_str() {
                "AUTH_REQUIRED"
                | "PERMISSION_DENIED"
                | "STORE_IDENTITY_CHANGED"
                | "JINXXY_IDENTITY_UNPROVEN"
                | "JINXXY_PRODUCT_BINDING_UNPROVEN" => self.clear_downloads(&account, None),
                "FILE_CHANGED" | "FILE_UNAVAILABLE" | "JINXXY_DOWNLOAD_UNAVAILABLE" => {
                    self.clear_downloads(&account, file["inventoryId"].as_str())
                }
                _ => {}
            }
        }
        result
    }
    async fn cached_download_ticket(
        &self,
        app: &AppHandle,
        file_id: &str,
        file: &Value,
        account: &str,
    ) -> Result<DownloadTicket> {
        let session = self.download_session(app, account).await?;
        let client = &session.client;
        let inventory = identity(&file["inventoryId"])?;
        let source_file = identity(&file["sourceFileId"])?;
        ensure!(
            file_id == format!("jinxxy:{account}:{inventory}:{source_file}"),
            "JINXXY_PRODUCT_BINDING_UNPROVEN"
        );
        let product = file["productId"]
            .as_str()
            .and_then(|id| id.strip_prefix("jinxxy:"))
            .ok_or_else(|| anyhow::anyhow!("JINXXY_PRODUCT_BINDING_UNPROVEN"))?;
        let cached_files = {
            let cache = self
                .inner
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            cache
                .get(account)
                .filter(|cached| cached.identity == session.identity)
                .and_then(|cached| cached.manifests.get(&inventory))
                .filter(|_| file["deliveryKind"] != "jinxxyCompiled3b")
                .filter(|manifest| manifest.valid(product, file, Instant::now()))
                .map(|manifest| manifest.files.clone())
        };
        let files = if let Some(files) = cached_files {
            files
        } else {
            self.clear_downloads(account, Some(&inventory));
            let data = self
                .page(
                    client,
                    session
                        .origin
                        .join(&format!("/my/inventory/{inventory}"))
                        .unwrap(),
                )
                .await?;
            let detail = parse_detail(&data, &inventory, Some(product))?;
            let record = self.account(account)?;
            ensure!(
                bound_account(&record) == Some(session.identity.as_str()),
                "STORE_IDENTITY_CHANGED"
            );
            let mut cache = self
                .inner
                .downloads
                .lock()
                .map_err(|_| anyhow::anyhow!("STATE_LOCK_FAILED"))?;
            if let Some(cached) = cache.get_mut(account).filter(|cached| {
                cached.valid(&record, Instant::now()) && cached.identity == session.identity
            }) {
                if cached.manifests.len() >= 8 {
                    if let Some(oldest) = cached
                        .manifests
                        .iter()
                        .min_by_key(|(_, manifest)| manifest.observed_at)
                        .map(|(id, _)| id.clone())
                    {
                        cached.manifests.remove(&oldest);
                    }
                }
                if detail.inventory_complete {
                    cached.manifests.insert(
                        inventory.clone(),
                        DownloadManifest {
                            product: product.to_owned(),
                            observed_at: Instant::now(),
                            files: detail.files.clone(),
                        },
                    );
                }
            }
            detail.files
        };
        let offered = files
            .iter()
            .find(|offered| offered.id == source_file)
            .ok_or_else(|| anyhow::anyhow!("FILE_UNAVAILABLE"))?;
        ensure!(
            file["sizeBytes"]
                .as_u64()
                .zip(offered.size_bytes)
                .is_none_or(|(old, current)| old == current),
            "FILE_CHANGED"
        );
        ensure!(
            file["revision"].as_str() == Some(file_revision(offered).as_str()),
            "FILE_CHANGED"
        );
        ensure!(
            offered.compiled == (file["deliveryKind"] == "jinxxyCompiled3b"),
            "FILE_CHANGED"
        );
        if offered.compiled {
            ensure!(
                bound_account(&self.account(account)?) == Some(session.identity.as_str()),
                "STORE_IDENTITY_CHANGED"
            );
            let request = compiled_request(&inventory, offered)?;
            // Disable redirects on the authenticated capability request as well.
            let metadata_client =
                super::http::build_head_client(StoreProvider::Jinxxy, session.jar.clone())?;
            let response = self
                .inner
                .http
                .post_json(&metadata_client, &session.origin, &request)
                .await?;
            let ticket = compiled_capability(&response)?.ticket().await?;
            ensure!(
                bound_account(&self.account(account)?) == Some(session.identity.as_str()),
                "STORE_IDENTITY_CHANGED"
            );
            return Ok(ticket);
        }
        let head_client =
            super::http::build_head_client(StoreProvider::Jinxxy, session.jar.clone())?;
        let response = self
            .inner
            .http
            .resolve_head(
                &head_client,
                inventory_file_endpoint(&inventory, &source_file)?,
            )
            .await?;
        let url = resolved_location(
            response.status(),
            response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok()),
        )?;
        ensure!(
            bound_account(&self.account(account)?) == Some(session.identity.as_str()),
            "STORE_IDENTITY_CHANGED"
        );
        Ok(DownloadTicket {
            provider: StoreProvider::Jinxxy,
            url,
            client: super::http::build_download_client(StoreProvider::Jinxxy, session.jar.clone())?,
            expected_size: offered.size_bytes,
            expected_origin: None,
        })
    }
}
pub fn validate_download_response(
    response: &reqwest::Response,
    expected_origin: Option<&Url>,
) -> Result<()> {
    ensure!(
        expected_origin.map_or_else(
            || file_destination(response.url()),
            |origin| compiled_destination(response.url())
                && response.url().origin() == origin.origin()
        ),
        "UNSAFE_DOWNLOAD_URL"
    );
    match response.status().as_u16() {
        401 => anyhow::bail!("AUTH_REQUIRED"),
        403 => anyhow::bail!("SIGNED_LINK_EXPIRED"),
        404 | 410 => anyhow::bail!("FILE_UNAVAILABLE"),
        429 => anyhow::bail!("RATE_LIMITED"),
        500..=599 => anyhow::bail!("DOWNLOAD_RESOLVER_FAILED"),
        _ => {}
    }
    ensure!(
        response.status().is_success(),
        "JINXXY_DOWNLOAD_UNAVAILABLE"
    );
    // The signed object may itself be HTML or JSON documentation. Transfer validates
    // the inventory's expected size and durable hash before recording completion.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_login_is_selected_independently_and_accounts_are_reserved_once() {
        let fixture = WorkerFixture::new();
        let service = fixture.service();
        let first = "d0140000-0000-4000-8000-000000000001";
        let second = "d0140000-0000-4000-8000-000000000002";
        for (id, kind, account) in [("authored-metadata", "jinxxySync", first), ("authored-login", "jinxxyLogin", second), ("authored-same-account", "jinxxyUpdate", first)] {
            service.inner.catalogue.put("job", id, json!({"id":id,"type":kind,"store":"Jinxxy","accountId":account,"state":"queued"})).unwrap();
        }
        let metadata_busy = service.inner.execution.try_lock().unwrap();
        let owner = Arc::downgrade(&service.inner);
        let (login_owner, login, login_work) = JinxxyService::next_worker_job(&owner).unwrap();
        assert_eq!(login["type"], "jinxxyLogin");
        let (metadata_owner, metadata, metadata_work) = JinxxyService::next_worker_job(&owner).unwrap();
        assert_eq!(metadata["accountId"], first);
        assert!(JinxxyService::next_worker_job(&owner).is_none());
        assert_eq!(service.inner.running.lock().unwrap().len(), 2);
        drop(login_work);
        drop(metadata_work);
        drop(login_owner);
        drop(metadata_owner);
        drop(metadata_busy);
        drop(service);
        assert!(owner.upgrade().is_none());
        fixture.finish();
    }
    #[test]
    fn incomplete_inventory_grant_keeps_known_files_and_does_not_block_healthy_publication() {
        let path = std::env::temp_dir().join(format!("nna-jinxxy-partial-grants-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = JinxxyService::new(catalogue.clone()).unwrap();
        let account = "d0140000-0000-4000-8000-000000000001";
        let partial_inventory = "1234567890123456701";
        let healthy_inventory = "1234567890123456702";
        let partial_product = "1234567890123456711";
        let healthy_product = "1234567890123456712";
        catalogue.put("account", account, json!({"id":account,"store":"Jinxxy"})).unwrap();
        let partial_entitlement = format!("jinxxy:{account}:{partial_inventory}");
        catalogue.put("entitlement", &partial_entitlement, json!({"id":partial_entitlement,"accountId":account,"productId":format!("jinxxy:{partial_product}"),"inventoryId":partial_inventory,"source":"library","fileIds":["retained-file"],"manifestComplete":false})).unwrap();
        let partial = parse_detail(&[json!({"inventory_item":{"id":partial_inventory,"item":{"id":partial_product,"name":"Authored incomplete","files":[]}}})], partial_inventory, Some(partial_product)).unwrap();
        let healthy = parse_detail(&[json!({"inventory_item":{"id":healthy_inventory,"item":{"id":healthy_product,"name":"Authored healthy","files":[{"id":"1234567890123456721","file_name":"healthy.txt","size":1}]}}})], healthy_inventory, Some(healthy_product)).unwrap();
        assert!(!partial.inventory_complete);
        assert!(healthy.inventory_complete);
        for detail in [partial, healthy] {
            let mut batch = crate::booth::RecordBatch::new(&catalogue);
            stage_observed_group(&mut batch, account, &[(detail, None, None)]).unwrap();
            batch.commit().unwrap();
        }
        assert_eq!(catalogue.get("entitlement", &partial_entitlement).unwrap().unwrap()["fileIds"], json!(["retained-file"]));
        let mut job = json!({"id":"authored-partial-job","type":"jinxxySync","store":"Jinxxy","accountId":account,"state":"running","completedProducts":2});
        catalogue.put("job", "authored-partial-job", job.clone()).unwrap();
        service.certify_product_membership(&job, account, &format!("jinxxy:{healthy_product}"), &BTreeSet::from([healthy_inventory.to_owned()])).unwrap();
        assert_eq!(catalogue.get("entitlement", &format!("jinxxy:{account}:{healthy_inventory}")).unwrap().unwrap()["manifestComplete"], true);
        assert_eq!(catalogue.get("entitlement", &partial_entitlement).unwrap().unwrap()["manifestComplete"], false);
        service.finish_job(&mut job, "needsAction", Some("JINXXY_MANIFEST_INCOMPLETE")).unwrap();
        assert_eq!(job["state"], "needsAction");
        assert_eq!(catalogue.related("acquisition", &format!("jinxxy:{healthy_product}")).unwrap().len(), 1);
        assert_eq!(catalogue.get("source_coverage", &format!("jinxxy:{account}:inventory")).unwrap().unwrap()["coverageComplete"], false);
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }

    struct WorkerFixture(std::path::PathBuf);
    impl WorkerFixture {
        fn new() -> Self {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/jinxxy-worker-owner")
                .join(format!("authored-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("library")).unwrap();
            Self(root)
        }
        fn service(&self) -> JinxxyService {
            let catalogue =
                Catalogue::open_bound(&self.0.join("library"), &self.0.join("device")).unwrap();
            JinxxyService::new(catalogue).unwrap()
        }
        fn finish(self) {
            let boundary = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/verification/jinxxy-worker-owner")
                .canonicalize()
                .unwrap();
            crate::storage::validate_library_path(&self.0).unwrap();
            let root = self.0.canonicalize().unwrap();
            assert_eq!(root.parent(), Some(boundary.as_path()));
            assert!(uuid::Uuid::parse_str(
                root.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .strip_prefix("authored-")
                    .unwrap()
            )
            .is_ok());
            let mut pending = vec![root.clone()];
            while let Some(path) = pending.pop() {
                crate::storage::validate_library_path(&path).unwrap();
                if path.is_dir() {
                    pending.extend(
                        std::fs::read_dir(path)
                            .unwrap()
                            .map(|entry| entry.unwrap().path()),
                    );
                }
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn idle_worker_releases_previous_library_when_services_are_replaced() {
        let fixture = WorkerFixture::new();
        let service = fixture.service();
        let root = service
            .inner
            .catalogue
            .library_root()
            .unwrap()
            .to_path_buf();
        let owner = Arc::downgrade(&service.inner);
        assert!(JinxxyService::next_worker_job(&owner).is_none());
        assert_eq!(owner.strong_count(), 1);
        let switching = service.inner.catalogue.begin_restore_work().unwrap();
        assert!(JinxxyService::next_worker_job(&owner).is_none());
        drop(switching);
        drop(service);
        assert!(owner.upgrade().is_none());
        assert!(JinxxyService::next_worker_job(&owner).is_none());
        let released = crate::library_lock::LibraryLock::acquire(&root).unwrap();
        drop(released);
        let reopened = Catalogue::open_bound(&root, &fixture.0.join("device")).unwrap();
        drop(reopened);
        fixture.finish();
    }
    #[test]
    fn selected_worker_job_retains_library_and_excludes_switch_until_finished() {
        let fixture = WorkerFixture::new();
        let service = fixture.service();
        let root = service
            .inner
            .catalogue
            .library_root()
            .unwrap()
            .to_path_buf();
        for (id, store) in [("authored-other", "BOOTH"), ("authored-selected", "Jinxxy")] {
            service
                .inner
                .catalogue
                .put(
                    "job",
                    id,
                    json!({"id":id,"type":"sync","store":store,"state":"queued"}),
                )
                .unwrap();
        }
        let owner = Arc::downgrade(&service.inner);
        let (running, mut job, work) = JinxxyService::next_worker_job(&owner).unwrap();
        assert_eq!(job["id"], "authored-selected");
        drop(service);
        assert!(owner.upgrade().is_some());
        assert!(crate::library_lock::LibraryLock::acquire(&root).is_err());
        assert_eq!(
            running
                .inner
                .catalogue
                .begin_restore_work()
                .err()
                .unwrap()
                .to_string(),
            "storage_busy"
        );
        job["state"] = json!("completed");
        running
            .inner
            .catalogue
            .put("job", "authored-selected", job)
            .unwrap();
        running
            .inner
            .catalogue
            .command("clear_activity", json!({}))
            .unwrap();
        // The live permit still protects terminal cleanup even after queued
        // records are gone. Releasing it permits the in-process switch.
        running
            .inner
            .catalogue
            .delete("job", "authored-other")
            .unwrap();
        assert!(running.inner.catalogue.begin_restore_work().is_err());
        drop(work);
        let switching = running.inner.catalogue.begin_restore_work().unwrap();
        drop(switching);
        drop(running);
        assert!(owner.upgrade().is_none());
        let released = crate::library_lock::LibraryLock::acquire(&root).unwrap();
        drop(released);
        fixture.finish();
    }
    #[test]
    fn terminal_result_does_not_recreate_a_removed_job() {
        let path =
            std::env::temp_dir().join(format!("nna-jinxxy-removed-job-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = JinxxyService::new(catalogue.clone()).unwrap();
        let account_id = "d0140000-0000-4000-8000-000000000001";
        let mut job = json!({"id":"removed-job","type":"jinxxySync","store":"Jinxxy","accountId":account_id,"state":"running"});
        assert_eq!(
            service
                .finish_job(&mut job, "completed", None)
                .unwrap_err()
                .to_string(),
            "JOB_UNAVAILABLE"
        );
        assert!(catalogue.get("job", "removed-job").unwrap().is_none());
        assert!(catalogue.list("source_coverage").unwrap().is_empty());
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn auth_required_expires_account_without_erasing_verified_ownership() {
        let path = std::env::temp_dir().join(format!("nna-jinxxy-auth-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let service = JinxxyService::new(catalogue.clone()).unwrap();
        let account_id = "d0140000-0000-4000-8000-000000000001";
        let account = json!({"id":account_id,"store":"Jinxxy","status":"connected","state":"connected","identityVerified":true,"identityHash":"authored-identity","authenticatedAt":"2026-01-01T00:00:00Z","lastSyncAt":"2026-01-02T00:00:00Z"});
        let entitlement = json!({"id":"authored-entitlement","accountId":account_id,"productId":"authored-product"});
        catalogue
            .put("entitlement", "authored-entitlement", entitlement.clone())
            .unwrap();
        for kind in ["jinxxySync", "jinxxyUpdate"] {
            catalogue
                .put("account", account_id, account.clone())
                .unwrap();
            let mut job = json!({"id":kind,"type":kind,"store":"Jinxxy","accountId":account_id,"state":"running"});
            catalogue.put("job", kind, job.clone()).unwrap();
            catalogue
                .command(
                    "preferences",
                    json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}}),
                )
                .unwrap();
            service
                .finish_job(&mut job, "needsAction", Some("AUTH_REQUIRED"))
                .unwrap();
            assert_eq!(
                catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"],
                json!({})
            );
            let expired = catalogue.get("account", account_id).unwrap().unwrap();
            assert_eq!(expired["status"], "expired");
            assert_eq!(expired["state"], "expired");
            for key in [
                "identityVerified",
                "identityHash",
                "authenticatedAt",
                "lastSyncAt",
            ] {
                assert_eq!(expired[key], account[key]);
            }
            assert_eq!(
                catalogue
                    .get("entitlement", "authored-entitlement")
                    .unwrap()
                    .unwrap(),
                entitlement
            );
            catalogue
                .put("account", account_id, account.clone())
                .unwrap();
            catalogue
                .command(
                    "preferences",
                    json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}}),
                )
                .unwrap();
            service
                .finish_job(&mut job, "failed", Some("NETWORK_UNAVAILABLE"))
                .unwrap();
            assert_eq!(
                catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"],
                json!({})
            );
            assert_eq!(
                catalogue.get("account", account_id).unwrap().unwrap()["status"],
                "connected"
            );
            catalogue
                .command(
                    "preferences",
                    json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}}),
                )
                .unwrap();
            service.finish_job(&mut job, "completed", None).unwrap();
            assert_eq!(job["state"], "completed");
            assert_eq!(
                catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"],
                json!({})
            );
            let mut cancellation = job.clone();
            cancellation["state"] = json!("running");
            cancellation["cancelRequested"] = json!(true);
            catalogue.put("job", kind, cancellation).unwrap();
            catalogue
                .command(
                    "preferences",
                    json!({"patch":{"avatarAutomaticEnabled":{"booth:3390957":true}}}),
                )
                .unwrap();
            service.finish_job(&mut job, "completed", None).unwrap();
            assert_eq!(
                job["state"], "cancelled",
                "Durable cancellation wins over a stale successful job"
            );
            assert_eq!(job["errorCode"], "CANCELLED");
            assert_eq!(
                catalogue.get("preferences", "app").unwrap().unwrap()["avatarAutomaticEnabled"],
                json!({})
            );
        }
        drop(service);
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    const INVENTORY: &str = "111111111111111111";
    const PRODUCT: &str = "222222222222222222";
    const FILE: &str = "333333333333333333";
    fn script(value: &Value) -> String {
        format!("<script>(window[Symbol.for(\"ApolloSSRDataTransport\")] ??= []).push({value});</script>")
    }
    #[test]
    fn streaming_ready_data_is_found_after_undefined_and_private_fields_are_discarded() {
        let loading = "<script>(window[Symbol.for(\"ApolloSSRDataTransport\")] ??= []).push({\"rehydrate\":{\"q\":{\"data\":undefined}}});</script>";
        let ready = json!({"events":[{"value":{"data":{"inventory_item":{"id":INVENTORY,"license":"AUTHORED_SECRET","seller_support":"AUTHORED_CONTACT","item":{"id":PRODUCT,"name":"undefined \\\" } remains text","files":[{"id":FILE,"file_name":"all-files.zip","size":0,"file_meta":{"secret":"AUTHORED_SECRET"}}],"user":{"username":"Fixture creator","email":"AUTHORED_CONTACT"}}},"viewer":{"token":"AUTHORED_SECRET"}}}}]});
        let data = buyer_data(&format!("{loading}{}", script(&ready))).unwrap();
        assert!(!serde_json::to_string(&data).unwrap().contains("AUTHORED_"));
        let parsed = parse_detail(&data, INVENTORY, Some(PRODUCT)).unwrap();
        assert!(parsed.inventory_complete);
        assert_eq!(parsed.files[0].size_bytes, Some(0));
        assert_eq!(
            parsed.title.as_deref(),
            Some("undefined \\\" } remains text")
        );
        assert!(parse_detail(&data, INVENTORY, Some(FILE)).is_err());
    }
    #[test]
    fn transport_rejects_executable_values_and_ignores_non_query_cache() {
        assert!(transport_objects(
            "(window[Symbol.for(\"ApolloSSRDataTransport\")] ??= []).push({\"data\":evil()});"
        )
        .is_err());
        assert!(transport_objects(
            "(window[Symbol.for(\"ApolloSSRDataTransport\")] ??= []).push({});evil();"
        )
        .is_err());
        assert!(buyer_data(&script(
            &json!({"cache":{"inventory_item":{"id":INVENTORY}}})
        ))
        .is_err());
        assert_eq!(
            normalize_undefined("[undefined,\"undefined\",{\"undefined\":undefined}]"),
            "[null,\"undefined\",{\"undefined\":null}]"
        );
    }
    #[test]
    fn buyer_identity_binds_protected_profile_navigation_without_seller_or_profile_details() {
        let html="<nav><a href='/authored_buyer' class='block'>Profile</a></nav><a href='/authored_seller'>Profile</a>";
        let profile = protected_profile_route(html).unwrap();
        let user = json!({"id":"4444444444444444444","username":"authored_buyer","bio":"AUTHORED_PRIVATE","email":"AUTHORED_PRIVATE"});
        let data=buyer_data(&script(&json!({"events":[{"value":{"data":{"user":user.clone()}}},{"value":{"data":{"user":user}}}]}))).unwrap();
        assert_eq!(
            profile_identity(&data, &profile).unwrap(),
            "4444444444444444444"
        );
        let localized_profile = protected_profile_route(
            "<nav><a href='https://zh.jinxxy.com/authored_buyer'>Profile</a></nav>",
        )
        .unwrap();
        assert_eq!(localized_profile, profile);
        assert_eq!(
            profile_identity(&data, &localized_profile).unwrap(),
            "4444444444444444444"
        );
        let chinese_profile = protected_profile_route(
            "<nav><a href='https://zh.jinxxy.com/authored_buyer'>个人资料</a></nav>",
        )
        .unwrap();
        assert_eq!(chinese_profile, profile);
        assert_eq!(
            profile_identity(&data, &chinese_profile).unwrap(),
            "4444444444444444444"
        );
        assert!(protected_profile_route(
            "<nav><a href='https://other.example/authored_buyer'>个人资料</a></nav>"
        )
        .is_err());
        assert!(!serde_json::to_string(&data)
            .unwrap()
            .contains("AUTHORED_PRIVATE"));
        assert!(profile_identity(
            &[json!({"user":{"id":PRODUCT,"username":"authored_seller"}})],
            &profile
        )
        .is_err());
        let mut switched = data.clone();
        switched.push(json!({"user":{"id":PRODUCT,"username":"authored_buyer"}}));
        assert!(profile_identity(&switched, &profile).is_err());
        for href in [
            "https://other.example/authored_buyer",
            "https://zh.jinxxy.com.example/authored_buyer",
            "https://ja.jinxxy.com/authored_buyer",
            "https://127.0.0.1/authored_buyer",
            "http://zh.jinxxy.com/authored_buyer",
            "https://zh.jinxxy.com:444/authored_buyer",
            "https://user@zh.jinxxy.com/authored_buyer",
            "https://zh.jinxxy.com/authored_buyer?token=x",
            "https://zh.jinxxy.com/authored_buyer#fragment",
            "/authored_buyer?token=x",
            "/seller/product",
            "/",
        ] {
            assert!(
                protected_profile_route(&format!("<nav><a href='{href}'>Profile</a></nav>"))
                    .is_err()
            );
        }
        assert!(protected_profile_route("<a href='/seller'>Profile</a>").is_err());
        assert!(protected_profile_route(
            "<nav><a href='https://zh.jinxxy.com/authored_buyer'>個人資料</a></nav>"
        )
        .is_err());
        assert!(identity(&json!("18446744073709551616")).is_err());
        assert!(identity(&json!("0")).is_err());
        let apex = Url::parse("https://jinxxy.com/").unwrap();
        let localized = Url::parse("https://zh.jinxxy.com/").unwrap();
        assert_eq!(
            native_origin(&Url::parse("https://zh.jinxxy.com/my/inventory?page=2").unwrap()),
            Some(localized.clone())
        );
        assert_eq!(
            inventory_page_at(&localized, 2).as_str(),
            "https://zh.jinxxy.com/my/inventory?page=2"
        );
        for host in [
            "jinxxy.com",
            "www.jinxxy.com",
            "ja.jinxxy.com",
            "de.jinxxy.com",
            "es.jinxxy.com",
            "fr.jinxxy.com",
            "zh.jinxxy.com",
            "ko.jinxxy.com",
        ] {
            let page = Url::parse(&format!("https://{host}/my/inventory?page=1")).unwrap();
            assert_eq!(native_origin(&page).unwrap().host_str(), Some(host));
            assert_eq!(english_origin(&page).is_ok(), host == "jinxxy.com");
        }
        assert_eq!(english_origin(&inventory_page_at(&apex, 1)).unwrap(), apex);
        for invalid in [
            "https://other.jinxxy.com/",
            "https://api.jinxxy.com/",
            "https://zh.jinxxy.com.example/",
            "https://127.0.0.1/",
            "http://zh.jinxxy.com/",
            "https://user@zh.jinxxy.com/",
            "https://zh.jinxxy.com:444/",
        ] {
            assert!(native_origin(&Url::parse(invalid).unwrap()).is_none());
            assert!(english_origin(&Url::parse(invalid).unwrap()).is_err());
        }
        use reqwest::cookie::CookieStore;
        let jar = reqwest::cookie::Jar::default();
        jar.add_cookie_str("authored=fixture; Path=/; Secure", &localized);
        assert!(jar.cookies(&inventory_page_at(&apex, 1)).is_none());
        assert!(jar.cookies(&inventory_page_at(&localized, 1)).is_some());
    }
    #[test]
    fn inventory_scope_duplicate_ids_and_incomplete_file_sets_remain_explicit() {
        let row = json!({"id":INVENTORY,"item":{"id":PRODUCT,"name":"Fixture"}});
        let data=buyer_data(&script(&json!({"rehydrate":{"q":{"data":{"inventory_items":{"page":1,"page_count":2,"payload":[row.clone()]}}}}}))).unwrap();
        assert_eq!(
            parse_inventory(&data, 1).unwrap().items[0]
                .product_id
                .as_deref(),
            Some(PRODUCT)
        );
        assert!(parse_inventory(&data, 2).is_err());
        assert_eq!(
            inventory_page_url(2).as_str(),
            "https://jinxxy.com/my/inventory?page=2"
        );
        assert!(parse_inventory(
            &[json!({"inventory_items":{"page":2,"page_count":1,"payload":[]}})],
            2
        )
        .is_err());
        assert!(parse_inventory(
            &[json!({"inventory_items":{"page":1,"page_count":1,"payload":[]}})],
            1
        )
        .unwrap()
        .items
        .is_empty());
        assert!(parse_inventory(
            &[json!({"inventory_items":{"page":1,"page_count":2,"payload":[]}})],
            1
        )
        .is_err());
        let duplicate =
            vec![json!({"inventory_items":{"page":1,"page_count":1,"payload":[row.clone(),row]}})];
        assert!(parse_inventory(&duplicate, 1).is_err());
        let data =
            vec![json!({"inventory_item":{"id":INVENTORY,"item":{"id":PRODUCT,"files":null}}})];
        assert!(
            !parse_detail(&data, INVENTORY, Some(PRODUCT))
                .unwrap()
                .inventory_complete
        );
        assert!(inventory_file_endpoint("../escape", FILE).is_err());
        assert_eq!(
            inventory_file_endpoint(INVENTORY, FILE).unwrap().host_str(),
            Some("api.jinxxy.com")
        );
    }
    #[test]
    fn orders_with_null_inventory_do_not_turn_order_totals_into_product_paid_price() {
        let order = json!({"order":{"id":INVENTORY,"payment_status":"PAID","paid_at":"2026-10-10T01:02:03Z","order_total":12345,"currency_code":"USD","inventory_items":null,"order_items":[{"id":FILE,"target_type":"PLATFORM_FEE","gross_price":12345,"description":"AUTHORED_PRIVATE"}]}});
        let html = format!(
            "<script>self.__next_f.push({});</script>",
            json!([1, format!("a:{order}\n")])
        );
        let payment = parse_order_payment(&html, INVENTORY, INVENTORY, PRODUCT).unwrap();
        assert!(payment.paid_price.is_none());
        assert!(payment.paid_currency.is_none());
        assert!(payment.paid_at.is_none());
        let mut selected = Vec::new();
        needed_orders(&order, &mut selected);
        let serialized = serde_json::to_string(&selected).unwrap();
        assert!(!serialized.contains("order_total"));
        assert!(!serialized.contains("AUTHORED_PRIVATE"));
        assert!(!serialized.contains("paidPrice"));
    }
    #[test]
    fn head_resolution_retains_get_capability_only_for_proven_exact_file_host() {
        let location =
            format!("https://{FILE_HOST}/authored/file.zip?X-Amz-Signature=authored-only");
        let url = resolved_location(reqwest::StatusCode::FOUND, Some(&location)).unwrap();
        assert_eq!(url.host_str(), Some(FILE_HOST));
        assert!(url.query().is_some());
        assert!(resolved_location(reqwest::StatusCode::FORBIDDEN, Some(&location)).is_err());
        for location in [
            format!("https://other.r2.cloudflarestorage.com/file.zip"),
            format!("https://{FILE_HOST}.example/file.zip"),
            format!("http://{FILE_HOST}/file.zip"),
            format!("https://user@{FILE_HOST}/file.zip"),
        ] {
            assert!(resolved_location(reqwest::StatusCode::FOUND, Some(&location)).is_err());
        }
    }
    #[test]
    fn flight_receipt_uses_unique_bound_product_gross_and_acquisition_precedence() {
        let order_id = "444444444444444444";
        let mut receipt = json!({"order":{"id":order_id,"payment_status":"PAID","paid_at":"2026-01-03T00:00:00Z","currency_code":"USD","order_total":3030,"payout_total":2450,"email":"AUTHORED_PRIVATE","order_items":[
            {"target_type":"PLATFORM_FEE","gross_price":350},
            {"target_type":"DIGITAL_PRODUCT","gross_price":3000,"net_price":2650,"price":3000,"inventory_item":{"id":INVENTORY,"item":{"id":PRODUCT},"license":"AUTHORED_PRIVATE"}}
        ]}});
        let html = |value: &Value| {
            let flight = format!("a:{}\n", json!(["$", "fixture", value]));
            let split = flight.len() / 2;
            format!(
                "<script>self.__next_f.push({});</script><script>self.__next_f.push({})</script>",
                json!([1, &flight[..split]]),
                json!([1, &flight[split..]])
            )
        };
        let payment = parse_order_payment(&html(&receipt), order_id, INVENTORY, PRODUCT).unwrap();
        assert_eq!(payment.paid_price, Some(30.0));
        assert_eq!(payment.paid_currency.as_deref(), Some("USD"));
        let mut selected = Vec::new();
        for record in flight_records(&html(&receipt)).unwrap() {
            needed_orders(&record, &mut selected);
        }
        let projected = serde_json::to_string(&selected).unwrap();
        for forbidden in [
            "AUTHORED_PRIVATE",
            "net_price",
            "payout_total",
            "order_total",
            "email",
            "license",
        ] {
            assert!(!projected.contains(forbidden));
        }
        assert!(
            parse_order_payment(&html(&receipt), "555555555555555555", INVENTORY, PRODUCT)
                .unwrap()
                .paid_price
                .is_none()
        );
        assert!(
            parse_order_payment(&html(&receipt), order_id, FILE, PRODUCT)
                .unwrap()
                .paid_price
                .is_none()
        );
        let duplicate = receipt["order"]["order_items"][1].clone();
        receipt["order"]["order_items"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(
            parse_order_payment(&html(&receipt), order_id, INVENTORY, PRODUCT)
                .unwrap()
                .paid_price
                .is_none()
        );
        receipt["order"]["order_items"]
            .as_array_mut()
            .unwrap()
            .pop();
        receipt["order"]["order_items"][1]["gross_price"] = json!(0);
        assert_eq!(
            parse_order_payment(&html(&receipt), order_id, INVENTORY, PRODUCT)
                .unwrap()
                .paid_price,
            Some(0.0)
        );
        receipt["order"]["currency_code"] = json!("JPY");
        assert!(
            parse_order_payment(&html(&receipt), order_id, INVENTORY, PRODUCT)
                .unwrap()
                .paid_price
                .is_none()
        );
        receipt["order"]["payment_status"] = json!("REFUNDED");
        assert!(
            parse_order_payment(&html(&receipt), order_id, INVENTORY, PRODUCT)
                .unwrap()
                .paid_at
                .is_none()
        );
        assert!(flight_records("<script>self.__next_f.push(runCode())</script>").is_err());
        let page = parse_inventory(&[json!({"inventory_items":{"page":1,"page_count":1,"payload":[{"id":INVENTORY,"created_at":"2026-01-02T00:00:00Z","item":{"id":PRODUCT,"created_at":"2020-01-01T00:00:00Z"}}]}})],1).unwrap();
        let mut detail = parse_detail(&[json!({"inventory_item":{"id":INVENTORY,"imported_purchased_at":"2026-01-01T00:00:00Z","item":{"id":PRODUCT,"files":[]}}})],INVENTORY,Some(PRODUCT)).unwrap();
        assert_eq!(
            acquisition_date(&detail, &page.items[0], Some(&payment)),
            date(&json!("2026-01-01T00:00:00Z"))
        );
        detail.imported_purchased_at = None;
        assert_eq!(
            acquisition_date(&detail, &page.items[0], Some(&payment)),
            date(&json!("2026-01-02T00:00:00Z"))
        );
        let mut entry = page.items.into_iter().next().unwrap();
        entry.created_at = None;
        assert_eq!(
            acquisition_date(&detail, &entry, Some(&payment)),
            payment.paid_at
        );
    }
    #[test]
    fn download_cache_expires_and_rejects_changed_identity_or_file_revision() {
        let now = Instant::now();
        let record = json!({"store":"Jinxxy","status":"connected","state":"connected","identityVerified":true,"identityHash":"authored-binding"});
        let session = DownloadSession {
            client: reqwest::Client::new(),
            jar: Arc::new(reqwest::cookie::Jar::default()),
            origin: Url::parse("https://zh.jinxxy.com/").unwrap(),
            identity: "authored-binding".into(),
            verified_at: now,
            manifests: BTreeMap::new(),
        };
        assert!(session.valid(&record, now + Duration::from_secs(119)));
        assert_eq!(session.snapshot().origin, session.origin);
        assert!(!session.valid(&record, now + Duration::from_secs(120)));
        for (key, value) in [
            ("identityHash", json!("other-binding")),
            ("identityVerified", json!(false)),
            ("disconnected", json!(true)),
            ("status", json!("disconnected")),
            ("state", json!("disconnected")),
        ] {
            let mut changed = record.clone();
            changed[key] = value;
            assert!(!session.valid(&changed, now));
        }
        let observed = FileObservation {
            id: FILE.into(),
            name: "authored.zip".into(),
            size_bytes: Some(20),
            updated_at: date(&json!("2026-10-10T01:02:03Z")),
            compiled: false,
            compiled_available: false,
        };
        let file = json!({"sourceFileId":FILE,"revision":file_revision(&observed)});
        let mut moved = observed.clone();
        moved.name = "Other/authored.zip".into();
        assert_ne!(file_revision(&moved), file_revision(&observed));
        let manifest = DownloadManifest {
            product: PRODUCT.into(),
            observed_at: now,
            files: vec![observed],
        };
        assert!(manifest.valid(PRODUCT, &file, now + Duration::from_secs(59)));
        assert!(!manifest.valid(PRODUCT, &file, now + Duration::from_secs(60)));
        assert!(!manifest.valid(INVENTORY, &file, now));
        let mut changed = file.clone();
        changed["revision"] = json!("same-file-new-revision");
        assert!(!manifest.valid(PRODUCT, &changed, now));
        changed = file;
        changed["sourceFileId"] = json!(INVENTORY);
        assert!(!manifest.valid(PRODUCT, &changed, now));
    }
    #[test]
    fn flat_file_paths_preserve_siblings_subfolders_and_compiled_incompleteness() {
        let path = std::env::temp_dir().join(format!(
            "nna-authored-jinxxy-folders-{}",
            uuid::Uuid::new_v4()
        ));
        let catalogue = Catalogue::open(&path).unwrap();
        let account = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let row = json!({"inventory_item":{"id":INVENTORY,"item":{"id":PRODUCT,"files":[
            {"id":FILE,"file_name":"Pack/avatar.unitypackage","size":10},
            {"id":"444444444444444444","file_name":"Pack/Textures/body.png","size":20},
            {"id":"555555555555555555","file_name":"Other/Textures/body.png","size":30}
        ]}}});
        let detail = parse_detail(&[row.clone()], INVENTORY, Some(PRODUCT)).unwrap();
        assert!(detail.inventory_complete);
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(&mut batch, account, &detail, None, None).unwrap();
        batch.commit().unwrap();
        let files = catalogue.list("remote_file").unwrap();
        assert_eq!(files.len(), 3);
        let paths: BTreeSet<_> = files
            .iter()
            .map(|file| file["sourceRelativePath"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            BTreeSet::from([
                "Pack/avatar.unitypackage",
                "Pack/Textures/body.png",
                "Other/Textures/body.png"
            ])
        );
        assert_eq!(
            files
                .iter()
                .filter(|file| file["name"] == "body.png")
                .count(),
            2
        );
        assert!(files
            .iter()
            .all(|file| file["accountId"] == account && file["inventoryId"] == INVENTORY));
        for invalid in [
            "../secret",
            "/absolute",
            "A/../secret",
            "A//leaf",
            "C:/secret",
            "A\\leaf",
        ] {
            assert!(source_file_name(invalid).is_err());
        }
        let mut compiled = row;
        compiled["inventory_item"]["item"]["files"][0]["extension"] = json!("3b");
        let detail = parse_detail(&[compiled.clone()], INVENTORY, Some(PRODUCT)).unwrap();
        assert!(!detail.inventory_complete);
        assert_eq!(detail.files.len(), 3);
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(&mut batch, account, &detail, None, None).unwrap();
        batch.commit().unwrap();
        let file = catalogue
            .get(
                "remote_file",
                &format!("jinxxy:{account}:{INVENTORY}:{FILE}"),
            )
            .unwrap()
            .unwrap();
        assert_eq!(file["deliveryKind"], "jinxxyCompiled3b");
        assert!(file["sizeBytes"].is_null());
        assert_eq!(file["sourceSizeBytes"], 10);
        assert_eq!(
            catalogue
                .get("entitlement", &format!("jinxxy:{account}:{INVENTORY}"))
                .unwrap()
                .unwrap()["manifestComplete"],
            false
        );
        assert!(compiled_request(INVENTORY, &detail.files[0]).is_err());
        compiled["inventory_item"]["item"]["files"][0]["file_meta"] = json!({
            "meta_type":"t3b_meta", "asset_bundles":{
                "windows":{"performance":"good","unrelated":"AUTHORED_PRIVATE"},
                "android":{"performance":"medium"}
            }
        });
        let data = buyer_data(&script(&json!({"events":[{"value":{"data":compiled}}]}))).unwrap();
        assert!(!serde_json::to_string(&data)
            .unwrap()
            .contains("AUTHORED_PRIVATE"));
        let detail = parse_detail(&data, INVENTORY, Some(PRODUCT)).unwrap();
        assert!(detail.inventory_complete);
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(&mut batch, account, &detail, None, None).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            catalogue
                .get("entitlement", &format!("jinxxy:{account}:{INVENTORY}"))
                .unwrap()
                .unwrap()["manifestComplete"],
            true
        );
        assert!(catalogue
            .get(
                "remote_file",
                &format!("jinxxy:{account}:{INVENTORY}:{FILE}")
            )
            .unwrap()
            .unwrap()["sizeBytes"]
            .is_null());
        let request = compiled_request(INVENTORY, &detail.files[0]).unwrap();
        // Both platform entries share one original ID, so it is requested exactly once.
        assert_eq!(
            request["variables"],
            json!({"inventory_id":INVENTORY,"file_ids":[FILE]})
        );
        assert!(compiled_request(INVENTORY, &detail.files[1]).is_err());
        assert!(compiled_request("not-an-inventory-id", &detail.files[0]).is_err());
        let response = json!({"data":{"inventory_item":{
            "downloadT3bFile":{"url":"https://download.example/compiled?cap=authored","token":"AUTHORED_EPHEMERAL_TOKEN"}
        }}});
        let capability = compiled_capability(&response).unwrap();
        assert!(capability.authorization.is_sensitive());
        assert_eq!(
            capability.url.origin().ascii_serialization(),
            "https://download.example"
        );
        let mut invalid = response.clone();
        invalid["data"]["inventory_item"] = Value::Null;
        assert!(compiled_capability(&invalid).is_err());
        invalid = response.clone();
        invalid["data"]["inventory_item"]["downloadT3bFile"]["token"] = json!("invalid\r\nheader");
        assert!(compiled_capability(&invalid).is_err());
        invalid = response.clone();
        invalid["errors"] = json!([{"message":"AUTHORED_PRIVATE"}]);
        assert!(compiled_capability(&invalid).is_err());
        for destination in [
            "http://download.example/file",
            "https://download.example:444/file",
            "https://user@download.example/file",
            "https://download.example/file#fragment",
            "https://127.0.0.1/file",
            "https://10.0.0.1/file",
            "https://169.254.169.254/file",
            "https://[::1]/file",
            "https://[fc00::1]/file",
            "https://host.local/file",
        ] {
            invalid = response.clone();
            invalid["data"]["inventory_item"]["downloadT3bFile"]["url"] = json!(destination);
            assert!(compiled_capability(&invalid).is_err());
        }
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "192.168.1.1",
            "::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_download_ip(address.parse().unwrap()));
        }
        assert!(public_download_ip("8.8.8.8".parse().unwrap()));
        drop(catalogue);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn native_jinxxy_records_bind_account_inventory_and_preserve_user_overlay_without_secrets() {
        let path =
            std::env::temp_dir().join(format!("nna-authored-jinxxy-{}", uuid::Uuid::new_v4()));
        let catalogue = Catalogue::open(&path).unwrap();
        let product_id = format!("jinxxy:{PRODUCT}");
        let mut row = json!({"inventory_item":{"id":INVENTORY,"license":"AUTHORED_SECRET","item":{"id":PRODUCT,"name":"Authored source title","url":"abc12","user":{"username":"authored-maker"},"base_price":3000,"currency_code":"USD","files":[{"id":FILE,"file_name":"source.psd","size":12,"updated_at":"2026-10-10T00:00:00Z"}],"cover":{"url":"https://jinxxy-c.jinxxy-cdn.com/authored.png"}}}});
        let detail = parse_detail(&[row.clone()], INVENTORY, Some(PRODUCT)).unwrap();
        assert_eq!(
            detail.source_url.as_deref(),
            Some("https://jinxxy.com/authored-maker/abc12")
        );
        assert_eq!(detail.current_price, Some(30.0));
        for segment in [
            "",
            "..",
            "../else",
            "https://example.test",
            "a?token=secret",
            "a%2Fb",
        ] {
            assert!(
                product_url(&json!({"user":{"username":"authored-maker"},"url":segment})).is_none()
            );
        }
        let overlay = json!({"id":product_id,"title":"Authored user title","note":"Authored note","category":"Hair","tags":["Authored manual tag"],"rejectedTags":["rejected"]});
        catalogue
            .put("overlay", &product_id, overlay.clone())
            .unwrap();
        for account in [
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        ] {
            let mut batch = crate::booth::RecordBatch::new(&catalogue);
            stage_detail(&mut batch, account, &detail, None, None).unwrap();
            batch.commit().unwrap();
        }
        // Prior same-product-bound public presentation is not supplied by the
        // buyer inventory query. An absent later public response must retain it.
        let mut presentation = catalogue.get("product", &product_id).unwrap().unwrap();
        presentation["sourceTags"] = json!(["Authored source tag", "rejected"]);
        presentation["sourceDescription"] = json!("Authored public description");
        catalogue.put("product", &product_id, presentation).unwrap();
        let assert_retained = |catalogue: &Catalogue, price: f64| {
            let source = catalogue.get("product", &product_id).unwrap().unwrap();
            assert_eq!(source["price"], price);
            assert_eq!(source["priceCurrency"], "USD");
            assert_eq!(
                source["sourceTags"],
                json!(["Authored source tag", "rejected"])
            );
            assert_eq!(source["sourceDescription"], "Authored public description");
            assert_eq!(
                catalogue.get("overlay", &product_id).unwrap().unwrap(),
                overlay
            );
            let projected = catalogue
                .command("product_details", json!({"id":product_id}))
                .unwrap();
            assert_eq!(projected["price"], price);
            assert_eq!(projected["title"], "Authored user title");
            assert_eq!(projected["note"], "Authored note");
            assert_eq!(projected["category"], "Hair");
            assert_eq!(
                projected["accountIds"],
                json!([
                    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
                ])
            );
            assert!(projected["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tag| tag == "Authored manual tag"));
            assert!(!projected["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tag| tag == "rejected"));
            assert_eq!(projected["rejectedTags"], json!(["rejected"]));
        };
        assert_retained(&catalogue, 30.0);
        let files = catalogue.list("remote_file").unwrap();
        assert_eq!(files.len(), 2);
        assert_ne!(files[0]["id"], files[1]["id"]);
        assert!(files.iter().all(|file| file["productId"] == product_id
            && file["sourceFileId"] == FILE
            && file.get("sourceUrl").is_none()));
        assert!(catalogue
            .list("acquisition")
            .unwrap()
            .iter()
            .all(|row| row["acquiredAt"].is_null() && row["paidPrice"].is_null()));
        assert_eq!(
            catalogue.get("overlay", &product_id).unwrap().unwrap()["title"],
            "Authored user title"
        );
        assert_eq!(
            catalogue.get("product", &product_id).unwrap().unwrap()["previewUrl"],
            "https://jinxxy-c.jinxxy-cdn.com/authored.png"
        );
        assert!(!serde_json::to_string(&catalogue.list("product").unwrap())
            .unwrap()
            .contains("AUTHORED_SECRET"));
        let projected = catalogue
            .command("product_details", json!({"id":product_id}))
            .unwrap();
        assert_eq!(projected["price"], 30.0);
        assert_eq!(projected["priceCurrency"], "USD");
        assert_eq!(
            projected["sourceUrl"],
            "https://jinxxy.com/authored-maker/abc12"
        );
        assert_eq!(
            catalogue.get("product", &product_id).unwrap().unwrap()["priceBasis"],
            "jinxxyBasePrice"
        );
        row["inventory_item"]["item"]["base_price"] = json!(0);
        let free = parse_detail(&[row.clone()], INVENTORY, Some(PRODUCT)).unwrap();
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(
            &mut batch,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            &free,
            None,
            None,
        )
        .unwrap();
        batch.commit().unwrap();
        assert_eq!(
            catalogue
                .command("product_details", json!({"id":product_id}))
                .unwrap()["price"],
            0.0
        );
        row["inventory_item"]["item"]["currency_code"] = json!("JPY");
        let account = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let unknown_listing_price = parse_detail(&[row.clone()], INVENTORY, Some(PRODUCT)).unwrap();
        assert!(unknown_listing_price.current_price.is_none());
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(&mut batch, account, &unknown_listing_price, None, None).unwrap();
        batch.commit().unwrap();
        let unknown_price_product = catalogue.get("product", &product_id).unwrap().unwrap();
        assert_eq!(unknown_price_product["price"], 0.0);
        assert_eq!(unknown_price_product["priceCurrency"], "USD");
        let entitlement = format!("jinxxy:{account}:{INVENTORY}");
        let original_file = format!("jinxxy:{account}:{INVENTORY}:{FILE}");
        let new_source = "7777777777777777777";
        let new_file = format!("jinxxy:{account}:{INVENTORY}:{new_source}");
        let acquired = date(&json!("2026-01-02T00:00:00Z"));
        let known_payment = OrderPayment {
            paid_price: Some(12.5),
            paid_currency: Some("USD".into()),
            paid_at: acquired,
        };
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(&mut batch, account, &free, acquired, Some(&known_payment)).unwrap();
        batch.commit().unwrap();
        // One unnamed offered file makes this refresh partial. Its known membership
        // and prior financial observations must survive both absent and unbound payment.
        row["inventory_item"]["item"]["files"] = json!([
            {"id":FILE,"size":12},
            {"id":new_source,"file_name":"additional.txt","size":5}
        ]);
        let partial = parse_detail(&[row.clone()], INVENTORY, Some(PRODUCT)).unwrap();
        assert!(!partial.inventory_complete);
        let unknown_payment = OrderPayment::default();
        for payment in [None, Some(&unknown_payment)] {
            let mut batch = crate::booth::RecordBatch::new(&catalogue);
            stage_detail(&mut batch, account, &partial, None, payment).unwrap();
            batch.commit().unwrap();
            let source = catalogue.get("product", &product_id).unwrap().unwrap();
            assert_eq!(source["price"], 0.0);
            assert_eq!(source["priceCurrency"], "USD");
            assert_eq!(source["priceBasis"], "jinxxyBasePrice");
            assert_retained(&catalogue, 0.0);
            let purchase = catalogue.get("acquisition", &entitlement).unwrap().unwrap();
            assert_eq!(purchase["paidPrice"], 12.5);
            assert_eq!(purchase["paidCurrency"], "USD");
            assert_eq!(purchase["paidPriceBasis"], "productLineTotal");
            assert_eq!(purchase["acquiredAt"], acquired.unwrap().to_rfc3339());
            let grant = catalogue.get("entitlement", &entitlement).unwrap().unwrap();
            assert_eq!(grant["manifestComplete"], false);
            let required: BTreeSet<_> = grant["fileIds"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(
                required,
                BTreeSet::from([original_file.as_str(), new_file.as_str()])
            );
        }
        drop(catalogue);
        let catalogue = Catalogue::open(&path).unwrap();
        assert_retained(&catalogue, 0.0);
        let retained_partial = catalogue.get("entitlement", &entitlement).unwrap().unwrap();
        assert_eq!(retained_partial["manifestComplete"], false);
        assert_eq!(retained_partial["fileIds"].as_array().unwrap().len(), 2);
        assert_eq!(
            catalogue.get("acquisition", &entitlement).unwrap().unwrap()["paidPrice"],
            12.5
        );
        // A complete observation may replace membership and a genuine zero payment.
        row["inventory_item"]["item"]["files"] =
            json!([{"id":new_source,"file_name":"additional.txt","size":5}]);
        row["inventory_item"]["item"]["base_price"] = json!(900);
        row["inventory_item"]["item"]["currency_code"] = json!("USD");
        let complete = parse_detail(&[row], INVENTORY, Some(PRODUCT)).unwrap();
        let zero_payment = OrderPayment {
            paid_price: Some(0.0),
            paid_currency: Some("USD".into()),
            paid_at: None,
        };
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        stage_detail(&mut batch, account, &complete, None, Some(&zero_payment)).unwrap();
        batch.commit().unwrap();
        let grant = catalogue.get("entitlement", &entitlement).unwrap().unwrap();
        assert_eq!(grant["manifestComplete"], true);
        assert_eq!(grant["fileIds"], json!([new_file]));
        assert!(catalogue
            .get("remote_file", &original_file)
            .unwrap()
            .is_some());
        assert_eq!(
            catalogue.get("product", &product_id).unwrap().unwrap()["price"],
            9.0
        );
        assert_eq!(
            catalogue.get("acquisition", &entitlement).unwrap().unwrap()["paidPrice"],
            0.0
        );
        assert_eq!(
            catalogue.get("overlay", &product_id).unwrap().unwrap()["title"],
            "Authored user title"
        );
        assert_retained(&catalogue, 9.0);
        let coverage_id = format!("jinxxy:{account}:inventory");
        let prior_success = "2026-01-02T00:00:00+00:00";
        catalogue
            .put(
                "source_coverage",
                &coverage_id,
                json!({
                    "id":coverage_id,"provider":"jinxxy","accountId":account,"source":"library",
                    "status":"complete","coverageComplete":true,"productCount":1,"completedAt":prior_success,"updatedAt":prior_success
                }),
            )
            .unwrap();
        let service = JinxxyService::new(catalogue.clone()).unwrap();
        let mut job = json!({"type":"jinxxySync","accountId":account,"completedProducts":3});
        service.coverage(&job, "running", None).unwrap();
        let coverage = || {
            catalogue
                .get("source_coverage", &coverage_id)
                .unwrap()
                .unwrap()
        };
        let running = coverage();
        assert_eq!(running["store"], "Jinxxy");
        assert_eq!(running["status"], "running");
        assert_eq!(running["coverageComplete"], false);
        assert_eq!(running["lastSuccessfulAt"], prior_success);
        assert!(running.get("state").is_none() && running.get("complete").is_none());
        for (state, code, expected) in [
            ("needsAction", "AUTH_REQUIRED", "auth-required"),
            ("failed", "JINXXY_SOURCE_UNSUPPORTED", "unsupported"),
            ("failed", "NETWORK_UNAVAILABLE", "failed"),
            ("failed", "VERIFICATION_PAGE_LIMIT", "incomplete"),
            ("cancelled", "CANCELLED", "cancelled"),
        ] {
            service.coverage(&job, state, Some(code)).unwrap();
            let terminal = coverage();
            assert_eq!(terminal["status"], expected);
            assert_eq!(terminal["coverageComplete"], false);
            assert_eq!(terminal["errorCode"], code);
            assert_eq!(terminal["lastSuccessfulAt"], prior_success);
            assert!(date(&terminal["completedAt"]).is_some());
        }
        service.coverage(&job, "complete", None).unwrap();
        let complete_coverage = coverage();
        assert_eq!(complete_coverage["status"], "complete");
        assert_eq!(complete_coverage["coverageComplete"], true);
        assert_eq!(complete_coverage["productCount"], 3);
        assert_eq!(
            complete_coverage["lastSuccessfulAt"],
            complete_coverage["completedAt"]
        );
        assert!(complete_coverage["errorCode"].is_null());
        job["type"] = json!("jinxxyUpdate");
        service
            .coverage(&job, "failed", Some("NETWORK_UNAVAILABLE"))
            .unwrap();
        assert_eq!(coverage(), complete_coverage);
        // Multiple grants are one publication unit, including on an incomplete
        // second grant. Prepared rows never change the prior visible snapshot.
        let prior_product = catalogue.get("product", &product_id).unwrap();
        let selected = json!({"type":"jinxxyUpdate","productIds":[product_id]});
        assert!(validate_selected_inventory(&selected, &BTreeSet::new()).is_err());
        assert!(
            validate_selected_inventory(&selected, &BTreeSet::from([PRODUCT.to_owned()])).is_ok()
        );
        let mixed_selected =
            json!({"type":"jinxxyUpdate","productIds":[product_id,"jinxxy:1234567890123456791"]});
        assert!(validate_selected_inventory(
            &mixed_selected,
            &BTreeSet::from([PRODUCT.to_owned()])
        )
        .is_err());
        let second_inventory = "1234567890123456790";
        let first_row = json!({"inventory_item":{"id":INVENTORY,"item":{"id":PRODUCT,"name":"New grouped title","files":[{"id":FILE,"file_name":"one.txt","size":1}]}}});
        let second_row =
            json!({"inventory_item":{"id":second_inventory,"item":{"id":PRODUCT,"files":null}}});
        let mut group = vec![
            (
                parse_detail(&[first_row], INVENTORY, Some(PRODUCT)).unwrap(),
                None,
                None,
            ),
            (
                parse_detail(&[second_row], second_inventory, Some(PRODUCT)).unwrap(),
                None,
                None,
            ),
        ];
        let mut batch = crate::booth::RecordBatch::new(&catalogue);
        assert!(stage_complete_group(&mut batch, account, &group).is_err());
        assert_eq!(batch.len(), 0);
        assert_eq!(
            catalogue.get("product", &product_id).unwrap(),
            prior_product
        );
        group[1].0.inventory_complete = true;
        group[1].0.files = group[0].0.files.clone();
        stage_observed_group(&mut batch, account, &group).unwrap();
        assert_eq!(
            catalogue.get("product", &product_id).unwrap(),
            prior_product
        );
        batch.commit().unwrap();
        assert_retained(&catalogue, 9.0);
        for inventory in [INVENTORY, second_inventory] {
            assert_eq!(
                catalogue
                    .get("entitlement", &format!("jinxxy:{account}:{inventory}"))
                    .unwrap()
                    .unwrap()["manifestComplete"],
                false
            );
        }
        job["id"] = json!("jinxxy:authored-membership");
        job["state"] = json!("running");
        catalogue
            .put("job", "jinxxy:authored-membership", job.clone())
            .unwrap();
        service
            .certify_product_membership(
                &job,
                account,
                &product_id,
                &BTreeSet::from([INVENTORY.to_owned(), second_inventory.to_owned()]),
            )
            .unwrap();
        assert!(catalogue
            .related("entitlement", &product_id)
            .unwrap()
            .iter()
            .all(|grant| grant["manifestComplete"] == true));
        assert_retained(&catalogue, 9.0);
        // A complete, bound group publishes without optional receipt data;
        // the earlier proven zero payment survives, and the new grant stays unknown.
        assert_eq!(
            catalogue.get("acquisition", &entitlement).unwrap().unwrap()["paidPrice"],
            0.0
        );
        assert!(catalogue
            .get(
                "acquisition",
                &format!("jinxxy:{account}:{second_inventory}")
            )
            .unwrap()
            .unwrap()["paidPrice"]
            .is_null());
        catalogue
            .put(
                "account",
                account,
                json!({"id":account,"store":"Jinxxy","lastSyncAt":prior_success}),
            )
            .unwrap();
        job["id"] = json!("jinxxy:authored-terminal");
        job["type"] = json!("jinxxySync");
        job["state"] = json!("running");
        catalogue
            .put("job", "jinxxy:authored-terminal", job.clone())
            .unwrap();
        service.finish_job(&mut job, "completed", None).unwrap();
        assert_eq!(job["progress"]["stage"], "completed");
        assert!(date(&job["completedAt"]).is_some());
        let successful_at =
            catalogue.get("account", account).unwrap().unwrap()["lastSyncAt"].clone();
        assert_eq!(successful_at, job["completedAt"]);
        let mut cancelled = job.clone();
        cancelled["cancelRequested"] = json!(true);
        catalogue
            .put("job", "jinxxy:authored-terminal", cancelled)
            .unwrap();
        service.finish_job(&mut job, "completed", None).unwrap();
        assert_eq!(job["state"], "cancelled");
        assert_eq!(job["progress"]["stage"], "cancelled");
        assert_eq!(coverage()["coverageComplete"], false);
        let account_row = catalogue.get("account", account).unwrap().unwrap();
        assert_eq!(account_row["lastSyncAt"], successful_at);
        assert_eq!(account_row["syncState"], "incomplete");
        assert_eq!(account_row["lastSyncErrorCode"], "CANCELLED");
        drop(service);
        drop(catalogue);
        let reopened = Catalogue::open(&path).unwrap();
        assert_retained(&reopened, 9.0);
        let retained_purchase = reopened.get("acquisition", &entitlement).unwrap().unwrap();
        assert_eq!(retained_purchase["accountId"], account);
        assert_eq!(retained_purchase["paidPrice"], 0.0);
        assert_eq!(retained_purchase["paidCurrency"], "USD");
        assert_eq!(retained_purchase["paidPriceBasis"], "productLineTotal");
        assert_eq!(
            retained_purchase["acquiredAt"],
            acquired.unwrap().to_rfc3339()
        );
        assert_eq!(
            reopened.related("entitlement", &product_id).unwrap().len(),
            3
        );
        assert!(reopened
            .related("entitlement", &product_id)
            .unwrap()
            .iter()
            .all(|grant| grant["manifestComplete"] == true));
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }
}
