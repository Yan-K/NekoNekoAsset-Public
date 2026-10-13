//! Provider-scoped native metadata transport, shared by the two new adapters.
use super::StoreProvider;
use anyhow::{bail, ensure, Result};
use reqwest::{Client, Method, Response, StatusCode};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

pub fn metadata_destination(provider: StoreProvider, url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.host_str().is_some_and(|host| match provider {
            StoreProvider::Booth => host == "booth.pm" || host.ends_with(".booth.pm"),
            StoreProvider::Gumroad => host == "gumroad.com" || host.ends_with(".gumroad.com"),
            StoreProvider::Jinxxy => super::session::jinxxy_host(host),
        })
}

pub fn build_client(provider: StoreProvider, jar: Arc<reqwest::cookie::Jar>) -> Result<Client> {
    build_provider_client(provider, jar, false)
}

/// HEAD returns the source Location unchanged: its capability is signed for a later GET.
pub fn build_head_client(
    provider: StoreProvider,
    jar: Arc<reqwest::cookie::Jar>,
) -> Result<Client> {
    build_provider_client(provider, jar, true)
}

fn build_provider_client(
    provider: StoreProvider,
    jar: Arc<reqwest::cookie::Jar>,
    head: bool,
) -> Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if provider == StoreProvider::Jinxxy {
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            reqwest::header::HeaderValue::from_static("en-US"),
        );
    }
    Client::builder()
        .cookie_provider(jar)
        .default_headers(headers)
        .redirect(if head {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 8 {
                    return attempt.error("REDIRECT_LIMIT");
                }
                let start = attempt.previous().first();
                let download = start.is_some_and(|url| {
                    !metadata_destination(provider, url)
                        || (provider == StoreProvider::Gumroad
                            && (url.path().starts_with("/r/")
                                || url.host_str() == Some("files.gumroad.com")))
                });
                if if download {
                    if provider == StoreProvider::Gumroad {
                        gumroad_file_destination(attempt.url())
                    } else {
                        resolver_destination(provider, attempt.url())
                    }
                } else {
                    metadata_destination(provider, attempt.url())
                } {
                    attempt.follow()
                } else {
                    attempt.error("UNSAFE_REDIRECT")
                }
            })
        })
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(45))
        .user_agent(concat!(
            "NekoNekoAsset/",
            env!("CARGO_PKG_VERSION"),
            " (private asset manager)"
        ))
        .build()
        .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))
}

/// This exact first-party file host is the observed destination of bound Gumroad controls.
pub fn gumroad_file_destination(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("files.gumroad.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
        && url.path() != "/"
}

pub fn resolver_destination(provider: StoreProvider, url: &Url) -> bool {
    if metadata_destination(provider, url) {
        return true;
    }
    if provider == StoreProvider::Jinxxy
        && url.scheme() == "https"
        && url.host_str()
            == Some("jinxxy-products.62bb56dad0a3633d04ee066263e8507d.r2.cloudflarestorage.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
        && url.path() != "/"
    {
        return true;
    }
    if provider != StoreProvider::Jinxxy
        || url.scheme() != "https"
        || url.host_str() != Some("api.jinxxy.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let parts: Vec<_> = url.path().split('/').collect();
    let id = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    parts.len() == 7
        && parts[1..4] == ["v1", "downloads", "inventory"]
        && parts[5] == "files"
        && id(parts[4])
        && id(parts[6])
}

fn jinxxy_graphql_url(origin: &Url) -> Result<Url> {
    ensure!(
        metadata_destination(StoreProvider::Jinxxy, origin)
            && origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none(),
        "INVALID_METADATA_URL"
    );
    origin
        .join("graphql")
        .map_err(|_| anyhow::anyhow!("INVALID_METADATA_URL"))
}

#[derive(Clone)]
pub struct MetadataHttp {
    provider: StoreProvider,
    pace: Arc<Mutex<Instant>>,
    metadata: Arc<Semaphore>,
    metrics: Arc<RequestMetrics>,
    classified: Arc<[RequestMetrics; 2]>,
    #[cfg(test)]
    authored_origin: Option<Url>,
}
#[derive(Default)]
struct RequestMetrics {
    requests: AtomicU64,
    wait_ms: AtomicU64,
    response_ms: AtomicU64,
    body_ms: AtomicU64,
    retries: AtomicU64,
    rate_limits: AtomicU64,
    server_errors: AtomicU64,
    transport_errors: AtomicU64,
    client_errors: AtomicU64,
}
#[derive(Clone, Copy)]
pub enum RequestKind {
    Content,
    Public,
}
impl RequestMetrics {
    fn snapshot(&self) -> [u64; 9] {
        [
            &self.requests,
            &self.wait_ms,
            &self.response_ms,
            &self.body_ms,
            &self.retries,
            &self.rate_limits,
            &self.server_errors,
            &self.transport_errors,
            &self.client_errors,
        ]
        .map(|counter| counter.load(Ordering::Relaxed))
    }
}

impl MetadataHttp {
    pub fn new(provider: StoreProvider) -> Self {
        Self {
            provider,
            pace: Arc::new(Mutex::new(Instant::now())),
            metadata: Arc::new(Semaphore::new(2)),
            metrics: Arc::new(RequestMetrics::default()),
            classified: Arc::new(std::array::from_fn(|_| RequestMetrics::default())),
            #[cfg(test)]
            authored_origin: None,
        }
    }
    pub fn classified_metrics(&self) -> [[u64; 9]; 2] {
        self.classified.each_ref().map(|metrics| metrics.snapshot())
    }
    pub fn classified_since(&self, before: [[u64; 9]; 2]) -> serde_json::Value {
        let after = self.classified_metrics();
        let names = [
            "requests",
            "pacingWaitMs",
            "responseWaitMs",
            "bodyReadMs",
            "retries",
            "rateLimits",
            "serverErrors",
            "transportErrors",
            "clientErrors",
        ];
        let mut result = serde_json::Map::new();
        for (i, label) in ["content", "public"].iter().enumerate() {
            let fields: serde_json::Map<_, _> = names
                .iter()
                .enumerate()
                .map(|(field, name)| {
                    (
                        (*name).into(),
                        serde_json::json!(after[i][field].saturating_sub(before[i][field])),
                    )
                })
                .collect();
            result.insert((*label).into(), serde_json::Value::Object(fields));
        }
        serde_json::Value::Object(result)
    }
    fn record(&self, kind: Option<RequestKind>, update: impl Fn(&RequestMetrics)) {
        update(&self.metrics);
        if let Some(kind) = kind {
            update(&self.classified[kind as usize]);
        }
    }
    pub fn metrics(&self) -> [u64; 3] {
        [
            self.metrics.requests.load(Ordering::Relaxed),
            self.metrics.wait_ms.load(Ordering::Relaxed),
            self.metrics.response_ms.load(Ordering::Relaxed),
        ]
    }

    pub(super) fn start_spacing_ms(&self) -> u64 {
        if self.provider == StoreProvider::Gumroad {
            500
        } else {
            1000
        }
    }

    async fn wait_start(&self) {
        loop {
            let at = {
                let mut next = self.pace.lock().await;
                let now = Instant::now();
                if *next <= now {
                    *next = now + Duration::from_millis(self.start_spacing_ms());
                    return;
                }
                *next
            };
            // A concurrent response may extend Retry-After while this caller waits.
            tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
        }
    }

    async fn defer(&self, wait: Duration) {
        let mut next = self.pace.lock().await;
        *next = (*next).max(Instant::now() + wait);
    }

    async fn request(
        &self,
        client: &Client,
        url: Url,
        resolver: bool,
        method: Method,
        body: Option<&serde_json::Value>,
        kind: Option<RequestKind>,
    ) -> Result<Response> {
        let destination = |url: &Url| {
            #[cfg(test)]
            if self
                .authored_origin
                .as_ref()
                .is_some_and(|origin| url.origin() == origin.origin())
            {
                return true;
            }
            if resolver {
                resolver_destination(self.provider, url)
            } else {
                metadata_destination(self.provider, url)
            }
        };
        ensure!(destination(&url), "INVALID_METADATA_URL");
        for attempt in 0..3u32 {
            let waiting = Instant::now();
            self.wait_start().await;
            let wait_ms = waiting.elapsed().as_millis() as u64;
            self.record(kind, |metrics| {
                metrics.wait_ms.fetch_add(wait_ms, Ordering::Relaxed);
                metrics.requests.fetch_add(1, Ordering::Relaxed);
                if attempt > 0 {
                    metrics.retries.fetch_add(1, Ordering::Relaxed);
                }
            });
            let started = Instant::now();
            let mut request = client.request(method.clone(), url.clone());
            if let Some(body) = body {
                request = request.json(body);
            }
            let sent = request.send().await;
            let response_ms = started.elapsed().as_millis() as u64;
            self.record(kind, |metrics| {
                metrics
                    .response_ms
                    .fetch_add(response_ms, Ordering::Relaxed);
                if sent.is_err() {
                    metrics.transport_errors.fetch_add(1, Ordering::Relaxed);
                }
            });
            let response = match sent {
                Ok(response) => response,
                Err(error) if error.is_redirect() => bail!("UNSAFE_REDIRECT"),
                Err(_) if attempt < 2 => {
                    self.defer(Duration::from_secs(1 << (attempt + 1))).await;
                    continue;
                }
                Err(_) => bail!("NETWORK_UNAVAILABLE"),
            };
            self.record(kind, |metrics| {
                if response.status() == StatusCode::TOO_MANY_REQUESTS {
                    metrics.rate_limits.fetch_add(1, Ordering::Relaxed);
                }
                if response.status().is_server_error() {
                    metrics.server_errors.fetch_add(1, Ordering::Relaxed);
                }
                if response.status().is_client_error() {
                    metrics.client_errors.fetch_add(1, Ordering::Relaxed);
                }
            });
            ensure!(destination(response.url()), "UNSAFE_REDIRECT");
            if matches!(response.url().path(), "/login" | "/two-factor" | "/sign_in")
                || response.status() == StatusCode::UNAUTHORIZED
            {
                bail!("AUTH_REQUIRED");
            }
            if response.status() == StatusCode::FORBIDDEN {
                bail!("PERMISSION_DENIED");
            }
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                || response.status().is_server_error()
            {
                let wait = retry_after(
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok()),
                );
                if attempt == 2 || wait > Duration::from_secs(120) {
                    if response.status() == StatusCode::TOO_MANY_REQUESTS {
                        bail!("RATE_LIMITED");
                    }
                    bail!("NETWORK_UNAVAILABLE");
                }
                self.defer(wait.max(Duration::from_secs(1 << (attempt + 1))))
                    .await;
                continue;
            }
            return Ok(response);
        }
        bail!("NETWORK_UNAVAILABLE")
    }

    /// Two bounded bodies can overlap; all starts share provider pacing and backoff.
    pub async fn get(&self, client: &Client, url: Url) -> Result<(StatusCode, String)> {
        let (status, body, _) = self.get_with_url(client, url).await?;
        Ok((status, body))
    }

    pub async fn get_with_url(
        &self,
        client: &Client,
        url: Url,
    ) -> Result<(StatusCode, String, Url)> {
        self.get_kind(client, url, None).await
    }
    pub async fn get_classified(
        &self,
        client: &Client,
        url: Url,
        kind: RequestKind,
    ) -> Result<(StatusCode, String)> {
        let (status, body, _) = self.get_kind(client, url, Some(kind)).await?;
        Ok((status, body))
    }
    async fn get_kind(
        &self,
        client: &Client,
        url: Url,
        kind: Option<RequestKind>,
    ) -> Result<(StatusCode, String, Url)> {
        let _permit = self
            .metadata
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))?;
        let response = self
            .request(client, url, false, Method::GET, None, kind)
            .await?;
        let final_url = response.url().clone();
        let started = Instant::now();
        let read = Self::read_metadata(response).await;
        let body_ms = started.elapsed().as_millis() as u64;
        self.record(kind, |metrics| {
            metrics.body_ms.fetch_add(body_ms, Ordering::Relaxed);
        });
        let (status, body) = read?;
        Ok((status, body, final_url))
    }

    /// The one buyer download-capability mutation; no arbitrary service POST surface.
    pub async fn post_json(
        &self,
        client: &Client,
        origin: &Url,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        ensure!(
            self.provider == StoreProvider::Jinxxy,
            "INVALID_METADATA_URL"
        );
        let _permit = self
            .metadata
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))?;
        let url = jinxxy_graphql_url(origin)?;
        let response = self
            .request(client, url.clone(), false, Method::POST, Some(body), None)
            .await?;
        ensure!(response.url() == &url, "UNSAFE_REDIRECT");
        let (status, body) = Self::read_metadata(response).await?;
        ensure!(status.is_success(), "JINXXY_DOWNLOAD_UNAVAILABLE");
        serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("JINXXY_DOWNLOAD_UNPROVEN"))
    }

    async fn read_metadata(mut response: Response) -> Result<(StatusCode, String)> {
        let status = response.status();
        ensure!(
            response
                .content_length()
                .is_none_or(|bytes| bytes <= 8 * 1024 * 1024),
            "METADATA_TOO_LARGE"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("NETWORK_UNAVAILABLE"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                "METADATA_TOO_LARGE"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok((
            status,
            String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("METADATA_ENCODING"))?,
        ))
    }

    /// Header-only capability resolution uses the same provider request-start spacing.
    pub async fn resolve_head(&self, client: &Client, url: Url) -> Result<Response> {
        ensure!(
            metadata_destination(self.provider, &url)
                || (self.provider == StoreProvider::Jinxxy
                    && url.host_str() == Some("api.jinxxy.com")
                    && resolver_destination(self.provider, &url)),
            "INVALID_METADATA_URL"
        );
        let _permit = self
            .metadata
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP_CLIENT_FAILED"))?;
        self.request(client, url, true, Method::HEAD, None, None)
            .await
    }
}

fn retry_after(value: Option<&str>) -> Duration {
    value
        .and_then(|value| {
            value
                .parse::<u64>()
                .ok()
                .map(Duration::from_secs)
                .or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(value).ok().map(|at| {
                        Duration::from_secs(
                            (at.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64,
                        )
                    })
                })
        })
        .unwrap_or(Duration::from_secs(5))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn authored_gumroad_http_measures_serial_and_full_product_pipeline() {
        use futures_util::StreamExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut bytes = [0; 4096];
                    socket.read(&mut bytes).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(3750)).await;
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await
                        .unwrap();
                });
            }
        });
        let client = Client::builder().no_proxy().build().unwrap();
        let mut measurements = Vec::new();
        for parallel in [false, true] {
            let mut transport = MetadataHttp::new(StoreProvider::Gumroad);
            transport.authored_origin = Some(origin.clone());
            if !parallel {
                transport.metadata = Arc::new(Semaphore::new(1));
            }
            let began = Instant::now();
            if parallel {
                let mut products = futures_util::stream::iter(0..4)
                    .map(|id| {
                        let transport = &transport;
                        let client = &client;
                        let origin = &origin;
                        async move {
                            transport
                                .get_classified(
                                    client,
                                    origin.join(&format!("content/{id}")).unwrap(),
                                    RequestKind::Content,
                                )
                                .await
                                .unwrap();
                            transport
                                .get_classified(
                                    client,
                                    origin.join(&format!("public/{id}")).unwrap(),
                                    RequestKind::Public,
                                )
                                .await
                                .unwrap();
                            id
                        }
                    })
                    .buffered(2);
                while products.next().await.is_some() {}
            } else {
                for id in 0..4 {
                    transport
                        .get_classified(
                            &client,
                            origin.join(&format!("content/{id}")).unwrap(),
                            RequestKind::Content,
                        )
                        .await
                        .unwrap();
                    transport
                        .get_classified(
                            &client,
                            origin.join(&format!("public/{id}")).unwrap(),
                            RequestKind::Public,
                        )
                        .await
                        .unwrap();
                }
            }
            let metrics = transport.metrics();
            let by_kind = transport.classified_since([[0; 9]; 2]);
            assert_eq!(by_kind["content"]["requests"], 4);
            assert_eq!(by_kind["public"]["requests"], 4);
            assert_eq!(by_kind["content"]["retries"], 0);
            assert_eq!(by_kind["public"]["retries"], 0);
            measurements.push(serde_json::json!({"parallel":parallel,"fullProductPipeline":parallel,"products":4,"requests":metrics[0],"requestsPerProduct":metrics[0]/4,"wallMs":began.elapsed().as_millis(),"pacingWaitMs":metrics[1],"responseWaitMs":metrics[2],"byKind":by_kind,"startSpacingMs":transport.start_spacing_ms(),"authoredResponseDelayMs":3750}));
            assert_eq!(metrics[0], 8);
        }
        server.abort();
        assert!(
            measurements[1]["wallMs"].as_u64().unwrap()
                < measurements[0]["wallMs"].as_u64().unwrap()
        );
        if let Ok(path) = std::env::var("NNA_STORE_EVIDENCE_DIR") {
            std::fs::write(
                std::path::Path::new(&path).join("gumroad-full-pipeline-timing.json"),
                serde_json::to_vec_pretty(&measurements).unwrap(),
            )
            .unwrap();
        }
    }
    #[tokio::test]
    async fn authored_public_retry_counts_distinguish_default_backoff_from_response_latency() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                socket.read(&mut bytes).await.unwrap();
                let response = if attempt < 2 {
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".as_slice()
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                        .as_slice()
                };
                socket.write_all(response).await.unwrap();
            }
        });
        let client = Client::builder().no_proxy().build().unwrap();
        let mut transport = MetadataHttp::new(StoreProvider::Gumroad);
        transport.authored_origin = Some(origin.clone());
        let began = Instant::now();
        transport
            .get_classified(&client, origin, RequestKind::Public)
            .await
            .unwrap();
        server.await.unwrap();
        let metrics = transport.classified_since([[0; 9]; 2]);
        assert_eq!(metrics["public"]["requests"], 3);
        assert_eq!(metrics["public"]["retries"], 2);
        assert_eq!(metrics["public"]["serverErrors"], 2);
        assert_eq!(metrics["content"]["requests"], 0);
        assert_eq!(retry_after(None), Duration::from_secs(5));
        if let Ok(path) = std::env::var("NNA_STORE_EVIDENCE_DIR") {
            std::fs::write(std::path::Path::new(&path).join("gumroad-retry-counts.json"),serde_json::to_vec_pretty(&serde_json::json!({"wallMs":began.elapsed().as_millis(),"byKind":metrics,"authoredResponses":[503,503,200],"retryAfterHeader":false})).unwrap()).unwrap();
        }
    }
    #[test]
    fn metadata_transport_rejects_other_provider_and_lookalike_origins() {
        let url = |value| Url::parse(value).unwrap();
        assert!(metadata_destination(
            StoreProvider::Gumroad,
            &url("https://authored.gumroad.com/l/fixture.json")
        ));
        assert!(metadata_destination(
            StoreProvider::Jinxxy,
            &url("https://zh.jinxxy.com/my/inventory")
        ));
        for host in super::super::session::JINXXY_HOSTS {
            assert!(metadata_destination(
                StoreProvider::Jinxxy,
                &Url::parse(&format!("https://{host}/my/inventory")).unwrap()
            ));
        }
        assert_eq!(
            jinxxy_graphql_url(&url("https://zh.jinxxy.com/")).unwrap(),
            url("https://zh.jinxxy.com/graphql")
        );
        for value in [
            "https://other.jinxxy.com/",
            "https://gumroad.com/",
            "https://zh.jinxxy.com/my/inventory",
            "https://zh.jinxxy.com/?redirect=authored",
        ] {
            assert!(jinxxy_graphql_url(&url(value)).is_err());
        }
        for value in [
            "https://zh.jinxxy.com.evil.test/my/inventory",
            "https://other.jinxxy.com/my/inventory",
            "http://zh.jinxxy.com/login",
            "https://secret@zh.jinxxy.com/login",
            "https://zh.jinxxy.com:8443/login",
        ] {
            assert!(!metadata_destination(StoreProvider::Jinxxy, &url(value)));
        }
        assert!(!metadata_destination(
            StoreProvider::Gumroad,
            &url("https://zh.jinxxy.com/my/inventory")
        ));
        for value in [
            "https://jinxxy.com/my/lists",
            "https://gumroad.com.evil.test/library",
            "https://gumroad.com:8443/library",
            "https://secret@gumroad.com/library",
        ] {
            assert!(!metadata_destination(StoreProvider::Gumroad, &url(value)));
        }
        let signed = url("https://jinxxy-products.62bb56dad0a3633d04ee066263e8507d.r2.cloudflarestorage.com/authored.zip?authored-signature=value");
        assert!(gumroad_file_destination(&url(
            "https://files.gumroad.com/authored/file.html?authored-capability=value"
        )));
        for value in [
            "https://gumroad.com/authored.zip",
            "https://other.gumroad.com/authored.zip",
            "https://files.gumroad.com.evil.test/authored.zip",
            "http://files.gumroad.com/authored.zip",
            "https://files.gumroad.com/",
            "https://secret@files.gumroad.com/authored.zip",
        ] {
            assert!(!gumroad_file_destination(&url(value)));
        }
        assert!(resolver_destination(StoreProvider::Jinxxy, &signed));
        assert!(!metadata_destination(StoreProvider::Jinxxy, &signed));
        assert!(!resolver_destination(StoreProvider::Gumroad, &signed));
        for value in ["http://jinxxy-products.62bb56dad0a3633d04ee066263e8507d.r2.cloudflarestorage.com/authored.zip", "https://other.r2.cloudflarestorage.com/authored.zip", "https://jinxxy-products.62bb56dad0a3633d04ee066263e8507d.r2.cloudflarestorage.com.evil.test/authored.zip"] {
            assert!(!resolver_destination(StoreProvider::Jinxxy, &url(value)));
        }
    }
}
