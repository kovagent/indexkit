//! Internet Archive Wayback Machine client.
//!
//! Uses the public CDX index and snapshot APIs to recover archived copies
//! of sponsor-CDN holdings files for every trading day that the Archive
//! happens to have captured.
//!
//! # Endpoints
//!
//! 1. CDX index:
//!    `http://web.archive.org/cdx/search/cdx?url={u}&output=json&from={YYYYMMDD}&to={YYYYMMDD}`
//!    Returns a JSON array whose first row is the column header. Columns
//!    of interest: `timestamp`, `original`, `statuscode`, `mimetype`.
//!
//! 2. Snapshot (identity copy of the original bytes):
//!    `https://web.archive.org/web/{timestamp}id_/{original-url}`
//!    The `id_` modifier returns the unmodified response body -- no
//!    Wayback HTML wrapping.
//!
//! # Rate limits
//!
//! IA does not publish a strict rate limit but throttles aggressive
//! clients. indexkit sleeps 250 ms between CDX calls and 500 ms between
//! snapshot fetches.

use crate::error::{Error, Result};
use std::time::Duration;
use tokio::time::sleep;

/// User-Agent for Wayback requests.
pub const WAYBACK_USER_AGENT: &str = "indexkit/1.0 (+https://github.com/userFRM/indexkit)";

/// One CDX match row, slimmed down to fields we use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaybackMatch {
    /// `YYYYMMDDhhmmss` timestamp at which the Archive captured the URL.
    pub timestamp: String,
    /// Original URL as captured (usually the same as the query but
    /// sometimes a redirect target).
    pub original: String,
    /// HTTP status at capture time (`"200"`, `"404"`, etc.).
    pub statuscode: String,
    /// MIME type at capture time.
    pub mimetype: String,
}

impl WaybackMatch {
    /// Construct the identity-copy snapshot URL for this match.
    pub fn snapshot_url(&self) -> String {
        format!(
            "https://web.archive.org/web/{}id_/{}",
            self.timestamp, self.original
        )
    }

    /// Calendar date component of the timestamp (`"YYYY-MM-DD"`).
    pub fn date(&self) -> Option<chrono::NaiveDate> {
        if self.timestamp.len() < 8 {
            return None;
        }
        let y: i32 = self.timestamp[..4].parse().ok()?;
        let m: u32 = self.timestamp[4..6].parse().ok()?;
        let d: u32 = self.timestamp[6..8].parse().ok()?;
        chrono::NaiveDate::from_ymd_opt(y, m, d)
    }
}

/// Client for the Wayback Machine CDX + snapshot APIs.
#[derive(Clone)]
pub struct WaybackClient {
    http: reqwest::Client,
}

impl WaybackClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(WAYBACK_USER_AGENT)
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self { http })
    }

    /// Query the CDX index for snapshots of `url` between `from` and `to`
    /// (inclusive), both `YYYYMMDD`.
    ///
    /// Only `statuscode == "200"` matches are returned.
    pub async fn list(&self, url: &str, from: &str, to: &str) -> Result<Vec<WaybackMatch>> {
        let api = format!(
            "http://web.archive.org/cdx/search/cdx?url={}&output=json&from={}&to={}",
            urlencoding_encode(url),
            from,
            to,
        );
        let resp = self.http.get(&api).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!(
                "CDX {api}: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let text = resp.text().await?;
        parse_cdx_json(&text)
    }

    /// Fetch the identity-copy body of a single CDX match.
    pub async fn fetch(&self, m: &WaybackMatch) -> Result<bytes::Bytes> {
        sleep(Duration::from_millis(500)).await;
        let url = m.snapshot_url();
        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!(
                "wayback fetch {url}: HTTP {}",
                resp.status().as_u16()
            )));
        }
        Ok(resp.bytes().await?)
    }
}

/// Parse a CDX JSON response. Returns only `statuscode == "200"` matches.
pub fn parse_cdx_json(body: &str) -> Result<Vec<WaybackMatch>> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap_or(serde_json::Value::Null);
    let arr = match v {
        serde_json::Value::Array(a) => a,
        _ => return Ok(Vec::new()),
    };
    if arr.is_empty() {
        return Ok(Vec::new());
    }
    // First row is the header.
    let header: Vec<String> = arr[0]
        .as_array()
        .map(|hs| {
            hs.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let idx_of = |want: &str| header.iter().position(|h| h == want);
    let ts_i = idx_of("timestamp");
    let orig_i = idx_of("original");
    let status_i = idx_of("statuscode");
    let mime_i = idx_of("mimetype");

    let mut out = Vec::new();
    for row in arr.iter().skip(1) {
        let cells: Vec<&str> = row
            .as_array()
            .map(|cs| cs.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let ts = ts_i.and_then(|i| cells.get(i).copied()).unwrap_or("");
        let orig = orig_i.and_then(|i| cells.get(i).copied()).unwrap_or("");
        let status = status_i.and_then(|i| cells.get(i).copied()).unwrap_or("");
        let mime = mime_i.and_then(|i| cells.get(i).copied()).unwrap_or("");
        if status != "200" {
            continue;
        }
        out.push(WaybackMatch {
            timestamp: ts.into(),
            original: orig.into(),
            statuscode: status.into(),
            mimetype: mime.into(),
        });
    }
    Ok(out)
}

/// Minimal RFC 3986 percent-encoding for URL query parameters.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cdx_empty() {
        assert!(parse_cdx_json("[]").unwrap().is_empty());
        assert!(parse_cdx_json("").unwrap().is_empty());
    }

    #[test]
    fn parse_cdx_normal() {
        let j = r#"[
            ["urlkey","timestamp","original","mimetype","statuscode","digest","length"],
            ["com,ishares)/us/products","20240320120000","https://www.ishares.com/us/products/x","text/csv","200","abc","1234"],
            ["com,ishares)/us/products","20240421120000","https://www.ishares.com/us/products/x","text/csv","200","def","5678"],
            ["com,ishares)/us/products","20240421120010","https://www.ishares.com/us/products/x","text/html","404","","0"]
        ]"#;
        let matches = parse_cdx_json(j).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].timestamp, "20240320120000");
        assert_eq!(
            matches[0].date(),
            chrono::NaiveDate::from_ymd_opt(2024, 3, 20)
        );
        assert!(matches[0]
            .snapshot_url()
            .contains("/web/20240320120000id_/"));
    }

    #[test]
    fn url_encode_spaces() {
        assert_eq!(urlencoding_encode("a b"), "a%20b");
        assert_eq!(urlencoding_encode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
        assert_eq!(
            urlencoding_encode("abc-123_XYZ.tilde~"),
            "abc-123_XYZ.tilde~"
        );
    }
}
