//! Unauthenticated editor suggestions. Public URLs never establish ownership.
use super::{http, StoreProvider};
use anyhow::{bail, ensure, Result};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use url::Url;

pub(super) fn product_url(raw: &str) -> Result<Option<(StoreProvider, Url)>> {
    let raw = raw.trim();
    ensure!(
        raw.len() <= 8192 && !raw.contains('\\') && !raw.chars().any(char::is_control),
        "INVALID_PRODUCT_URL"
    );
    let url = Url::parse(raw).map_err(|_| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    let rest = raw
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_URL"))?
        .1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    ensure!(
        url.scheme() == "https"
            && !authority.contains([':', '@'])
            && url.query().is_none()
            && url.fragment().is_none(),
        "INVALID_PRODUCT_URL"
    );
    let host = url.host_str().unwrap_or("");
    if host == "booth.pm" || host.ends_with(".booth.pm") {
        return Ok(None);
    }
    let provider = if host == "gumroad.com" || host.ends_with(".gumroad.com") {
        StoreProvider::Gumroad
    } else if super::session::jinxxy_host(host) {
        StoreProvider::Jinxxy
    } else {
        bail!("UNSUPPORTED_PROVIDER")
    };
    let original = rest.strip_prefix(authority).unwrap_or("");
    let parts: Vec<_> = original.split('/').collect();
    let segment = |part: &str| {
        !part.is_empty()
            && part.len() <= 256
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    };
    let valid = match (provider, parts.as_slice()) {
        (StoreProvider::Gumroad, ["", "l", slug]) => {
            segment(slug) && host != "files.gumroad.com" && host != "app.gumroad.com"
        }
        (StoreProvider::Jinxxy, ["", seller, slug]) => {
            segment(seller)
                && segment(slug)
                && !matches!(
                    *seller,
                    "my" | "login" | "sign_in" | "graphql" | "api" | "marketplace" | "search"
                )
        }
        _ => false,
    };
    ensure!(valid && url.path() == original, "INVALID_PRODUCT_URL");
    Ok(Some((provider, url)))
}

fn text(raw: &str, max: usize) -> String {
    Html::parse_fragment(raw)
        .root_element()
        .text()
        .collect::<String>()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Jinxxy descriptions are serialized rich-text trees, not their JSON syntax.
pub(super) fn jinxxy_description(raw: &str) -> String {
    fn append(node: &Value, output: &mut String) {
        match node["type"].as_str() {
            Some("text") => {
                if let Some(value) = node["text"].as_str() {
                    output.push_str(value);
                }
            }
            Some("linebreak") => output.push('\n'),
            _ => {}
        }
        for child in node["children"].as_array().into_iter().flatten() {
            append(child, output);
        }
        if matches!(node["type"].as_str(), Some("paragraph" | "heading" | "listitem")) {
            output.push('\n');
        }
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        if value["root"]["children"].is_array() {
            let mut result = String::new();
            append(&value["root"], &mut result);
            return result
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .filter(|c| !c.is_control())
                .take(40000)
                .collect();
        }
        if value.is_object() || value.is_array() {
            return String::new();
        }
    }
    Html::parse_fragment(raw)
        .root_element()
        .text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(40000)
        .collect()
}

// Decode data literals only. Text records use UTF-8 byte lengths and can contain
// newlines; splitting the whole stream into lines would lose their boundaries.
fn jinxxy_flight(document: &Html) -> Result<BTreeMap<String, Value>> {
    const LIMIT: usize = 16 * 1024 * 1024;
    const PUSH: &str = "self.__next_f.push(";
    let mut stream = String::new();
    for script in document.select(&Selector::parse("script").unwrap()) {
        // HTML serialization escapes raw script text (including NBSP), changing
        // Flight's declared UTF-8 byte lengths. Read the text nodes verbatim.
        let content = script.text().collect::<String>();
        let mut remaining = content.trim();
        if let Some(value) = remaining.strip_prefix("(self.__next_f=self.__next_f||[]).push([0]);") {
            remaining = value.trim_start();
        }
        while let Some(literal) = remaining.strip_prefix(PUSH) {
            let mut decoder = serde_json::Deserializer::from_str(literal).into_iter::<Value>();
            let value = decoder.next().transpose()
                .map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?
                .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
            remaining = literal[decoder.byte_offset()..].trim_start().strip_prefix(')')
                .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?.trim_start();
            remaining = remaining.strip_prefix(';').unwrap_or(remaining).trim_start();
            if value[0] == 1 {
                let chunk = value[1].as_str()
                    .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
                ensure!(stream.len() + chunk.len() <= LIMIT, "PUBLIC_METADATA_INVALID");
                stream.push_str(chunk);
            }
        }
    }
    let mut records = BTreeMap::new();
    let mut remaining = stream.as_str();
    while !remaining.is_empty() {
        let Some((id, body)) = remaining.split_once(':') else { break; };
        // Next's public stream includes ID-less CSS/font preload hints. They
        // carry no model data and must not prevent reading the product records.
        if id.is_empty() && body.starts_with("HL[") {
            remaining = body.split_once('\n').map_or("", |(_, rest)| rest);
            continue;
        }
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("PUBLIC_METADATA_INVALID");
        }
        if let Some(length) = body.strip_prefix('T') {
            let (length, payload) = length.split_once(',')
                .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
            let length = usize::from_str_radix(length, 16)
                .map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
            ensure!(length <= payload.len() && payload.is_char_boundary(length), "PUBLIC_METADATA_INVALID");
            records.insert(id.to_owned(), Value::String(payload[..length].to_owned()));
            remaining = payload[length..].trim_start_matches('\n');
        } else {
            let (row, rest) = body.split_once('\n').unwrap_or((body, ""));
            if let Ok(value) = serde_json::from_str(row) {
                records.insert(id.to_owned(), value);
            }
            remaining = rest;
        }
    }
    Ok(records)
}

fn jinxxy_presentation(source: &Url, document: &Html, expected_id: Option<&str>) -> Result<Option<Value>> {
    fn products<'a>(value: &'a Value, result: &mut Vec<&'a Value>) {
        match value {
            Value::Object(object) => {
                if value["__typename"] == "PublicProduct" {
                    result.push(value);
                }
                for child in object.values() { products(child, result); }
            }
            Value::Array(values) => for child in values { products(child, result); },
            _ => {}
        }
    }
    let (_, validated) = product_url(source.as_str())?
        .filter(|(provider, _)| *provider == StoreProvider::Jinxxy)
        .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    let parts: Vec<_> = validated.path().split('/').collect();
    let records = jinxxy_flight(document)?;
    let mut candidates = Vec::new();
    for record in records.values() { products(record, &mut candidates); }
    let Some(product) = candidates.iter().copied().find(|product| {
        product["user"]["username"].as_str() == Some(parts[1])
            && product["url"].as_str() == Some(parts[2])
    }) else {
        ensure!(candidates.is_empty(), "PUBLIC_SOURCE_HOST_MISMATCH");
        return Ok(None);
    };
    let id = product["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 20
        && id.bytes().all(|byte| byte.is_ascii_digit()) && id.parse::<u64>().is_ok_and(|id| id > 0))
        .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
    ensure!(expected_id.is_none_or(|expected| expected == id), "PUBLIC_SOURCE_HOST_MISMATCH");
    let resolve = |value: &Value| -> Option<String> {
        let raw = value.as_str()?;
        if let Some(id) = raw.strip_prefix('$').filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit())) {
            records.get(id)?.as_str().map(str::to_owned)
        } else if raw == "$undefined" {
            None
        } else { Some(raw.to_owned()) }
    };
    let labels = |value: &Value| -> Option<Vec<String>> {
        Some(value.as_array()?.iter().filter_map(Value::as_str).take(100)
            .map(|value| text(value,256)).filter(|value| !value.is_empty()).collect())
    };
    let mut result = json!({"publicProductId":id,"sourceUrl":source.as_str()});
    if let Some(title) = resolve(&product["name"]).map(|title| text(&title,4096)).filter(|title| !title.is_empty()) {
        result["sourceTitle"] = json!(title);
    }
    if let Some(description) = resolve(&product["description"])
        .map(|description| jinxxy_description(&description))
        .filter(|description| !description.is_empty())
    {
        result["sourceDescription"] = json!(description);
    }
    let category = resolve(&product["category"]).map(|category| text(&category,256));
    let types = labels(&product["type"]);
    if category.is_some() || types.is_some() {
        result["sourceCategory"] = json!({"name":category.unwrap_or_default(),"types":types.unwrap_or_default()});
    }
    if let Some(tags) = labels(&product["tags"]) { result["sourceTags"] = json!(tags); }
    if let Some(url) = product["cover"]["url"].as_str().and_then(|raw| crate::preview::parse_public_url(raw).ok()) {
        result["previewUrl"] = json!(url.as_str());
    }
    Ok(Some(result))
}

fn suggestions(
    url: &Url,
    title: &str,
    description: &str,
    tags: Vec<String>,
    thumbnail: Option<&str>,
    provider: Option<StoreProvider>,
    source_category: Option<&Value>,
) -> Result<Value> {
    let title = text(title, 4096);
    ensure!(!title.is_empty(), "PUBLIC_METADATA_INVALID");
    let tags: BTreeSet<_> = tags
        .iter()
        .take(100)
        .map(|tag| text(tag, 256))
        .filter(|tag| !tag.is_empty())
        .collect();
    // Gumroad's bound JSON body is already decoded plain text; do not parse its
    // literal angle brackets or HTML entities a second time.
    let description = if provider == Some(StoreProvider::Gumroad) {
        description.to_owned()
    } else {
        text(description, 40000)
    };
    let mut product =
        json!({"sourceTitle":title,"sourceDescription":description,"sourceTags":tags});
    if let Some(provider) = provider { product["provider"] = json!(provider.key()); }
    if let Some(category) = source_category { product["sourceCategory"] = category.clone(); }
    let category = crate::taxonomy::infer_product_category(&product)["category"].clone();
    let thumbnail = thumbnail
        .and_then(|raw| crate::preview::parse_public_url(raw).ok())
        .map(|url| url.to_string());
    Ok(
        json!({"sourceUrl":url.as_str(),"title":title,"category":category,"tags":tags,"thumbnailUrl":thumbnail}),
    )
}

fn gumroad_values(url: &Url, value: &Value) -> Result<Value> {
    let observation = super::gumroad::parse_public_product(value, None)?;
    if let Some(permalink) = value["short_permalink"]
        .as_str()
        .or_else(|| value["permalink"].as_str())
    {
        ensure!(
            url.path().strip_prefix("/l/") == Some(permalink),
            "PUBLIC_SOURCE_HOST_MISMATCH"
        );
    }
    let tags = value["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tag| tag.as_str().or_else(|| tag["name"].as_str()))
        .map(str::to_owned)
        .collect();
    suggestions(
        url,
        value["name"].as_str().unwrap_or(""),
        observation.description.as_deref().unwrap_or(""),
        tags,
        value["preview_url"]
            .as_str()
            .or_else(|| value["cover_image_url"].as_str())
            .or_else(|| value["thumbnail_url"].as_str()),
        Some(StoreProvider::Gumroad),
        None,
    )
}

fn html_values(url: &Url, html: &str) -> Result<Value> {
    let document = Html::parse_document(html);
    let meta = |key: &str| {
        document
            .select(&Selector::parse("meta").unwrap())
            .find_map(|node| {
                let name = node
                    .value()
                    .attr("property")
                    .or_else(|| node.value().attr("name"));
                (name == Some(key))
                    .then(|| node.value().attr("content"))
                    .flatten()
                    .map(str::to_owned)
            })
    };
    // Public presentation only; never parse buyer/inventory/receipt records.
    if let Some(canonical) = meta("og:url") {
        let canonical = product_url(&canonical)?
            .ok_or_else(|| anyhow::anyhow!("PUBLIC_SOURCE_HOST_MISMATCH"))?
            .1;
        ensure!(
            canonical.path() == url.path() && canonical.host_str() == url.host_str(),
            "PUBLIC_SOURCE_HOST_MISMATCH"
        );
    }
    if let Some(product) = jinxxy_presentation(url, &document, None)? {
        let title = product["sourceTitle"].as_str().map(str::to_owned)
            .or_else(|| meta("og:title")).unwrap_or_default();
        let thumbnail = product["previewUrl"].as_str().map(str::to_owned)
            .or_else(|| meta("og:image"));
        return suggestions(
            url,
            &title,
            product["sourceDescription"].as_str().unwrap_or(""),
            product["sourceTags"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
            thumbnail.as_deref(),
            Some(StoreProvider::Jinxxy),
            product.get("sourceCategory"),
        );
    }
    let title = meta("og:title").ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
    let description = meta("og:description")
        .or_else(|| meta("description"))
        .unwrap_or_default();
    let tags = meta("keywords")
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect();
    suggestions(url, &title, &description, tags, meta("og:image").as_deref(), Some(StoreProvider::Jinxxy), None)
}

async fn fetch_body(
    transport: &http::MetadataHttp,
    provider: StoreProvider,
    source: Url,
) -> Result<String> {
    let client = http::build_client(provider, Arc::new(reqwest::cookie::Jar::default()))?;
    let mut request = source.clone();
    if provider == StoreProvider::Gumroad {
        request.set_path(&format!("{}.json", source.path()));
    }
    let (status, body, final_url) = transport.get_with_url(&client, request.clone()).await?;
    ensure!(final_url == request, "PUBLIC_SOURCE_HOST_MISMATCH");
    if matches!(status.as_u16(), 404 | 410) {
        bail!("PUBLIC_METADATA_UNAVAILABLE");
    }
    ensure!(status.is_success(), "PUBLIC_METADATA_FAILED");
    Ok(body)
}

/// Public presentation is bound to a buyer-known product ID but grants no access.
pub(super) async fn fetch_jinxxy_presentation(
    transport: &http::MetadataHttp,
    source: Url,
    expected_product_id: &str,
) -> Result<Value> {
    product_url(source.as_str())?
        .filter(|(provider, _)| *provider == StoreProvider::Jinxxy)
        .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    let body = fetch_body(transport, StoreProvider::Jinxxy, source.clone()).await?;
    jinxxy_presentation(&source, &Html::parse_document(&body), Some(expected_product_id))?
        .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))
}

pub(super) async fn fetch(
    transport: &http::MetadataHttp,
    provider: StoreProvider,
    source: Url,
) -> Result<Value> {
    let body = fetch_body(transport, provider, source.clone()).await?;
    match provider {
        StoreProvider::Gumroad => gumroad_values(
            &source,
            &serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?,
        ),
        StoreProvider::Jinxxy => html_values(&source, &body),
        StoreProvider::Booth => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authored_product() -> Value {
        json!({
            "__typename":"PublicProduct", "id":"42", "url":"product",
            "user":{"username":"authored"}, "name":"Authored Coat",
            "category":"avatar-clothing", "type":["clothing"], "tags":["coat","cloth"],
            "cover":{"url":"https://booth.pximg.net/authored.png"},
            "description":"$a", "product_type":"DIGITAL_PRODUCT",
            "files":[{"secret":"NOT_PRESENTATION"}], "license":"NOT_PRESENTATION"
        })
    }

    #[test]
    fn gumroad_url_fetch_requires_explicit_title_or_visible_product_body_vrchat_evidence() {
        let source = Url::parse("https://authored.gumroad.com/l/shader").unwrap();
        let mut product = json!({"api_version":1,"id":"authored-product",
            "short_permalink":"shader","name":"Authored Shader",
            "tags":["Shader","VRChat"],"description":"Blender shader"});
        assert_eq!(gumroad_values(&source, &product).unwrap()["category"], "NonVRChat");
        product["description"] = json!("<p>Shader for VR<strong>Chat</strong></p>");
        assert_eq!(gumroad_values(&source, &product).unwrap()["category"], "Shader");
        product["description"] = json!("<a href='https://example.invalid/VRChat' title='VRChat'>Read manual</a><img alt='VRC'><script>VRChat</script><style>VRC</style><nav>VRChat</nav>");
        assert_eq!(gumroad_values(&source, &product).unwrap()["category"], "NonVRChat");
        product["name"] = json!("Authored VRC Shader");
        assert_eq!(gumroad_values(&source, &product).unwrap()["category"], "Shader");
        product["name"] = json!("Authored Shader");
        product["description"] = json!("Shader for &lt;VRChat&gt;");
        assert_eq!(gumroad_values(&source, &product).unwrap()["category"], "Shader");
        product["description"] = Value::Null;
        assert_eq!(gumroad_values(&source, &product).unwrap()["category"], "NonVRChat");
    }

    fn authored_flight(product: Value, description: Option<&str>) -> String {
        let mut stream = format!("1:{}\n", json!({"product":product}));
        if let Some(description) = description {
            stream.push_str(&format!("a:T{:x},{description}\n", description.len()));
        }
        let split = stream.char_indices().map(|(offset, _)| offset)
            .find(|offset| *offset >= stream.len() / 2).unwrap_or(stream.len());
        format!(
            "<script>(self.__next_f=self.__next_f||[]).push([0]);self.__next_f.push({});</script><script>self.__next_f.push({})</script>",
            json!([1,&stream[..split]]), json!([1,&stream[split..]])
        )
    }

    #[test]
    fn jinxxy_public_flight_keeps_only_bound_presentation() {
        let description = json!({"root":{"type":"root","children":[
            {"type":"heading","children":[{"type":"text","text":"Authored coat"}]},
            {"type":"paragraph","children":[
                {"type":"text","text":"For VRChat. ","style":"NON_TEXT".repeat(3000)},
                {"type":"link","url":"https://example.com/NON_TEXT","children":[{"type":"text","text":"Read manual"}]},
                {"type":"linebreak"}, {"type":"text","text":"対応衣装"}
            ]},
            {"type":"media","url":"https://example.com/NON_TEXT","text":"NON_TEXT"}
        ]}}).to_string();
        assert!(description.len() > 16384);
        let source = Url::parse("https://jinxxy.com/authored/product").unwrap();
        let html = authored_flight(authored_product(), Some(&description));
        let presentation = jinxxy_presentation(&source, &Html::parse_document(&html), Some("42"))
            .unwrap().unwrap();
        assert_eq!(presentation["publicProductId"], "42");
        assert_eq!(presentation["sourceUrl"], source.as_str());
        assert_eq!(presentation["sourceTitle"], "Authored Coat");
        assert_eq!(presentation["sourceDescription"], "Authored coat For VRChat. Read manual 対応衣装");
        assert_eq!(presentation["sourceCategory"], json!({"name":"avatar-clothing","types":["clothing"]}));
        assert_eq!(presentation["sourceTags"], json!(["coat","cloth"]));
        assert_eq!(presentation.as_object().unwrap().len(), 7);
        assert!(!presentation.to_string().contains("NOT_PRESENTATION"));
        assert!(!presentation.to_string().contains("NON_TEXT"));
        assert!(jinxxy_presentation(&source, &Html::parse_document(&html), Some("43")).is_err());
        let other = Url::parse("https://jinxxy.com/other/product").unwrap();
        assert!(jinxxy_presentation(&other, &Html::parse_document(&html), Some("42")).is_err());
        let other = Url::parse("https://jinxxy.com/authored/other").unwrap();
        assert!(jinxxy_presentation(&other, &Html::parse_document(&html), Some("42")).is_err());
        let editor = html_values(&source, &html).unwrap();
        assert_eq!(editor["title"], "Authored Coat");
        assert_eq!(editor["tags"], json!(["cloth","coat"]));
        assert_eq!(editor.as_object().unwrap().len(), 5);
    }

    #[test]
    fn jinxxy_public_optional_fields_do_not_invent_metadata() {
        let source = Url::parse("https://zh.jinxxy.com/authored/product").unwrap();
        let product = json!({"__typename":"PublicProduct","id":"42","url":"product","user":{"username":"authored"}});
        let html = authored_flight(product, None);
        let presentation = jinxxy_presentation(&source, &Html::parse_document(&html), Some("42"))
            .unwrap().unwrap();
        assert_eq!(presentation, json!({"publicProductId":"42","sourceUrl":source.as_str()}));
        let html = format!("<meta property=\"og:title\" content=\"Authored title\">{html}");
        let editor = html_values(&source, &html).unwrap();
        assert_eq!(editor["title"], "Authored title");
        assert_eq!(editor["tags"], json!([]));
        assert_eq!(editor.as_object().unwrap().len(), 5);
        assert!(jinxxy_presentation(&source, &Html::parse_document("<p>No product data</p>"), Some("42"))
            .unwrap().is_none());
    }

    #[test]
    fn jinxxy_flight_text_lengths_are_utf8_bytes_and_scripts_are_data() {
        let source = Url::parse("https://jinxxy.com/authored/product").unwrap();
        let html = authored_flight(authored_product(), Some("衣装\nsecond line"));
        let presentation = jinxxy_presentation(&source, &Html::parse_document(&html), Some("42"))
            .unwrap().unwrap();
        assert_eq!(presentation["sourceDescription"], "衣装 second line");
        let invalid = format!("<script>self.__next_f.push({})</script>", json!([1,"a:T1,衣"]));
        assert!(jinxxy_flight(&Html::parse_document(&invalid)).is_err());
        let invalid = format!("<script>self.__next_f.push({})</script>", json!([1,"a:Tff,short"]));
        assert!(jinxxy_flight(&Html::parse_document(&invalid)).is_err());
        assert!(jinxxy_flight(&Html::parse_document("<script>self.__next_f.push(runCode())</script>")).is_err());
    }

    #[test]
    fn jinxxy_flight_preserves_raw_script_text_record_bytes() {
        let description = json!({"root":{"type":"root","children":[
            {"type":"paragraph","children":[{"type":"text","text":"Raw <angle> & nonbreaking\u{00a0}space"}]}
        ]}}).to_string();
        let html = authored_flight(authored_product(), Some(&description));
        let document = Html::parse_document(&html);
        assert!(document.select(&Selector::parse("script").unwrap()).any(|script| {
            script.inner_html() != script.text().collect::<String>()
        }));
        let records = jinxxy_flight(&document).unwrap();
        assert_eq!(records["a"], description);
        let source = Url::parse("https://jinxxy.com/authored/product").unwrap();
        let presentation = jinxxy_presentation(&source, &document, Some("42")).unwrap().unwrap();
        assert_eq!(presentation["sourceDescription"], "Raw <angle> & nonbreaking space");
        assert_eq!(html_values(&source, &html).unwrap().as_object().unwrap().len(), 5);
    }

    #[test]
    fn jinxxy_flight_skips_idless_preload_hints_but_requires_model_ids() {
        let html = format!("<script>self.__next_f.push({})</script>",
            json!([1,":HL[\"/authored.css\",\"style\"]\n:HL[\"/authored.woff2\",\"font\"]\n1:{\"name\":\"Authored model\"}\n"]));
        let records = jinxxy_flight(&Html::parse_document(&html)).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records["1"]["name"], "Authored model");
        let invalid = format!("<script>self.__next_f.push({})</script>",
            json!([1,":{\"name\":\"Missing model ID\"}\n"]));
        assert!(jinxxy_flight(&Html::parse_document(&invalid)).is_err());
    }

    #[test]
    fn jinxxy_description_ignores_unrecognized_json_fields() {
        assert_eq!(jinxxy_description(r#"{"root":{"children":[],"secret":"IGNORE"}}"#), "");
        assert_eq!(jinxxy_description(r#"{"root":{"secret":"IGNORE"}}"#), "");
        assert_eq!(jinxxy_description("<p>Authored coat</p>"), "Authored coat");
    }

    #[test]
    fn public_editor_urls_and_results_do_not_grant_ownership() {
        for raw in [
            "https://author.gumroad.com/l/authored",
            "https://gumroad.com/l/authored",
            "https://jinxxy.com/authored/product",
        ] {
            assert!(product_url(raw).unwrap().is_some());
        }
        for raw in [
            "https://jinxxy.com/my/inventory",
            "https://jinxxy.com/a/../b/c",
            "https://jinxxy.com/a/b?secret=1",
            "https://gumroad.com/d/secret",
            "https://files.gumroad.com/l/secret",
            "https://secret@jinxxy.com/a/b",
            "https://jinxxy.com:443/a/b",
            "https://jinxxy.com/a/%62",
        ] {
            assert!(product_url(raw).is_err(), "{raw}");
        }
        let source = Url::parse("https://jinxxy.com/authored/product").unwrap();
        let value = html_values(&source,r#"<meta property="og:title" content="Authored Shader"><meta property="og:description" content="VRChat Unity shader"><meta name="keywords" content="VRChat,Shader"><meta property="og:image" content="https://booth.pximg.net/authored.png"><script>PRIVATE_INVENTORY</script>"#).unwrap();
        assert_eq!(value["category"], "Shader");
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert!(!value.to_string().contains("PRIVATE"));
        assert!(html_values(&source,r#"<meta property="og:title" content="Other"><meta property="og:url" content="https://jinxxy.com/other/product">"#).is_err());
        let source = Url::parse("https://authored.gumroad.com/l/shader").unwrap();
        let value = gumroad_values(&source,&json!({"api_version":1,"id":"authored-id","short_permalink":"shader","name":"Authored Shader","description":"VRChat shader","tags":["Shader"],"thumbnail_url":"https://booth.pximg.net/a.png","purchase":"PRIVATE"})).unwrap();
        assert_eq!(value["title"], "Authored Shader");
        assert_eq!(value.as_object().unwrap().len(), 5);
    }
}
