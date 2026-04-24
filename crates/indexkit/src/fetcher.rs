//! ETag-aware HTTP fetcher with retry, single-flight, CDN mirror fallback,
//! and SHA-256 manifest verification.
//!
//! # Cache layout
//!
//! ```text
//! $INDEXKIT_CACHE_DIR/                (default: XDG cache / indexkit)
//! |- sp500/
//! |  |- sp500-2024-01.parquet
//! |  |- sp500-2024-01.parquet.etag
//! |- ndx/
//! |  |- ...
//! ```
//!
//! # Fetch flow (per call)
//!
//! 1. Single-flight gate: if another task is already fetching this key,
//!    join the in-flight request rather than issuing a duplicate.
//! 2. Cache check: if a local file exists, send `If-None-Match` with the
//!    stored ETag.
//! 3. `304 Not Modified` -> return the cached bytes.
//! 4. `2xx` -> write body + ETag, return bytes.
//! 5. Retry-able error (5xx, 429, connect/timeout): exponential backoff up
//!    to 3 total attempts. Delays: 250 ms -> 750 ms -> 2 000 ms (capped).
//!    429 response: respect `Retry-After` header if present.
//! 6. On primary-URL exhaustion: try jsDelivr CDN mirror once.
//! 7. All transports failed but cache exists -> warn + return stale.
//! 8. All transports failed + no cache -> return `Err`.
//!
//! # SHA-256 verification
//!
//! If a `manifest.json` entry exists for the key, the fetched bytes are
//! checked against the stored digest. A mismatch returns
//! `Error::ChecksumMismatch` and the corrupt bytes are NOT written to cache.

use bytes::Bytes;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OnceCell};

use crate::error::{Error, Result};

/// Maximum total attempts (initial + 2 retries).
const MAX_ATTEMPTS: u32 = 3;
/// Base delay for exponential backoff.
const BACKOFF_BASE_MS: u64 = 250;
/// Cap on backoff delay.
const BACKOFF_MAX_MS: u64 = 2_000;

/// An in-flight or completed fetch. Stored in the single-flight map while
/// a key is being fetched; the value is an error message if the fetch failed.
type InflightCell = Arc<OnceCell<std::result::Result<Bytes, String>>>;

/// ETag-aware fetcher with retry, single-flight deduplication, CDN mirror
/// fallback, and SHA-256 manifest verification.
pub(crate) struct CachedFetcher {
    pub http: reqwest::Client,
    /// Primary origin URL (e.g. `raw.githubusercontent.com/.../data`).
    pub base_url: String,
    /// CDN mirror base URL (or `None` to disable).
    pub mirror_url: Option<String>,
    pub cache_dir: PathBuf,
    inflight: Arc<Mutex<HashMap<String, InflightCell>>>,
    manifest: Arc<Mutex<Option<HashMap<String, String>>>>,
}

impl CachedFetcher {
    pub fn new(http: reqwest::Client, base_url: String, cache_dir: PathBuf) -> Self {
        let mirror_url = Some(
            std::env::var("INDEXKIT_MIRROR_URL").unwrap_or_else(|_| DEFAULT_MIRROR_URL.to_string()),
        );
        Self {
            http,
            base_url,
            mirror_url,
            cache_dir,
            inflight: Arc::new(Mutex::new(HashMap::new())),
            manifest: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn set_base_url(&mut self, url: String) {
        self.base_url = url;
    }

    pub(crate) fn set_mirror_url(&mut self, url: Option<String>) {
        self.mirror_url = url;
    }

    pub(crate) fn set_cache_dir(&mut self, dir: PathBuf) {
        self.cache_dir = dir;
    }

    /// Fetch a parquet file by logical key, where `key` is the relative path
    /// under `data/`, without the `.parquet` suffix (e.g. `"sp500/sp500-2024-01"`).
    pub async fn fetch(&self, key: &str) -> Result<Bytes> {
        let cell: InflightCell = {
            let mut map = self.inflight.lock().await;
            map.entry(key.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let key_owned = key.to_string();
        let result = cell
            .get_or_init(|| async {
                match self.do_fetch(&key_owned).await {
                    Ok(b) => Ok(b),
                    Err(e) => Err(e.to_string()),
                }
            })
            .await;

        {
            let mut map = self.inflight.lock().await;
            map.remove(key);
        }

        result
            .clone()
            .map_err(|e| Error::Other(format!("fetch {key}: {e}")))
    }

    async fn do_fetch(&self, key: &str) -> Result<Bytes> {
        let cache_path = self.cache_dir.join(format!("{key}.parquet"));
        let etag_path = self.cache_dir.join(format!("{key}.parquet.etag"));

        match self
            .fetch_with_retry(key, &self.base_url.clone(), &cache_path, &etag_path)
            .await
        {
            Ok(bytes) => {
                return self
                    .verify_and_return(key, bytes, &cache_path, &etag_path)
                    .await
            }
            Err(primary_err) => {
                if let Some(mirror) = &self.mirror_url {
                    tracing::warn!(
                        key,
                        error = %primary_err,
                        "primary fetch exhausted retries, trying CDN mirror"
                    );
                    match self.fetch_single(key, &mirror.clone()).await {
                        Ok(bytes) => {
                            if let Err(e) = tokio::fs::create_dir_all(
                                cache_path.parent().unwrap_or(Path::new(".")),
                            )
                            .await
                            {
                                tracing::warn!("could not create cache dir: {e}");
                            } else if let Err(e) = tokio::fs::write(&cache_path, &bytes).await {
                                tracing::warn!("could not write mirror response to cache: {e}");
                            }
                            return self
                                .verify_and_return(key, bytes, &cache_path, &etag_path)
                                .await;
                        }
                        Err(mirror_err) => {
                            tracing::warn!(
                                key,
                                mirror_error = %mirror_err,
                                "CDN mirror also failed"
                            );
                        }
                    }
                } else {
                    tracing::debug!(key, "mirror fallback disabled, returning primary error");
                }
                if cache_path.exists() {
                    tracing::warn!(key, "all transports failed, serving stale cache");
                    let bytes = tokio::fs::read(&cache_path).await?;
                    return Ok(bytes.into());
                }
                Err(primary_err)
            }
        }
    }

    async fn fetch_with_retry(
        &self,
        key: &str,
        base: &str,
        cache_path: &Path,
        etag_path: &Path,
    ) -> Result<Bytes> {
        let url = format!("{base}/{key}.parquet");
        let mut last_err: Option<Error> = None;

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                let delay_ms = backoff_delay_ms(attempt);
                tracing::debug!(key, attempt, delay_ms, "retry backoff");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            let mut req = self.http.get(&url);
            if cache_path.exists() {
                if let Some(etag) = read_etag(etag_path) {
                    req = req.header("If-None-Match", etag);
                }
            }

            match req.send().await {
                Ok(resp) if resp.status() == StatusCode::NOT_MODIFIED => {
                    let bytes = tokio::fs::read(cache_path).await?;
                    return Ok(bytes.into());
                }
                Ok(resp) if resp.status().is_success() => {
                    let etag = resp
                        .headers()
                        .get("etag")
                        .and_then(|v| v.to_str().ok())
                        .map(String::from);
                    let bytes = resp.bytes().await?;
                    tokio::fs::create_dir_all(cache_path.parent().unwrap_or(Path::new(".")))
                        .await?;
                    tokio::fs::write(cache_path, &bytes).await?;
                    if let Some(e) = etag {
                        tokio::fs::write(etag_path, e).await?;
                    }
                    return Ok(bytes);
                }
                Ok(resp) if resp.status() == StatusCode::TOO_MANY_REQUESTS => {
                    let delay = retry_after_delay(&resp)
                        .unwrap_or_else(|| Duration::from_millis(backoff_delay_ms(attempt + 1)));
                    tracing::warn!(
                        key,
                        attempt,
                        delay_secs = delay.as_secs_f32(),
                        "429 rate-limited"
                    );
                    if attempt + 1 < MAX_ATTEMPTS {
                        tokio::time::sleep(delay).await;
                        last_err =
                            Some(Error::Other(format!("fetch {key}: 429 Too Many Requests")));
                        continue;
                    }
                    return Err(Error::Other(format!(
                        "fetch {key}: 429 Too Many Requests (final)"
                    )));
                }
                Ok(resp) if should_retry_status(resp.status()) => {
                    last_err = Some(Error::Other(format!(
                        "fetch {key}: HTTP {} {}",
                        resp.status().as_u16(),
                        resp.status().canonical_reason().unwrap_or("")
                    )));
                }
                Ok(resp) => {
                    return Err(Error::Other(format!(
                        "fetch {key}: HTTP {} {}",
                        resp.status().as_u16(),
                        resp.status().canonical_reason().unwrap_or("")
                    )));
                }
                Err(e) if is_retriable_error(&e) => {
                    tracing::warn!(key, attempt, error = %e, "transient error, will retry");
                    last_err = Some(Error::Http(e));
                }
                Err(e) => {
                    last_err = Some(Error::Http(e));
                    break;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| Error::Other(format!("fetch {key}: all attempts failed"))))
    }

    async fn fetch_single(&self, key: &str, base: &str) -> Result<Bytes> {
        let url = format!("{base}/{key}.parquet");
        let resp = self.http.get(&url).send().await?;
        if resp.status().is_success() {
            Ok(resp.bytes().await?)
        } else {
            Err(Error::Other(format!(
                "mirror {key}: HTTP {} {}",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or("")
            )))
        }
    }

    async fn verify_and_return(
        &self,
        key: &str,
        bytes: Bytes,
        cache_path: &Path,
        etag_path: &Path,
    ) -> Result<Bytes> {
        let expected_hex = self.manifest_digest_for(key).await;
        if let Some(expected) = expected_hex {
            let actual = hex_sha256(&bytes);
            if actual != expected {
                let _ = tokio::fs::remove_file(cache_path).await;
                let _ = tokio::fs::remove_file(etag_path).await;
                return Err(Error::ChecksumMismatch {
                    file: format!("{key}.parquet"),
                    expected,
                    actual,
                });
            }
        }
        Ok(bytes)
    }

    async fn manifest_digest_for(&self, key: &str) -> Option<String> {
        let mut manifest_guard = self.manifest.lock().await;
        if manifest_guard.is_none() {
            let manifest_url = format!("{}/manifest.json", self.base_url);
            match self.http.get(&manifest_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<HashMap<String, String>>().await {
                        Ok(m) => {
                            *manifest_guard = Some(m);
                        }
                        Err(e) => {
                            tracing::warn!("manifest parse failed: {e}");
                            *manifest_guard = Some(HashMap::new());
                        }
                    }
                }
                _ => {
                    *manifest_guard = Some(HashMap::new());
                }
            }
        }

        manifest_guard
            .as_ref()?
            .get(&format!("{key}.parquet"))
            .and_then(|v| v.strip_prefix("sha256:").map(str::to_string))
    }
}

fn backoff_delay_ms(attempt: u32) -> u64 {
    let raw = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(10));
    raw.min(BACKOFF_MAX_MS)
}

fn should_retry_status(status: StatusCode) -> bool {
    status.is_server_error()
}

fn is_retriable_error(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}

fn retry_after_delay(resp: &reqwest::Response) -> Option<Duration> {
    let header = resp.headers().get("Retry-After")?;
    let val = header.to_str().ok()?;
    val.trim().parse::<u64>().ok().map(Duration::from_secs)
}

fn read_etag(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().filter(|s| !s.is_empty())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Resolve the cache directory.
///
/// Priority:
/// 1. `$INDEXKIT_CACHE_DIR` env var.
/// 2. XDG/platform cache dir for the `indexkit` application.
/// 3. Fallback: `~/.cache/indexkit`.
pub(crate) fn default_cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("INDEXKIT_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(proj) = directories::ProjectDirs::from("", "", "indexkit") {
        return proj.cache_dir().to_path_buf();
    }
    dirs_fallback()
}

fn dirs_fallback() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("indexkit").join("cache"))
            .unwrap_or_else(|_| PathBuf::from("indexkit-cache"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(".cache").join("indexkit"))
            .unwrap_or_else(|_| PathBuf::from(".indexkit-cache"))
    }
}

/// Default primary base URL (GitHub raw content).
pub(crate) const DEFAULT_BASE_URL: &str =
    "https://raw.githubusercontent.com/userFRM/indexkit/main/data";

/// Default CDN mirror (jsDelivr).
pub(crate) const DEFAULT_MIRROR_URL: &str =
    "https://cdn.jsdelivr.net/gh/userFRM/indexkit@main/data";

pub(crate) fn resolved_base_url() -> String {
    std::env::var("INDEXKIT_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_progression() {
        assert_eq!(backoff_delay_ms(0), 250);
        assert_eq!(backoff_delay_ms(1), 500);
        assert_eq!(backoff_delay_ms(2), 1000);
        assert_eq!(backoff_delay_ms(3), 2000);
        assert_eq!(backoff_delay_ms(10), 2000);
    }

    #[test]
    fn hex_sha256_known_value() {
        let digest = hex_sha256(b"");
        assert_eq!(
            digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn test_with_mirror_url_none_skips_fallback() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let primary = MockServer::start().await;
        let mirror_sentinel = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/sp500/sp500-2024-01.parquet"))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&primary)
            .await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"irrelevant"))
            .expect(0)
            .mount(&mirror_sentinel)
            .await;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let mut fetcher = CachedFetcher::new(http, primary.uri(), cache_dir.path().to_path_buf());
        fetcher.set_mirror_url(None);

        let result = fetcher.fetch("sp500/sp500-2024-01").await;
        assert!(
            result.is_err(),
            "primary 503 + no mirror must propagate error"
        );
    }

    #[tokio::test]
    async fn test_with_mirror_url_custom_used() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let primary = MockServer::start().await;
        let custom_mirror = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/sp500/sp500-2024-01.parquet"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&primary)
            .await;

        Mock::given(method("GET"))
            .and(path("/sp500/sp500-2024-01.parquet"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-parquet"))
            .expect(1)
            .mount(&custom_mirror)
            .await;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let cache_dir = tempfile::TempDir::new().unwrap();
        let mut fetcher = CachedFetcher::new(http, primary.uri(), cache_dir.path().to_path_buf());
        fetcher.set_mirror_url(Some(custom_mirror.uri()));

        let result = fetcher.fetch("sp500/sp500-2024-01").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_ref(), b"fake-parquet");
    }
}
