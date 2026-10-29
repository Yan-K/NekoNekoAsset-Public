use super::safety;
use anyhow::{bail, Result};
use reqwest::{Client, StatusCode};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

/// One listing and two detail requests across accounts, with shared request-start pacing.
#[derive(Clone)]
pub struct MetadataHttp {
    pace: Arc<Mutex<Instant>>,
    metadata: Arc<Semaphore>,
    listings: Arc<Semaphore>,
}
impl MetadataHttp {
    async fn wait_start(&self) {
        loop {
            let wait = {
                let mut next = self.pace.lock().await;
                let now = Instant::now();
                if *next <= now {
                    *next = now + start_spacing(uuid::Uuid::new_v4().as_bytes()[0]);
                    return;
                }
                *next
            };
            // Recheck after sleeping: another request's Retry-After can defer
            // waiters that have not started yet. No timestamp lock spans I/O.
            tokio::time::sleep_until(tokio::time::Instant::from_std(wait)).await;
        }
    }
    async fn defer(&self, delay: Duration) {
        let mut next = self.pace.lock().await;
        *next = (*next).max(Instant::now() + delay);
    }
    pub fn new() -> Self {
        Self {
            pace: Arc::new(Mutex::new(Instant::now())),
            metadata: Arc::new(Semaphore::new(2)),
            listings: Arc::new(Semaphore::new(1)),
        }
    }
    pub async fn resolve(&self, client: &Client, url: Url) -> Result<reqwest::Response> {
        if safety::resolver(url.as_str()).is_none() {
            bail!("INVALID_RESOLVER");
        }
        let _request = self
            .metadata
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))?;
        for attempt in 0..3 {
            self.wait_start().await;
            let response = match client
                .get(url.clone())
                .timeout(Duration::from_secs(45))
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    let code = transport_code(&error);
                    if code == "NETWORK_UNAVAILABLE" && attempt < 2 {
                        self.defer(Duration::from_secs(2u64.pow(attempt + 1))).await;
                        continue;
                    }
                    bail!(
                        "{}",
                        if code == "NETWORK_UNAVAILABLE" {
                            "DOWNLOAD_RESOLVER_FAILED"
                        } else {
                            code
                        }
                    );
                }
            };
            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
            {
                let wait = retry_after(
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    std::time::SystemTime::now(),
                );
                self.defer(wait.max(Duration::from_secs(2u64.pow(attempt + 1))))
                    .await;
                if attempt == 2 || wait > Duration::from_secs(120) {
                    bail!("RATE_LIMITED");
                }
                continue;
            }
            return Ok(response);
        }
        bail!("DOWNLOAD_RESOLVER_FAILED")
    }
    pub async fn get(&self, client: &Client, url: Url) -> Result<(StatusCode, String)> {
        if let Some(limit) = crate::verification::source_page_limit() {
            if listing_url(&url)
                && url.query_pairs().any(|(key, value)| {
                    key == "page" && value.parse::<usize>().is_ok_and(|page| page > limit)
                })
            {
                bail!("VERIFICATION_PAGE_LIMIT");
            }
        }
        if !safety::secure(&url) || !url.host_str().is_some_and(safety::booth_host) {
            bail!("INVALID_METADATA_URL");
        }
        let gate = if listing_url(&url) {
            &self.listings
        } else {
            &self.metadata
        };
        let _request = gate
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))?;
        for attempt in 0..3 {
            self.wait_start().await;
            crate::support_trace::request(&url, attempt);
            let response = match client.get(url.clone()).send().await {
                Ok(r) => r,
                Err(error) => {
                    let code = transport_code(&error);
                    crate::support_trace::transport(&url, if error.is_timeout() { "NETWORK_TIMEOUT" } else { code });
                    if code == "UNSAFE_REDIRECT" { crate::support_trace::redirect_rejected(&url); }
                    if code == "NETWORK_UNAVAILABLE" && attempt < 2 {
                        let wait = Duration::from_secs(2u64.pow(attempt + 1));
                        crate::support_trace::backoff(&url, wait);
                        self.defer(wait).await;
                        continue;
                    }
                    bail!("{}", code);
                }
            };
            crate::support_trace::response(&url, response.status().as_u16());
            if !safety::secure(response.url())
                || !response.url().host_str().is_some_and(safety::booth_host)
            {
                crate::support_trace::redirect_rejected(&url);
                bail!("UNSAFE_REDIRECT");
            }
            if response.url().path().contains("sign_in") || response.url().path().contains("login")
            {
                bail!("AUTH_REQUIRED");
            }
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                bail!("AUTH_REQUIRED");
            }
            if status == StatusCode::FORBIDDEN {
                bail!("PERMISSION_DENIED");
            }
            if let Err(error) = validate_source_response(&url, response.url()) {
                crate::support_trace::redirect_rejected(&url);
                return Err(error);
            }
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
            {
                let wait = retry_after(
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    std::time::SystemTime::now(),
                );
                let wait = wait.max(Duration::from_secs(2u64.pow(attempt + 1)));
                crate::support_trace::backoff(&url, wait);
                self.defer(wait).await;
                if attempt == 2 || wait > Duration::from_secs(120) {
                    bail!("RATE_LIMITED");
                }
                continue;
            }
            if status.is_server_error() && attempt < 2 {
                let wait = Duration::from_secs(2u64.pow(attempt + 1));
                crate::support_trace::backoff(&url, wait);
                self.defer(wait).await;
                continue;
            }
            if response
                .content_length()
                .is_some_and(|n| n > 8 * 1024 * 1024)
            {
                bail!("METADATA_TOO_LARGE");
            }
            let mut body = Vec::new();
            let mut response = response;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| {
                    crate::support_trace::transport(&url, if error.is_timeout() { "NETWORK_TIMEOUT" } else { "NETWORK_UNAVAILABLE" });
                    anyhow::anyhow!("NETWORK_UNAVAILABLE")
                })?
            {
                if body.len() + chunk.len() > 8 * 1024 * 1024 {
                    bail!("METADATA_TOO_LARGE");
                }
                body.extend_from_slice(&chunk);
            }
            return Ok((
                status,
                String::from_utf8(body).map_err(|_| anyhow::anyhow!("METADATA_ENCODING"))?,
            ));
        }
        bail!("NETWORK_UNAVAILABLE")
    }
}
// Symmetric rounding keeps the mean at 400ms for a uniformly random byte.
fn start_spacing(sample: u8) -> Duration {
    Duration::from_millis(350 + (u64::from(sample) * 100 + 127) / 255)
}
fn listing_url(url: &Url) -> bool {
    url.host_str() == Some("accounts.booth.pm")
        && matches!(
            url.path(),
            "/library" | "/library/gifts" | "/library/free_downloads" | "/orders"
        )
}
fn validate_source_response(requested: &Url, actual: &Url) -> Result<()> {
    if safety::free_item_id(requested.as_str()).is_some() {
        if requested != actual {
            bail!("RESPONSE_ROUTE_CHANGED");
        }
        return Ok(());
    }
    if requested.host_str() != Some("accounts.booth.pm") {
        return Ok(());
    }
    let source = match requested.path() {
        "/library" => "library",
        "/library/gifts" => "gift",
        "/library/free_downloads" => "free",
        "/orders" => "purchase",
        path if path.strip_prefix("/orders/").is_some_and(safety::numeric) => "purchase",
        _ => bail!("INVALID_SOURCE_ROUTE"),
    };
    let requested = safety::route(requested.as_str(), source)?;
    let actual = safety::route(actual.as_str(), source)
        .map_err(|_| anyhow::anyhow!("RESPONSE_ROUTE_CHANGED"))?;
    if requested != actual {
        bail!("RESPONSE_ROUTE_CHANGED");
    }
    Ok(())
}
fn retry_after(value: Option<&str>, now: std::time::SystemTime) -> Duration {
    value
        .and_then(|s| {
            s.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                chrono::DateTime::parse_from_rfc2822(s).ok().and_then(|d| {
                    let seconds = now.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
                    Some(Duration::from_secs(
                        d.timestamp().saturating_sub(seconds).max(0) as u64,
                    ))
                })
            })
        })
        // An unrepresentable header is invalid, just like malformed text.
        // defer() must be able to form its monotonic deadline before returning.
        .filter(|delay| Instant::now().checked_add(*delay).is_some())
        .unwrap_or(Duration::from_secs(5))
}
pub fn build_client(jar: Arc<reqwest::cookie::Jar>, download: bool) -> Result<Client> {
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 8 {
            return attempt.error("REDIRECT_LIMIT");
        }
        let valid = if download {
            safety::download_destination(attempt.url())
        } else {
            safety::secure(attempt.url())
                && attempt.url().host_str().is_some_and(safety::booth_host)
        };
        if valid {
            attempt.follow()
        } else if safety::auth_navigation(attempt.url())
            && attempt
                .url()
                .host_str()
                .is_some_and(|h| h == "pixiv.net" || h.ends_with(".pixiv.net"))
        {
            attempt.error("AUTH_REQUIRED")
        } else {
            attempt.error("UNSAFE_REDIRECT")
        }
    });
    let builder = Client::builder()
        .cookie_provider(jar)
        .redirect(policy)
        .connect_timeout(Duration::from_secs(20))
        .user_agent(concat!(
            "NekoNekoAsset/",
            env!("CARGO_PKG_VERSION"),
            " (Windows; private asset manager)"
        ));
    let builder = if download {
        builder.read_timeout(Duration::from_secs(60))
    } else {
        builder.timeout(Duration::from_secs(45))
    };
    builder
        .build()
        .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))
}
fn transport_code(error: &reqwest::Error) -> &'static str {
    // Inspect fixed sentinel codes in the local error chain; never return URL-bearing messages.
    use std::error::Error;
    let mut source = error.source();
    while let Some(error) = source {
        let message = error.to_string();
        if message == "AUTH_REQUIRED" {
            return "AUTH_REQUIRED";
        }
        if message == "UNSAFE_REDIRECT" || message == "REDIRECT_LIMIT" {
            return "UNSAFE_REDIRECT";
        }
        source = error.source();
    }
    "NETWORK_UNAVAILABLE"
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listing_and_detail_slots_are_bounded_separately() {
        let http = MetadataHttp::new();
        assert_eq!(http.listings.available_permits(), 1);
        assert_eq!(http.metadata.available_permits(), 2);
        for path in [
            "/library",
            "/library/gifts",
            "/library/free_downloads",
            "/orders",
        ] {
            assert!(listing_url(
                &Url::parse(&format!("https://accounts.booth.pm{path}?page=2")).unwrap()
            ));
        }
        for raw in [
            "https://accounts.booth.pm/orders/910001",
            "https://booth.pm/ja/items/900001",
            "https://booth.pm/ja/items/900001.json",
        ] {
            assert!(!listing_url(&Url::parse(raw).unwrap()));
        }
    }
    #[tokio::test]
    async fn pacing_waiters_obey_shared_start_interval_and_later_deferral() {
        let http = MetadataHttp::new();
        http.wait_start().await;
        let started = Instant::now();
        let waiter = http.wait_start();
        let defer = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            http.defer(Duration::from_millis(1100)).await;
        };
        tokio::join!(waiter, defer);
        assert!(started.elapsed() >= Duration::from_millis(1200));
        let next = Instant::now();
        http.wait_start().await;
        assert!(next.elapsed() >= Duration::from_millis(350));
    }
    #[test]
    fn start_spacing_jitter_has_bounded_exact_mean() {
        let intervals: Vec<_> = (0..=u8::MAX)
            .map(|sample| start_spacing(sample).as_millis())
            .collect();
        assert!(intervals.iter().all(|ms| (350..=450).contains(ms)));
        assert_eq!(intervals.first(), Some(&350));
        assert_eq!(intervals.last(), Some(&450));
        assert_eq!(intervals.iter().sum::<u128>(), 400 * 256);
        for sample in 0..=u8::MAX {
            assert_eq!(
                start_spacing(sample) + start_spacing(u8::MAX - sample),
                Duration::from_millis(800)
            );
        }
    }
    #[test]
    fn free_item_response_preserves_exact_public_origin_path_and_identity() {
        let requested = Url::parse("https://booth.pm/ja/items/900001").unwrap();
        assert!(validate_source_response(&requested, &requested).is_ok());
        for actual in [
            "https://booth.pm/ja/items/900002",
            "https://synthetic.booth.pm/ja/items/900001",
            "https://booth.pm/items/900001",
            "https://booth.pm/ja/items/900001?x=1",
            "https://booth.pm/ja/items/900001#x",
            "https://accounts.booth.pm/library/free_downloads?page=1",
        ] {
            assert_eq!(
                validate_source_response(&requested, &Url::parse(actual).unwrap())
                    .unwrap_err()
                    .to_string(),
                "RESPONSE_ROUTE_CHANGED"
            );
        }
    }
    #[test]
    fn source_response_keeps_requested_route_and_only_canonicalizes_page_one() {
        let url = |path: &str| Url::parse(&format!("https://accounts.booth.pm{path}")).unwrap();
        for (requested, actual) in [
            ("/library?page=1", "/library"),
            ("/library", "/library?page=1"),
            ("/orders?page=1", "/orders"),
            ("/orders/900001", "/orders/900001"),
        ] {
            assert!(validate_source_response(&url(requested), &url(actual)).is_ok());
        }
        for (requested, actual) in [
            ("/library?page=2", "/library?page=1"),
            ("/library?page=2", "/library"),
            ("/orders?page=1", "/orders/900001"),
            ("/orders/900001", "/orders?page=1"),
            ("/library?page=1", "/library/gifts?page=1"),
            ("/library?page=1", "/library?page=1&filter=active"),
        ] {
            assert_eq!(
                validate_source_response(&url(requested), &url(actual))
                    .unwrap_err()
                    .to_string(),
                "RESPONSE_ROUTE_CHANGED"
            );
        }
        assert!(validate_source_response(
            &url("/library?page=1"),
            &Url::parse("https://booth.pm/library?page=1").unwrap()
        )
        .is_err());
    }
    #[test]
    fn retry_after_obeys_dates_and_bounds() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1000);
        assert_eq!(retry_after(Some("90"), now), Duration::from_secs(90));
        assert_eq!(retry_after(Some("600"), now), Duration::from_secs(600));
        assert_eq!(retry_after(Some("18446744073709551615"), now), Duration::from_secs(5));
        let date = "Thu, 01 Jan 1970 00:17:10 GMT";
        assert_eq!(retry_after(Some(date), now), Duration::from_secs(30));
        assert!(retry_after(Some("999999"), now) > Duration::from_secs(120));
        assert_eq!(retry_after(Some("bad"), now), Duration::from_secs(5));
    }
}
