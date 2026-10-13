use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use url::Url;

pub fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
pub fn numeric(value: &str) -> bool {
    !value.is_empty() && value.len() <= 24 && value.bytes().all(|c| c.is_ascii_digit())
}
pub fn account_id(value: &str) -> Result<String> {
    Ok(uuid::Uuid::parse_str(value)
        .map_err(|_| anyhow::anyhow!("INVALID_ACCOUNT_ID"))?
        .to_string())
}
pub fn secure(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}
pub fn booth_host(host: &str) -> bool {
    host == "booth.pm" || host.ends_with(".booth.pm")
}
pub fn auth_navigation(url: &Url) -> bool {
    secure(url)
        && url
            .host_str()
            .is_some_and(|h| booth_host(h) || h == "pixiv.net" || h.ends_with(".pixiv.net"))
        && ![
            "/downloadables/",
            "/gifts/",
            "/items/",
            "/cart",
            "/checkout",
        ]
        .iter()
        .any(|p| url.path().contains(p))
}
pub fn download_destination(url: &Url) -> bool {
    secure(url)
        && url.host_str().is_some_and(|h| {
            booth_host(h) || h == "booth.pximg.net" || h == "booth-download.pximg.net"
        })
}
pub fn item_id(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if !secure(&url)
        || !url
            .host_str()
            .is_some_and(|h| booth_host(h) && !matches!(h, "accounts.booth.pm" | "manage.booth.pm"))
    {
        return None;
    }
    let pieces: Vec<_> = url.path_segments()?.collect();
    let id = match pieces.as_slice() {
        ["items", id] => *id,
        [locale, "items", id] if locale.bytes().all(|c| c.is_ascii_alphabetic() || c == b'-') => {
            *id
        }
        _ => return None,
    };
    numeric(id).then(|| id.to_owned())
}
pub fn public_url(value: &str, kind: &str) -> Option<String> {
    let mut url = Url::parse(value).ok()?;
    if !secure(&url) {
        return None;
    }
    let host = url.host_str()?;
    let valid = match kind {
        "item" => item_id(value).is_some(),
        "shop" => {
            booth_host(host)
                && !matches!(
                    host,
                    "booth.pm" | "accounts.booth.pm" | "manage.booth.pm" | "www.booth.pm"
                )
                && url.path() == "/"
        }
        "preview" => host == "booth.pximg.net" || host.ends_with(".pximg.net"),
        _ => false,
    };
    if !valid {
        return None;
    }
    if kind == "item" && host == "booth.pm" && url.path().starts_with("/items/") {
        url.set_path(&format!("/ja{}", url.path()));
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}
/// Normalize only observed first-party canonical identity for the live item parser.
pub(super) fn observed_product_url(value: &str) -> Option<String> {
    let path = value.strip_prefix("https://booth.pm")?;
    let pieces: Vec<_> = path.split('/').collect();
    match pieces.as_slice() {
        ["", "items", id] if numeric(id) => Some(format!("https://booth.pm/ja/items/{id}")),
        ["", locale, "items", id] if matches!(*locale, "ja" | "en") && numeric(id) => {
            Some(value.to_owned())
        }
        _ => None,
    }
}
pub(super) fn free_item_id(route: &str) -> Option<&str> {
    route
        .strip_prefix("https://booth.pm/ja/items/")
        .filter(|id| numeric(id))
}
pub(super) fn free_item_route(id: &str) -> Result<String> {
    if !numeric(id) {
        bail!("INVALID_FREE_ITEM_ID");
    }
    Ok(format!("https://booth.pm/ja/items/{id}"))
}
pub fn resolver(value: &str) -> Option<(String, Option<String>, String)> {
    let url = Url::parse(value).ok()?;
    if !secure(&url) || url.host_str()? != "booth.pm" {
        return None;
    }
    let id = url.path().strip_prefix("/downloadables/")?;
    if !numeric(id) {
        return None;
    }
    let mut variant = None;
    for (key, value) in url.query_pairs() {
        if key != "variation_id" || !numeric(&value) || variant.is_some() {
            return None;
        }
        variant = Some(value.into_owned());
    }
    let route = format!(
        "https://booth.pm/downloadables/{id}{}",
        variant
            .as_ref()
            .map(|v| format!("?variation_id={v}"))
            .unwrap_or_default()
    );
    Some((id.to_owned(), variant, route))
}
/// Read a DOM control's stable IDs, discarding all signing parameters immediately.
/// Unlike resolver(), this never supplies an untrusted URL to the HTTP client.
pub fn observed_resolver(value: &str) -> Option<(String, Option<String>, String)> {
    let url = Url::parse(value).ok()?;
    if !secure(&url) || url.host_str() != Some("booth.pm") {
        return None;
    }
    let observed = url.path().strip_prefix("/downloadables/")?;
    let id = observed.strip_suffix("/deeplink").unwrap_or(observed);
    if !numeric(id) {
        return None;
    }
    let mut variant = None;
    for (key, value) in url.query_pairs() {
        if key == "variation_id" {
            if variant.is_some() || !numeric(&value) {
                return None;
            }
            variant = Some(value.into_owned());
        }
    }
    let route = format!(
        "https://booth.pm/downloadables/{id}{}",
        variant
            .as_ref()
            .map(|v| format!("?variation_id={v}"))
            .unwrap_or_default()
    );
    Some((id.into(), variant, route))
}
pub fn source_path(source: &str) -> Result<&'static str> {
    match source {
        "library" => Ok("/library"),
        "purchase" | "orders" => Ok("/orders"),
        "gift" | "gifts" => Ok("/library/gifts"),
        "free" => Ok("/library/free_downloads"),
        _ => bail!("INVALID_SOURCE"),
    }
}
pub fn source_name(source: &str) -> Result<&'static str> {
    match source {
        "library" => Ok("library"),
        "purchase" | "orders" => Ok("purchase"),
        "gift" | "gifts" => Ok("gift"),
        "free" => Ok("free"),
        _ => bail!("INVALID_SOURCE"),
    }
}
pub fn route(value: &str, source: &str) -> Result<Url> {
    let base = Url::parse("https://accounts.booth.pm").unwrap();
    let mut url = base
        .join(value)
        .map_err(|_| anyhow::anyhow!("INVALID_SOURCE_ROUTE"))?;
    if !secure(&url) || url.host_str() != Some("accounts.booth.pm") || url.fragment().is_some() {
        bail!("INVALID_SOURCE_ROUTE");
    }
    let path = source_path(source)?;
    if url.path() == path {
        // BOOTH's canonical first-page anchor/response may omit the query.
        // All requests and parser projections still use the explicit page route.
        if url.query().is_none() {
            url.set_query(Some("page=1"));
        }
        let pairs: Vec<_> = url.query_pairs().collect();
        if pairs.len() != 1
            || pairs[0].0 != "page"
            || !numeric(&pairs[0].1)
            || pairs[0].1.parse::<u32>().unwrap_or(0) == 0
        {
            bail!("INVALID_SOURCE_ROUTE");
        }
    } else if path != "/orders"
        || !url.path().strip_prefix("/orders/").is_some_and(numeric)
        || url.query().is_some()
    {
        bail!("INVALID_SOURCE_ROUTE");
    }
    Ok(url)
}
pub fn short(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(4000)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_deeplink_retains_only_canonical_file_and_variant_identity() {
        let canonical = observed_resolver("https://booth.pm/downloadables/700001?variation_id=800001").unwrap();
        assert_eq!(observed_resolver("https://booth.pm/downloadables/700001/deeplink?client=booth-library-manager&variation_id=800001&type=zip"), Some(canonical));
        assert!(resolver("https://booth.pm/downloadables/700001/deeplink?variation_id=800001").is_none());
        for url in [
            "https://booth.pm/downloadables/700001/deeplink/extra?variation_id=800001",
            "https://booth.pm/downloadables/700001/deeplink/deeplink?variation_id=800001",
            "https://booth.pm/downloadables/not-numeric/deeplink?variation_id=800001",
            "https://booth.pm/downloadables/700001/deeplink?variation_id=800001&variation_id=800002",
            "https://other.invalid/downloadables/700001/deeplink?variation_id=800001",
            "http://booth.pm/downloadables/700001/deeplink?variation_id=800001",
        ] {
            assert!(observed_resolver(url).is_none(), "{url}");
        }
    }
    #[test]
    fn free_item_queue_routes_are_canonical_and_separate_from_history_routes() {
        let route = free_item_route("900001").unwrap();
        assert_eq!(free_item_id(&route), Some("900001"));
        assert!(super::route(&route, "free").is_err());
        for route in [
            "https://booth.pm/items/900001",
            "https://synthetic.booth.pm/ja/items/900001",
            "https://booth.pm/ja/items/900001?token=x",
            "https://booth.pm/ja/items/900001/",
            "https://booth.pm/ja/items/%39",
            "https://booth.pm:443/ja/items/900001",
        ] {
            assert!(free_item_id(route).is_none());
        }
        assert!(free_item_route("not-numeric").is_err());
    }
    #[test]
    fn public_item_url_normalization_preserves_shop_host_and_explicit_locale() {
        for (input, expected) in [
            (
                "https://booth.pm/items/900001",
                "https://booth.pm/ja/items/900001",
            ),
            (
                "https://booth.pm/ja/items/900001",
                "https://booth.pm/ja/items/900001",
            ),
            (
                "https://booth.pm/en/items/900001",
                "https://booth.pm/en/items/900001",
            ),
            (
                "https://synthetic.booth.pm/items/900001",
                "https://synthetic.booth.pm/items/900001",
            ),
            (
                "https://synthetic.booth.pm/en/items/900001",
                "https://synthetic.booth.pm/en/items/900001",
            ),
        ] {
            let normalized = public_url(input, "item").unwrap();
            assert_eq!(normalized, expected);
            assert_eq!(item_id(&normalized).as_deref(), Some("900001"));
            assert_eq!(public_url(&normalized, "item").as_deref(), Some(expected));
        }
        for input in [
            "https://accounts.booth.pm/items/900001",
            "https://manage.booth.pm/items/900001",
            "https://booth.pm.evil.test/items/900001",
            "https://user@booth.pm/items/900001",
            "https://booth.pm:444/items/900001",
            "https://booth.pm/items/not-numeric",
            "https://booth.pm/downloadables/900001",
        ] {
            assert!(public_url(input, "item").is_none());
        }
        assert_eq!(
            public_url("https://synthetic.booth.pm/", "shop").as_deref(),
            Some("https://synthetic.booth.pm/")
        );
    }
    #[test]
    fn observed_product_identity_accepts_exact_first_party_canonical_paths() {
        assert_eq!(
            observed_product_url("https://booth.pm/items/900001").as_deref(),
            Some("https://booth.pm/ja/items/900001")
        );
        for locale in ["ja", "en"] {
            let url = format!("https://booth.pm/{locale}/items/900001");
            assert_eq!(observed_product_url(&url).as_deref(), Some(url.as_str()));
        }
        for url in [
            "https://synthetic.booth.pm/items/900001",
            "https://synthetic.booth.pm/ja/items/900001",
            "https://www.booth.pm/items/900001",
            "http://booth.pm/items/900001",
            "https://user@booth.pm/items/900001",
            "https://booth.pm:443/items/900001",
            "https://booth.pm/items/900001/",
            "https://booth.pm/items/900001?token=private",
            "https://booth.pm/items/900001#private",
            "https://booth.pm/ja/items/900001?language=en",
            "https://booth.pm/ignored/../items/900001",
            "https://booth.pm/items/%39%30%30%30%30%31",
            "https://booth.pm\\items\\900001",
            "https://booth.pm.evil.test/items/900001",
            "https://booth.pm/items/not-numeric",
            "https://booth.pm/unknown/items/900001",
        ] {
            assert!(observed_product_url(url).is_none());
        }
    }
    #[test]
    fn network_boundaries() {
        for value in [
            "http://booth.pm/downloadables/1",
            "https://booth.pm.evil.test/downloadables/1",
            "https://booth.pm/downloadables/1?token=secret",
            "https://user@booth.pm/downloadables/1",
            "https://booth.pm:444/downloadables/1",
        ] {
            assert!(resolver(value).is_none());
        }
        assert_eq!(
            resolver("https://booth.pm/downloadables/32?variation_id=8")
                .unwrap()
                .1
                .as_deref(),
            Some("8")
        );
        assert!(route("/orders/12", "purchase").is_ok());
        assert_eq!(
            route("/library", "library").unwrap().as_str(),
            "https://accounts.booth.pm/library?page=1"
        );
        assert!(route("/library?", "library").is_err());
        assert!(route("/orders/12/receipt", "purchase").is_err());
        assert!(route("https://evil.test/library?page=1", "library").is_err());
        assert!(route("/library?page=1&token=x", "library").is_err());
        assert!(!download_destination(
            &Url::parse("https://127.0.0.1/a").unwrap()
        ));
    }
}
