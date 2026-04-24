//! SEC EDGAR client for N-PORT filings.
//!
//! Used by the CLI (`backfill`, `nightly-append`) to:
//!
//! 1. List NPORT-P filings for a given trust CIK.
//! 2. Filter those filings to a target series ID.
//! 3. Download and parse each filing's `primary_doc.xml`.
//!
//! SEC requires a descriptive User-Agent on all `data.sec.gov` requests.
//! See <https://www.sec.gov/developer> for the policy. indexkit sets:
//!
//! `User-Agent: indexkit/1.0 (+https://github.com/userFRM/indexkit)`
//!
//! SEC rate limit is 10 req/s per IP. The fetchers in this module insert a
//! 100 ms sleep between calls for comfortable headroom.

use crate::cik::CikEntry;
use crate::error::{Error, Result};
use crate::nport::{parse_nport, NportFiling};
use crate::types::IndexId;
use std::time::Duration;
use tokio::time::sleep;

/// User-Agent used for all SEC requests. SEC mandates a contact email and
/// rejects anonymous or `noreply@` addresses at the Akamai edge.
///
/// Format: `application contact-email` -- exactly one space between the two.
/// Override at runtime via the `INDEXKIT_SEC_USER_AGENT` environment variable
/// (strongly recommended when running under CI -- set it to a reachable
/// email you control).
pub const SEC_USER_AGENT_DEFAULT: &str = "indexkit frederic.miesegaes@gmail.com";

/// Resolve the User-Agent to use for SEC requests, honouring
/// `INDEXKIT_SEC_USER_AGENT`.
pub fn resolved_sec_user_agent() -> String {
    std::env::var("INDEXKIT_SEC_USER_AGENT").unwrap_or_else(|_| SEC_USER_AGENT_DEFAULT.to_string())
}

/// Sleep between SEC requests -- 100 ms = 10 req/s = SEC's stated rate limit.
pub const INTER_REQUEST_DELAY: Duration = Duration::from_millis(120);

/// One NPORT-P filing reference (not the XML body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingRef {
    /// 18-char accession number with dashes (e.g. `"0001752724-24-043113"`).
    pub accession: String,
    /// ISO filing date (`YYYY-MM-DD`).
    pub filing_date: String,
    /// Reporting period end (`YYYY-MM-DD`) if available in submissions JSON.
    pub report_date: Option<String>,
    /// Primary document filename (usually `"primary_doc.xml"` directly, or a
    /// nested path like `"xslFormNPORT-P_X01/primary_doc.xml"`).
    pub primary_document: String,
}

/// Client for the SEC EDGAR submissions and archives APIs.
#[derive(Clone)]
pub struct SecClient {
    http: reqwest::Client,
}

impl SecClient {
    /// Create a new SEC client with the mandated User-Agent.
    ///
    /// Reads `INDEXKIT_SEC_USER_AGENT` from the environment; falls back to
    /// [`SEC_USER_AGENT_DEFAULT`].
    pub fn new() -> Result<Self> {
        let ua = resolved_sec_user_agent();
        let http = reqwest::Client::builder()
            .user_agent(ua)
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { http })
    }

    /// List NPORT-P filings for a given 10-digit zero-padded CIK.
    ///
    /// Pulls both the `filings.recent` page and any older archives linked
    /// from `filings.files[]`. Returns filings sorted by filing date
    /// descending (newest first).
    pub async fn list_nport_filings(&self, cik: &str) -> Result<Vec<FilingRef>> {
        let mut out: Vec<FilingRef> = Vec::new();

        // Recent filings page.
        let url = format!("https://data.sec.gov/submissions/CIK{cik}.json");
        let v: serde_json::Value = self.http_get_json(&url).await?;
        append_from_submissions(&v, &mut out, true);
        let older_files: Vec<String> = parse_older_archives(&v).into_iter().collect();

        // Older archives (paginated). SEC stores these at
        // https://data.sec.gov/submissions/{archive-file}.
        for fname in older_files {
            sleep(INTER_REQUEST_DELAY).await;
            let url = format!("https://data.sec.gov/submissions/{fname}");
            match self.http_get_json(&url).await {
                Ok(vv) => append_from_submissions(&vv, &mut out, false),
                Err(e) => tracing::warn!("archive {fname} fetch failed: {e}"),
            }
        }

        out.sort_by(|a, b| b.filing_date.cmp(&a.filing_date));
        Ok(out)
    }

    /// Download and parse a single filing's `primary_doc.xml`.
    pub async fn fetch_nport(&self, cik: &str, accession: &str) -> Result<NportFiling> {
        let cik_num = cik
            .trim_start_matches('0')
            .parse::<u64>()
            .map_err(|_| Error::Nport(format!("bad CIK {cik}")))?;
        let no_dash: String = accession.chars().filter(|c| *c != '-').collect();
        let url =
            format!("https://www.sec.gov/Archives/edgar/data/{cik_num}/{no_dash}/primary_doc.xml");

        let resp = self.http.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Nport(format!(
                "fetch {url}: HTTP {} {}",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or("")
            )));
        }
        let body = resp.bytes().await?;
        parse_nport(&body)
    }

    /// List filings for an index's trust and filter to those matching the
    /// trust's series ID.
    ///
    /// For multi-series trusts (iShares Trust hosts ~130 ETFs under one CIK)
    /// the full submissions feed is huge; indexkit uses the EDGAR full-text
    /// search API (`efts.sec.gov`) with the series ID as a query string to
    /// narrow candidates before downloading XML. Each accession still needs
    /// `primary_doc.xml` to confirm the series ID, because the search may
    /// return false positives where the series ID appears elsewhere in the
    /// document.
    ///
    /// For single-series trusts (DIA, QQQ) all filings are kept.
    pub async fn filings_for_series(
        &self,
        entry: &CikEntry,
    ) -> Result<Vec<(FilingRef, NportFiling)>> {
        // Candidate shortlist. For single-series trusts we use the full
        // submissions feed; for multi-series trusts we use EDGAR search by
        // seriesId which returns 20-100 candidates instead of thousands.
        let candidates: Vec<FilingRef> = match &entry.series_id {
            Some(sid) => self.search_filings_by_series(&entry.trust_cik, sid).await?,
            None => self.list_nport_filings(&entry.trust_cik).await?,
        };
        tracing::info!(
            cik = %entry.trust_cik,
            series = ?entry.series_id,
            candidates = candidates.len(),
            "series candidate shortlist"
        );

        let mut out = Vec::new();
        for f in &candidates {
            sleep(INTER_REQUEST_DELAY).await;
            match self.fetch_nport(&entry.trust_cik, &f.accession).await {
                Ok(nport) => {
                    let matches = match &entry.series_id {
                        Some(sid) => nport.header.series_id.as_deref() == Some(sid.as_str()),
                        None => true,
                    };
                    if matches {
                        out.push((f.clone(), nport));
                    }
                }
                Err(e) => tracing::warn!(
                    accession = %f.accession,
                    "skip: fetch/parse failed: {e}"
                ),
            }
        }
        Ok(out)
    }

    /// Backwards-compatible name used by existing call sites.
    pub async fn filter_to_series(
        &self,
        entry: &CikEntry,
        _filings: &[FilingRef],
    ) -> Result<Vec<(FilingRef, NportFiling)>> {
        self.filings_for_series(entry).await
    }

    /// Query EDGAR full-text search for NPORT-P filings from `cik` that
    /// mention the given `series_id`. Paginates through results (SEC returns
    /// up to 100 per page via `&from=` offset).
    pub async fn search_filings_by_series(
        &self,
        cik: &str,
        series_id: &str,
    ) -> Result<Vec<FilingRef>> {
        let mut out: Vec<FilingRef> = Vec::new();
        let mut seen = std::collections::BTreeSet::<String>::new();
        let mut from: u32 = 0;
        loop {
            sleep(INTER_REQUEST_DELAY).await;
            // EDGAR search requires the zero-padded 10-digit CIK.
            let url = format!(
                "https://efts.sec.gov/LATEST/search-index?q=%22{}%22&forms=NPORT-P&ciks={}&from={}",
                series_id, cik, from
            );
            let v: serde_json::Value = self.http_get_json(&url).await?;
            let hits = v.pointer("/hits/hits").and_then(|x| x.as_array());
            let Some(hits) = hits else { break };
            if hits.is_empty() {
                break;
            }
            let mut added = 0;
            for h in hits {
                let Some(src) = h.get("_source") else {
                    continue;
                };
                let Some(accession) = src.get("adsh").and_then(|x| x.as_str()) else {
                    continue;
                };
                if !seen.insert(accession.to_string()) {
                    continue;
                }
                let filing_date = src
                    .get("file_date")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let report_date = src
                    .get("period_ending")
                    .and_then(|x| x.as_str())
                    .map(str::to_string);
                out.push(FilingRef {
                    accession: accession.to_string(),
                    filing_date,
                    report_date,
                    primary_document: "primary_doc.xml".into(),
                });
                added += 1;
            }
            let total = v
                .pointer("/hits/total/value")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            from += hits.len() as u32;
            if added == 0 || from >= total as u32 {
                break;
            }
        }
        out.sort_by(|a, b| b.filing_date.cmp(&a.filing_date));
        Ok(out)
    }

    /// Convenience: for `IndexId`, list filings and return the series-filtered
    /// (filing, nport) pairs.
    pub async fn filings_for_index(&self, index: IndexId) -> Result<Vec<(FilingRef, NportFiling)>> {
        let entry = crate::cik::entry_for(index);
        self.filings_for_series(&entry).await
    }

    async fn http_get_json(&self, url: &str) -> Result<serde_json::Value> {
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Nport(format!(
                "SEC fetch {url}: HTTP {} {}",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or("")
            )));
        }
        let body = resp.bytes().await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

/// Extract NPORT-P filings from a submissions JSON value.
fn append_from_submissions(v: &serde_json::Value, out: &mut Vec<FilingRef>, use_recent_key: bool) {
    // Recent submissions live under `filings.recent`; older archives have
    // the fields at the top level.
    let base = if use_recent_key {
        v.pointer("/filings/recent")
    } else {
        Some(v)
    };
    let Some(r) = base else {
        return;
    };
    let forms = r
        .get("form")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let dates = r
        .get("filingDate")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let accs = r
        .get("accessionNumber")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let reports = r
        .get("reportDate")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let docs = r
        .get("primaryDocument")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();

    for i in 0..forms.len() {
        let Some(form) = forms.get(i).and_then(|x| x.as_str()) else {
            continue;
        };
        if form != "NPORT-P" {
            continue;
        }
        let accession = accs
            .get(i)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let filing_date = dates
            .get(i)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let report_date = reports.get(i).and_then(|x| x.as_str()).map(str::to_string);
        let primary_document = docs
            .get(i)
            .and_then(|x| x.as_str())
            .unwrap_or("primary_doc.xml")
            .to_string();
        if accession.is_empty() {
            continue;
        }
        out.push(FilingRef {
            accession,
            filing_date,
            report_date,
            primary_document,
        });
    }
}

fn parse_older_archives(v: &serde_json::Value) -> Vec<String> {
    v.pointer("/filings/files")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_from_recent() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "filings": {
                    "recent": {
                        "form": ["NPORT-P", "10-K", "NPORT-P"],
                        "filingDate": ["2024-06-01", "2024-05-01", "2024-07-01"],
                        "accessionNumber": ["0001-24-001", "0002-24-002", "0001-24-003"],
                        "reportDate": ["2024-04-30", "2023-12-31", "2024-05-31"],
                        "primaryDocument": ["primary_doc.xml", "10k.htm", "primary_doc.xml"]
                    }
                }
            }"#,
        )
        .unwrap();
        let mut out = Vec::new();
        append_from_submissions(&v, &mut out, true);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].filing_date, "2024-06-01");
        assert_eq!(out[1].filing_date, "2024-07-01");
    }

    #[test]
    fn parse_older_archives_extracts_names() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "filings": {
                    "files": [
                        {"name":"CIK0001100663-submissions-001.json","filingCount":999,"filingFrom":"2021-01-01","filingTo":"2021-12-31"},
                        {"name":"CIK0001100663-submissions-002.json","filingCount":999,"filingFrom":"2020-01-01","filingTo":"2020-12-31"}
                    ]
                }
            }"#,
        )
        .unwrap();
        let names = parse_older_archives(&v);
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], "CIK0001100663-submissions-001.json");
    }
}
