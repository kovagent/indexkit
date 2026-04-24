//! Sponsor-CDN holdings-file parsers (iShares, Invesco, SPDR) and the
//! Internet Archive Wayback Machine bridge.
//!
//! # Why CDN + Wayback in addition to SEC N-PORT?
//!
//! SEC N-PORT gives us a guaranteed-public monthly baseline, but ETF
//! sponsors publish daily holdings on their own sites. Combining the three
//! produces near-daily resolution back to Nov 2019 and true T+1 going
//! forward.
//!
//! Source priority at the coalesce layer:
//! `Cdn (3) > Wayback (2) > Nport (1)`
//!
//! # Legal posture
//!
//! - **Sponsor CDN fetches**: each sponsor's terms of service should be
//!   reviewed before running live CDN polls. This module exposes the
//!   fetchers; it is the caller's responsibility to ensure their use
//!   complies with the sponsor's terms. The default `indexkit-cli
//!   daily-fetch` command requires the `--accept-sponsor-tos` flag to
//!   run.
//! - **Wayback Machine** (`web.archive.org`): the Internet Archive is a
//!   501(c)(3) archival service operating under fair-use doctrine; its
//!   public CDX + snapshot APIs are designed for automated access.
//! - **SEC EDGAR**: federal records in the public domain.

use crate::error::{Error, Result};
use crate::types::{Constituent, DataSource, IndexId};
use chrono::NaiveDate;
use std::time::Duration;

/// Default User-Agent for sponsor-CDN fetches. Sponsors sometimes ToS-limit
/// automated access; indexkit identifies itself clearly so traffic is not
/// mistaken for a malicious bot.
pub const SPONSOR_USER_AGENT: &str = "indexkit/1.0 (+https://github.com/userFRM/indexkit)";

/// URL for the live sponsor-CDN holdings file of an ETF proxy.
///
/// Returns `None` for indices where we do not have a sponsor CDN endpoint
/// (all five are supported in v1.0 but the list is provided for forward
/// compatibility).
pub fn sponsor_url(index: IndexId) -> Option<(DataSource, &'static str, &'static str)> {
    match index {
        IndexId::Sp500 => Some((
            DataSource::IsharesCdn,
            "IVV",
            "https://www.ishares.com/us/products/239726/ishares-core-sp-500-etf/1467271812596.ajax?fileType=csv&fileName=IVV_holdings&dataType=fund",
        )),
        IndexId::Sp400 => Some((
            DataSource::IsharesCdn,
            "IJH",
            "https://www.ishares.com/us/products/239763/ishares-core-sp-midcap-etf/1467271812596.ajax?fileType=csv&fileName=IJH_holdings&dataType=fund",
        )),
        IndexId::Sp600 => Some((
            DataSource::IsharesCdn,
            "IJR",
            "https://www.ishares.com/us/products/239774/ishares-core-sp-smallcap-etf/1467271812596.ajax?fileType=csv&fileName=IJR_holdings&dataType=fund",
        )),
        IndexId::Ndx => Some((
            DataSource::InvescoCdn,
            "QQQ",
            "https://www.invesco.com/us/financial-products/etfs/holdings/main/holdings/0?audienceType=Investor&action=download&ticker=QQQ",
        )),
        IndexId::Dji => Some((
            DataSource::SpdrCdn,
            "DIA",
            "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-dia.xlsx",
        )),
    }
}

/// Client for sponsor-CDN holdings files.
#[derive(Clone)]
pub struct SponsorClient {
    http: reqwest::Client,
}

impl SponsorClient {
    /// New client with the default indexkit User-Agent.
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(SPONSOR_USER_AGENT)
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { http })
    }

    /// Fetch today's sponsor-CDN holdings as raw bytes.
    ///
    /// Returns [`Error::Nport`] (repurposed) if the fetch fails or the
    /// index has no sponsor URL.
    pub async fn fetch_today(&self, index: IndexId) -> Result<(DataSource, bytes::Bytes)> {
        let (src, _ticker, url) = sponsor_url(index)
            .ok_or_else(|| Error::Other(format!("no sponsor url for {index}")))?;
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!(
                "sponsor fetch {url}: HTTP {} {}",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or("")
            )));
        }
        Ok((src, resp.bytes().await?))
    }
}

/// Parse an iShares CSV holdings file into [`Constituent`]s.
///
/// iShares files have a ~9-line preamble with trust metadata before the
/// header row. The header appears when a line starts with `"Ticker"`.
/// Returns an empty vec if the header is not found.
///
/// Dates in iShares CSVs are reported in the preamble as `"Fund Holdings
/// as of","MMM DD, YYYY"`. If not found, `as_of_fallback` is used.
pub fn parse_ishares_csv(
    csv: &str,
    as_of_fallback: NaiveDate,
    source: DataSource,
) -> Result<Vec<Constituent>> {
    let mut as_of = as_of_fallback;
    // Preamble scan for the date and header.
    let mut lines = csv.lines().peekable();
    let mut header_idx: Option<Vec<String>> = None;
    for line in &mut lines {
        // Heuristic: header row in iShares CSVs starts with "Ticker".
        if line.starts_with('"') && line.contains("Ticker") && line.contains("Name") {
            header_idx = Some(parse_csv_row(line));
            break;
        }
        if let Some(ds) = extract_ishares_date(line) {
            as_of = ds;
        }
    }
    let Some(header) = header_idx else {
        return Ok(Vec::new());
    };

    let idx = |want: &str| header.iter().position(|h| h.eq_ignore_ascii_case(want));

    let ticker_i = idx("Ticker");
    let name_i = idx("Name");
    let cusip_i = idx("CUSIP");
    let isin_i = idx("ISIN");
    let asset_i = idx("Asset Class");
    let shares_i = idx("Shares").or_else(|| idx("Quantity"));
    let weight_i = idx("Weight (%)")
        .or_else(|| idx("Weight(%)"))
        .or_else(|| idx("Weight"));
    let mv_i = idx("Market Value").or_else(|| idx("Notional Value"));
    let sedol_i = idx("SEDOL");

    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let row = parse_csv_row(line);
        if row.len() < header.len() {
            continue;
        }
        // Keep equity rows only.
        if let Some(ai) = asset_i {
            let v = row.get(ai).map(|s| s.as_str()).unwrap_or("");
            if !v.eq_ignore_ascii_case("Equity") {
                continue;
            }
        }
        let ticker = ticker_i.and_then(|i| row.get(i)).cloned();
        let name = name_i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
        let cusip = cusip_i
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_default();
        // Skip if no cusip AND no ISIN/SEDOL -- we can't join it.
        if cusip.is_empty() {
            let has_isin = isin_i
                .and_then(|i| row.get(i))
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let has_sedol = sedol_i
                .and_then(|i| row.get(i))
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if !has_isin && !has_sedol {
                continue;
            }
        }
        let shares = shares_i
            .and_then(|i| row.get(i))
            .and_then(|s| parse_number(s))
            .unwrap_or(0.0);
        let weight_pct = weight_i
            .and_then(|i| row.get(i))
            .and_then(|s| parse_number(s))
            .unwrap_or(0.0);
        // iShares reports weights as percents (e.g. 7.12), not fractions.
        let weight = weight_pct / 100.0;
        let mv = mv_i
            .and_then(|i| row.get(i))
            .and_then(|s| parse_number(s))
            .unwrap_or(0.0);

        if name.is_empty() && cusip.is_empty() {
            continue;
        }
        out.push(Constituent {
            ticker: ticker.filter(|s| !s.is_empty() && s != "-"),
            name,
            cusip,
            lei: None,
            shares,
            market_value_usd: mv,
            weight,
            issuer_cik: None,
            sector: None,
            as_of,
            source: source.clone(),
        });
    }
    out.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// Parse an Invesco CSV holdings file (QQQ format).
///
/// Invesco QQQ holdings CSVs have columns such as
/// `Holdings Ticker, Holdings Name, Weight, Shares/Par Value, Market Value,
/// Notional Value, Sector`. Date typically appears in a `Date` column.
pub fn parse_invesco_csv(csv: &str, as_of_fallback: NaiveDate) -> Result<Vec<Constituent>> {
    let mut lines = csv.lines();
    let Some(header_line) = lines.next() else {
        return Ok(Vec::new());
    };
    let header = parse_csv_row(header_line);
    let idx = |want: &str| {
        header
            .iter()
            .position(|h| h.eq_ignore_ascii_case(want.trim()))
    };
    let ticker_i = idx("Holdings Ticker").or_else(|| idx("Ticker"));
    let name_i = idx("Name")
        .or_else(|| idx("Holdings Name"))
        .or_else(|| idx("Security Name"));
    let weight_i = idx("Weight")
        .or_else(|| idx("% of Fund"))
        .or_else(|| idx("% Weight"));
    let shares_i = idx("Shares/Par Value").or_else(|| idx("Shares"));
    let mv_i = idx("Market Value").or_else(|| idx("Holdings Market Value"));
    let date_i = idx("Date").or_else(|| idx("As of Date"));
    let cusip_i = idx("CUSIP");
    let isin_i = idx("ISIN");

    let mut out = Vec::new();
    let mut as_of = as_of_fallback;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let row = parse_csv_row(line);
        if row.len() < header.len() {
            continue;
        }
        if let Some(di) = date_i {
            if let Some(s) = row.get(di) {
                if let Some(d) = parse_invesco_date(s) {
                    as_of = d;
                }
            }
        }
        let ticker = ticker_i.and_then(|i| row.get(i)).cloned();
        let name = name_i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
        let cusip = cusip_i
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_default();
        if name.is_empty() && cusip.is_empty() {
            continue;
        }
        let weight_pct = weight_i
            .and_then(|i| row.get(i))
            .and_then(|s| parse_number(s))
            .unwrap_or(0.0);
        let weight = if weight_pct > 1.0 {
            weight_pct / 100.0
        } else {
            weight_pct
        };
        let shares = shares_i
            .and_then(|i| row.get(i))
            .and_then(|s| parse_number(s))
            .unwrap_or(0.0);
        let mv = mv_i
            .and_then(|i| row.get(i))
            .and_then(|s| parse_number(s))
            .unwrap_or(0.0);
        // Invesco often omits CUSIP for QQQ; keep rows anyway if ISIN/ticker present.
        if cusip.is_empty() {
            let has_id = ticker.as_deref().is_some_and(|s| !s.is_empty())
                || isin_i
                    .and_then(|i| row.get(i))
                    .is_some_and(|s| !s.is_empty());
            if !has_id {
                continue;
            }
        }
        out.push(Constituent {
            ticker: ticker.filter(|s| !s.is_empty() && s != "-"),
            name,
            cusip,
            lei: None,
            shares,
            market_value_usd: mv,
            weight,
            issuer_cik: None,
            sector: None,
            as_of,
            source: DataSource::InvescoCdn,
        });
    }
    out.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

// -- helpers --

fn extract_ishares_date(line: &str) -> Option<NaiveDate> {
    // Matches lines like: "Fund Holdings as of","Mar 15, 2024"
    let key = "Fund Holdings as of";
    let pos = line.find(key)?;
    let after = &line[pos + key.len()..];
    let s = after.trim_start_matches(['"', ',', ' ']);
    // Take the "Mar 15, 2024" segment up to the next double quote.
    let end = s.find('"').unwrap_or(s.len());
    NaiveDate::parse_from_str(s[..end].trim(), "%b %d, %Y").ok()
}

fn parse_invesco_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%m/%d/%Y")
        .or_else(|_| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d"))
        .ok()
}

fn parse_number(s: &str) -> Option<f64> {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, ',' | '$' | '%' | ' ' | '"'))
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned.eq_ignore_ascii_case("n/a") {
        return None;
    }
    cleaned.parse().ok()
}

/// Minimal CSV row splitter. Handles double-quoted fields with embedded commas.
fn parse_csv_row(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                // Escaped quote "" inside quoted field.
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => {
                in_quotes = true;
            }
            ',' if !in_quotes => {
                out.push(std::mem::take(&mut cur).trim().to_string());
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_csv_row_basic() {
        let r = parse_csv_row(r#"a,"b,c",d,"1,234.56""#);
        assert_eq!(r, vec!["a", "b,c", "d", "1,234.56"]);
    }

    #[test]
    fn parse_csv_row_escaped_quotes() {
        let r = parse_csv_row(r#""a ""b"" c",d"#);
        assert_eq!(r, vec![r#"a "b" c"#, "d"]);
    }

    #[test]
    fn parse_number_with_commas() {
        assert_eq!(parse_number("1,234.56"), Some(1234.56));
        assert_eq!(parse_number("$1,000"), Some(1000.0));
        assert_eq!(parse_number("7.12%"), Some(7.12));
        assert_eq!(parse_number("-"), None);
        assert_eq!(parse_number("N/A"), None);
    }

    #[test]
    fn parse_ishares_csv_minimal() {
        let csv = r#""Fund Holdings as of","Mar 15, 2024"
"iShares Core S&P 500 ETF"
"
"Ticker","Name","Sector","Asset Class","Market Value","Weight (%)","Price","Shares","CUSIP","ISIN","SEDOL","Exchange"
"AAPL","APPLE INC","IT","Equity","28900000000.00","7.12","182.41","158300000","037833100","US0378331005","2046251","NASDAQ"
"MSFT","MICROSOFT CORP","IT","Equity","19500000000.00","4.81","412.31","47300000","594918104","US5949181045","2588173","NASDAQ"
"#;
        let rows = parse_ishares_csv(
            csv,
            NaiveDate::from_ymd_opt(2024, 3, 1).unwrap(),
            DataSource::IsharesCdn,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
        assert_eq!(rows[0].cusip, "037833100");
        assert!((rows[0].weight - 0.0712).abs() < 1e-6);
        assert_eq!(rows[0].as_of, NaiveDate::from_ymd_opt(2024, 3, 15).unwrap());
        assert_eq!(rows[0].source, DataSource::IsharesCdn);
    }

    #[test]
    fn parse_invesco_csv_minimal() {
        let csv = r#"Fund Ticker,Security Identifier,Holdings Ticker,Name,Weight,Shares/Par Value,Market Value,Date
QQQ,037833100,AAPL,APPLE INC,7.12,158300000,28900000000,03/15/2024
QQQ,594918104,MSFT,MICROSOFT CORP,4.81,47300000,19500000000,03/15/2024
"#;
        let rows = parse_invesco_csv(csv, NaiveDate::from_ymd_opt(2024, 3, 1).unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
        assert!((rows[0].weight - 0.0712).abs() < 1e-6);
        assert_eq!(rows[0].as_of, NaiveDate::from_ymd_opt(2024, 3, 15).unwrap());
        assert_eq!(rows[0].source, DataSource::InvescoCdn);
    }

    #[test]
    fn sponsor_url_shape() {
        for id in IndexId::ALL {
            let url = sponsor_url(id);
            assert!(url.is_some(), "no sponsor url for {id}");
        }
    }
}
