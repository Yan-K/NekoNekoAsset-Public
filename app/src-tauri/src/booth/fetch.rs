//! Public editor suggestions only. No account session or catalogue writes.
use super::{http, metadata, safety, BoothService};
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use url::Url;

struct ItemUrl {
    id: String,
    source: Url,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FetchedProductMetadata {
    source_url: String,
    title: String,
    category: String,
    tags: Vec<String>,
    thumbnail_url: Option<String>,
}
fn item_url(raw: &str) -> Result<ItemUrl> {
    let raw = raw.trim();
    if raw.len() > 8192 || raw.contains('\\') || raw.chars().any(char::is_control) {
        bail!("INVALID_PRODUCT_URL");
    }
    let mut source = Url::parse(raw).map_err(|_| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    let (_, rest) = raw
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if !safety::secure(&source)
        || authority.contains([':', '@'])
        || source.query().is_some()
        || source.fragment().is_some()
    {
        bail!("INVALID_PRODUCT_URL");
    }
    if !source.host_str().is_some_and(safety::booth_host) {
        bail!("UNSUPPORTED_PROVIDER");
    }
    if matches!(
        source.host_str(),
        Some("accounts.booth.pm" | "manage.booth.pm")
    ) {
        bail!("INVALID_PRODUCT_URL");
    }
    // Inspect the original path too: URL normalization must not hide dot segments.
    let original_path = rest.strip_prefix(authority).unwrap_or("");
    let parts: Vec<_> = original_path.split('/').collect();
    let id = match parts.as_slice() {
        ["", "items", id] => *id,
        ["", locale, "items", id] if matches!(*locale, "ja" | "en") => *id,
        _ => bail!("INVALID_PRODUCT_URL"),
    };
    if !safety::numeric(id) || safety::item_id(source.as_str()).as_deref() != Some(id) {
        bail!("INVALID_PRODUCT_URL");
    }
    let normalized = safety::public_url(source.as_str(), "item")
        .ok_or_else(|| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    source = Url::parse(&normalized).map_err(|_| anyhow::anyhow!("INVALID_PRODUCT_URL"))?;
    Ok(ItemUrl {
        id: id.into(),
        source,
    })
}
fn staged(item: &ItemUrl, payload: &Value) -> Result<FetchedProductMetadata> {
    let public = metadata::public_metadata(payload, &item.id)?;
    let host = item.source.host_str().unwrap();
    if !matches!(host, "booth.pm" | "www.booth.pm") {
        let shop = public["sourceShop"]["url"]
            .as_str()
            .and_then(|s| Url::parse(s).ok());
        if shop.as_ref().and_then(Url::host_str) != Some(host) {
            bail!("PUBLIC_SOURCE_HOST_MISMATCH");
        }
    }
    let category = crate::taxonomy::infer_product_category(&public)["category"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?
        .to_owned();
    let title = public["sourceTitle"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?
        .to_owned();
    let tags = public["sourceTags"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let thumbnail_url = payload["images"]
        .as_array()
        .into_iter()
        .flatten()
        .take(100)
        .filter_map(|image| image["original"].as_str())
        .find_map(|raw| {
            crate::preview::parse_public_url(raw)
                .ok()
                .map(|url| url.to_string())
        });
    Ok(FetchedProductMetadata {
        source_url: item.source.to_string(),
        title,
        category,
        tags,
        thumbnail_url,
    })
}
impl BoothService {
    pub(super) async fn fetch_product_metadata(&self, raw: &str) -> Result<FetchedProductMetadata> {
        let item = item_url(raw)?;
        let client = http::build_client(Arc::new(reqwest::cookie::Jar::default()), false)?;
        let url = Url::parse(&format!("https://booth.pm/ja/items/{}.json", item.id)).unwrap();
        let (status, body) = self.inner.http.get(&client, url).await?;
        if matches!(status.as_u16(), 404 | 410) {
            bail!("PUBLIC_METADATA_UNAVAILABLE");
        }
        if !status.is_success() {
            bail!("PUBLIC_METADATA_FAILED");
        }
        let payload =
            serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("PUBLIC_METADATA_INVALID"))?;
        staged(&item, &payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn editor_url_fetch_accepts_only_canonical_public_booth_items() {
        for raw in [
            "https://booth.pm/items/900001",
            "https://booth.pm/ja/items/900001",
            "https://synthetic.booth.pm/en/items/900001",
        ] {
            assert_eq!(item_url(raw).unwrap().id, "900001");
        }
        assert_eq!(
            item_url("https://booth.pm/items/900001")
                .unwrap()
                .source
                .as_str(),
            "https://booth.pm/ja/items/900001"
        );
        assert_eq!(
            item_url("https://synthetic.booth.pm/en/items/900001")
                .unwrap()
                .source
                .as_str(),
            "https://synthetic.booth.pm/en/items/900001"
        );
        for raw in [
            "http://booth.pm/items/900001",
            "https://user:secret@booth.pm/items/900001",
            "https://booth.pm:443/items/900001",
            "https://booth.pm/items/900001?token=PRIVATE_TOKEN",
            "https://booth.pm/items/900001#private",
            "https://booth.pm/ignored/../items/900001",
            "https://booth.pm/items/%39%30%30%30%30%31",
            "https://booth.pm/items/900001/",
            "https://booth.pm/downloadables/900001",
            "https://accounts.booth.pm/items/900001",
            "https://manage.booth.pm/items/900001",
            "https://booth.pm\\items\\900001",
            "https://boo\nth.pm/items/900001",
        ] {
            assert_eq!(
                item_url(raw).err().unwrap().to_string(),
                "INVALID_PRODUCT_URL"
            );
        }
        assert_eq!(
            item_url("https://gumroad.com/items/900001")
                .err()
                .unwrap()
                .to_string(),
            "UNSUPPORTED_PROVIDER"
        );
        assert_eq!(
            item_url("https://booth.pm.evil.test/items/900001")
                .err()
                .unwrap()
                .to_string(),
            "UNSUPPORTED_PROVIDER"
        );
    }
    #[test]
    fn fetched_editor_values_have_closed_schema_and_never_include_entitlements() {
        let payload = json!({"id":900001,"name":"<b>Authored VRChat Shader</b>","description":"Unity shader for VRChat","category":{"name":"3D"},"tags":[{"name":"VRChat"},{"name":"Shader"}],"shop":{"url":"https://synthetic.booth.pm/"},"images":[{"original":"https://booth.pximg.net/unsafe.png?token=PRIVATE_TOKEN"},{"original":"https://booth.pximg.net/authored.png"}],"cookies":"PRIVATE_SESSION","variations":[{"downloadable":{"url":"https://booth.pm/downloadables/700001?signature=PRIVATE_SIGNATURE"}}]});
        let value = serde_json::to_value(
            staged(
                &item_url("https://synthetic.booth.pm/items/900001").unwrap(),
                &payload,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["title"], "Authored VRChat Shader");
        assert_eq!(value["category"], "Shader");
        assert_eq!(value["tags"], json!(["VRChat", "Shader"]));
        assert_eq!(
            value["thumbnailUrl"],
            "https://booth.pximg.net/authored.png"
        );
        assert_eq!(
            value["sourceUrl"],
            "https://synthetic.booth.pm/items/900001"
        );
        let keys = ["sourceUrl", "title", "category", "tags", "thumbnailUrl"];
        assert_eq!(value.as_object().unwrap().len(), keys.len());
        assert!(value
            .as_object()
            .unwrap()
            .keys()
            .all(|key| keys.contains(&key.as_str())));
        assert!(!value.to_string().contains("PRIVATE"));
        let mut no_image = payload.clone();
        no_image["images"] = json!([]);
        assert!(serde_json::to_value(
            staged(
                &item_url("https://booth.pm/items/900001").unwrap(),
                &no_image
            )
            .unwrap()
        )
        .unwrap()["thumbnailUrl"]
            .is_null());
        assert_eq!(
            staged(
                &item_url("https://other.booth.pm/items/900001").unwrap(),
                &payload
            )
            .err()
            .unwrap()
            .to_string(),
            "PUBLIC_SOURCE_HOST_MISMATCH"
        );
        assert_eq!(
            staged(
                &item_url("https://booth.pm/items/900002").unwrap(),
                &payload
            )
            .err()
            .unwrap()
            .to_string(),
            "PUBLIC_ID_MISMATCH"
        );
    }
}
