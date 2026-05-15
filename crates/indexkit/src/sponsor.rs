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

/// Ordered list of sponsor-CDN endpoints for an ETF proxy index, ranked by
/// AUM (primary first, backups follow). [`SponsorClient::fetch_today`] walks
/// the list and returns the first successful 200, falling back to the next
/// entry on 4xx/5xx/network failure.
///
/// AUM ranking is approximate (late-2025 / early-2026 figures) and prefers
/// data-source robustness as a tie-breaker (clean XLSX/CSV endpoints over
/// JS-rendered HTML pages). VOO outranks SPY by AUM but is omitted because
/// Vanguard's holdings page has no clean machine-readable endpoint at the
/// time of writing.
///
/// | Index | Primary             | Backup                         |
/// |-------|---------------------|--------------------------------|
/// | SP500 | SPY (SSGA SPDR XLSX) | IVV (iShares CSV)             |
/// | SP400 | IJH (iShares CSV)   | MDY (SSGA SPDR XLSX)           |
/// | SP600 | IJR (iShares CSV)   | SLY (SSGA SPDR XLSX)           |
/// | NDX   | QQQ (Invesco JSON)  | QQQM (Invesco JSON, same Trust) |
/// | DJIA  | DIA (SSGA SPDR XLSX) | (none — no comparable second)  |
/// | RUT   | IWM (iShares CSV)   | (none — VTWO needs JS scraper) |
pub fn sponsor_urls(index: IndexId) -> Vec<(DataSource, &'static str, &'static str)> {
    match index {
        IndexId::Sp500 => vec![
            (
                DataSource::SpdrCdn,
                "SPY",
                "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-spy.xlsx",
            ),
            (
                DataSource::IsharesCdn,
                "IVV",
                "https://www.ishares.com/us/products/239726/ishares-core-sp-500-etf/1467271812596.ajax?fileType=csv&fileName=IVV_holdings&dataType=fund",
            ),
        ],
        IndexId::Sp400 => vec![
            (
                DataSource::IsharesCdn,
                "IJH",
                "https://www.ishares.com/us/products/239763/ishares-core-sp-midcap-etf/1467271812596.ajax?fileType=csv&fileName=IJH_holdings&dataType=fund",
            ),
            (
                DataSource::SpdrCdn,
                "MDY",
                "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-mdy.xlsx",
            ),
        ],
        IndexId::Sp600 => vec![
            (
                DataSource::IsharesCdn,
                "IJR",
                "https://www.ishares.com/us/products/239774/ishares-core-sp-smallcap-etf/1467271812596.ajax?fileType=csv&fileName=IJR_holdings&dataType=fund",
            ),
            (
                DataSource::SpdrCdn,
                "SLY",
                "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-sly.xlsx",
            ),
        ],
        IndexId::Ndx => vec![
            (
                DataSource::InvescoCdn,
                "QQQ",
                "https://www.invesco.com/us/financial-products/etfs/holdings/main/holdings/0?audienceType=Investor&action=download&ticker=QQQ",
            ),
            (
                DataSource::InvescoCdn,
                "QQQM",
                "https://www.invesco.com/us/financial-products/etfs/holdings/main/holdings/0?audienceType=Investor&action=download&ticker=QQQM",
            ),
        ],
        IndexId::Dji => vec![(
            DataSource::SpdrCdn,
            "DIA",
            "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-dia.xlsx",
        )],
        IndexId::Rut => vec![(
            DataSource::IsharesCdn,
            "IWM",
            "https://www.ishares.com/us/products/239710/ishares-russell-2000-etf/1467271812596.ajax?fileType=csv&fileName=IWM_holdings&dataType=fund",
        )],
    }
}

/// Primary sponsor-CDN endpoint (first entry of [`sponsor_urls`]).
///
/// Kept for backwards compatibility with v1.0 callers that only need the
/// AUM-ranked primary. New code should prefer [`sponsor_urls`] to enable
/// backup-fallback.
pub fn sponsor_url(index: IndexId) -> Option<(DataSource, &'static str, &'static str)> {
    sponsor_urls(index).into_iter().next()
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
    /// Walks [`sponsor_urls`] in AUM order: tries the primary first, falls
    /// back to each backup on network failure or non-2xx response. Returns
    /// the source tag and bytes of the first successful endpoint.
    ///
    /// Errors only when every endpoint fails or the index has no sponsor
    /// entries at all.
    pub async fn fetch_today(&self, index: IndexId) -> Result<(DataSource, bytes::Bytes)> {
        let endpoints = sponsor_urls(index);
        if endpoints.is_empty() {
            return Err(Error::Other(format!("no sponsor url for {index}")));
        }
        let mut last_err: Option<String> = None;
        for (src, ticker, url) in endpoints {
            match self.http.get(url).send().await {
                Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                    Ok(body) => return Ok((src, body)),
                    Err(e) => {
                        last_err = Some(format!("{ticker}: body read failed: {e}"));
                        tracing::warn!(%index, %ticker, "sponsor body read failed: {e}");
                    }
                },
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    let reason = resp.status().canonical_reason().unwrap_or("");
                    last_err = Some(format!("{ticker}: HTTP {code} {reason}"));
                    tracing::warn!(%index, %ticker, "sponsor fetch HTTP {code} {reason}, trying next");
                }
                Err(e) => {
                    last_err = Some(format!("{ticker}: {e}"));
                    tracing::warn!(%index, %ticker, "sponsor fetch network error: {e}, trying next");
                }
            }
        }
        Err(Error::Other(format!(
            "all sponsor endpoints failed for {index}: {}",
            last_err.unwrap_or_else(|| "unknown".into())
        )))
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
        // Header detection: anchor on the unambiguous co-occurrence of
        // `Ticker`, `Name`, and `Asset Class` -- these three strings appear
        // together only on the header row. Tolerates both the legacy
        // quoted shape (`"Ticker","Name",...`) and the bare shape
        // (`Ticker,Name,...`) iShares began emitting in late-2025.
        let trimmed = line.trim_start_matches('\u{feff}').trim_start();
        let cell0 = trimmed.trim_start_matches('"');
        if cell0.starts_with("Ticker") && line.contains("Name") && line.contains("Asset Class") {
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

/// Parse a State Street SPDR daily-holdings XLSX into [`Constituent`]
/// rows. Used for DIA (Dow Jones), MDY (Mid-Cap 400 backup), SPY
/// (S&P 500 backup), etc. State Street ships these as binary `.xlsx`
/// rather than CSV, so we rely on `calamine` to extract the cells.
///
/// Sheet layout (verified against `holdings-daily-us-en-dia.xlsx`):
/// - Row 1: fund name + "Daily" banner
/// - Row 2: empty
/// - Row 3: "Fund Name" / "DIA" or fund details
/// - Row 4: "As of MMM DD, YYYY" preamble carrying the as-of date
/// - Row 5: header row -- columns include `Ticker`, `Name`,
///   `Shares Held`, `Weight`, `Sector`, `Asset Class`
/// - Rows 6+: equity holdings; the sheet may include sub-totals,
///   cash rows, and "USD" pseudo-tickers that the equity filter drops
///
/// Numeric cells on the sheet sometimes arrive as strings with thousand
/// separators (`"1,234.56"`); the parser strips commas before parsing.
/// Empty/zero shares rows are kept (they appear when a name is being
/// removed end-of-day) so the diff layer can detect membership exits.
pub fn parse_spdr_xlsx(bytes: &[u8], as_of_fallback: NaiveDate) -> Result<Vec<Constituent>> {
    use calamine::{open_workbook_from_rs, Data, Reader, Xlsx};
    use std::io::Cursor;

    let cursor = Cursor::new(bytes.to_vec());
    let mut wb: Xlsx<_> =
        open_workbook_from_rs(cursor).map_err(|e| Error::Other(format!("xlsx open: {e}")))?;

    let sheet_names = wb.sheet_names();
    let first = sheet_names
        .first()
        .ok_or_else(|| Error::Other("xlsx has no sheets".into()))?
        .clone();
    let range = wb
        .worksheet_range(&first)
        .map_err(|e| Error::Other(format!("xlsx worksheet '{first}': {e}")))?;

    // Locate header row -- the row that contains a "Ticker" cell. SSGA
    // pads the preamble with a variable number of rows depending on the
    // fund (typically rows 1..=4), so scanning for the header is more
    // robust than hardcoding row 5.
    let mut header_row_idx: Option<usize> = None;
    let mut as_of_from_preamble: Option<NaiveDate> = None;
    for (row_idx, row) in range.rows().enumerate().take(20) {
        for cell in row {
            if let Data::String(s) = cell {
                let trimmed = s.trim();
                if trimmed.eq_ignore_ascii_case("Ticker") {
                    header_row_idx = Some(row_idx);
                    break;
                }
                if let Some(rest) = trimmed.strip_prefix("As of ") {
                    if let Ok(d) = NaiveDate::parse_from_str(rest.trim(), "%b %d, %Y")
                        .or_else(|_| NaiveDate::parse_from_str(rest.trim(), "%B %d, %Y"))
                        .or_else(|_| NaiveDate::parse_from_str(rest.trim(), "%d-%b-%Y"))
                    {
                        as_of_from_preamble = Some(d);
                    }
                }
            }
        }
        if header_row_idx.is_some() {
            break;
        }
    }
    let header_row_idx = header_row_idx
        .ok_or_else(|| Error::Other("SPDR xlsx: 'Ticker' header row not found".into()))?;
    let as_of = as_of_from_preamble.unwrap_or(as_of_fallback);

    // Map column header -> column index for the columns we care about.
    let header_row = range
        .rows()
        .nth(header_row_idx)
        .ok_or_else(|| Error::Other("xlsx header row missing".into()))?;
    let mut col_ticker: Option<usize> = None;
    let mut col_name: Option<usize> = None;
    let mut col_shares: Option<usize> = None;
    let mut col_weight: Option<usize> = None;
    let mut col_asset: Option<usize> = None;
    for (col_idx, cell) in header_row.iter().enumerate() {
        if let Data::String(s) = cell {
            match s.trim().to_ascii_lowercase().as_str() {
                "ticker" => col_ticker = Some(col_idx),
                "name" | "company" | "issuer name" => col_name = Some(col_idx),
                "shares held" | "shares" | "quantity" => col_shares = Some(col_idx),
                "weight" | "weight (%)" | "weighting" => col_weight = Some(col_idx),
                "asset class" | "type" => col_asset = Some(col_idx),
                _ => {}
            }
        }
    }
    let c_ticker =
        col_ticker.ok_or_else(|| Error::Other("SPDR xlsx: Ticker column missing".into()))?;
    let c_name = col_name.unwrap_or(c_ticker);
    let c_shares =
        col_shares.ok_or_else(|| Error::Other("SPDR xlsx: Shares column missing".into()))?;
    let c_weight =
        col_weight.ok_or_else(|| Error::Other("SPDR xlsx: Weight column missing".into()))?;

    let mut out: Vec<Constituent> = Vec::new();
    for (row_idx, row) in range.rows().enumerate().skip(header_row_idx + 1) {
        let _ = row_idx;
        let ticker = match row.get(c_ticker) {
            Some(Data::String(s)) => s.trim().to_string(),
            _ => continue,
        };
        if ticker.is_empty() || ticker == "-" {
            continue;
        }
        // Skip non-equity sub-totals / cash placeholders. SPDR DIA
        // currently lists "USD" with empty asset class; ignore it.
        if ticker.eq_ignore_ascii_case("USD") || ticker.eq_ignore_ascii_case("CASH") {
            continue;
        }
        if let Some(c) = col_asset {
            if let Some(Data::String(asset)) = row.get(c) {
                if !asset.eq_ignore_ascii_case("Equity")
                    && !asset.is_empty()
                    && !asset.eq_ignore_ascii_case("Common Stock")
                {
                    continue;
                }
            }
        }
        let name = match row.get(c_name) {
            Some(Data::String(s)) => s.trim().to_string(),
            _ => ticker.clone(),
        };
        let shares = cell_as_f64(row.get(c_shares)).unwrap_or(0.0);
        let weight_pct = cell_as_f64(row.get(c_weight)).unwrap_or(0.0);
        out.push(Constituent {
            ticker: Some(ticker),
            name,
            // SSGA's daily XLSX does not stamp the per-row CUSIP on this
            // sheet; downstream coalesce keys on `(identity, date)` and
            // accepts ticker as the identity when CUSIP is empty.
            cusip: String::new(),
            lei: None,
            shares,
            market_value_usd: 0.0,
            weight: weight_pct / 100.0,
            issuer_cik: None,
            sector: None,
            as_of,
            source: DataSource::SpdrCdn,
        });
    }
    if out.is_empty() {
        return Err(Error::Other(
            "SPDR xlsx: no equity rows parsed (sheet shape changed?)".into(),
        ));
    }
    Ok(out)
}

fn cell_as_f64(cell: Option<&calamine::Data>) -> Option<f64> {
    use calamine::Data;
    match cell? {
        Data::Float(f) => Some(*f),
        Data::Int(i) => Some(*i as f64),
        Data::String(s) => {
            let cleaned: String = s.chars().filter(|c| *c != ',' && *c != '%').collect();
            cleaned.trim().parse().ok()
        }
        _ => None,
    }
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
    fn parse_ishares_csv_bare_ticker_header() {
        // Late-2025 iShares shape: preamble + header row are NOT quoted.
        // Reproduces the IJH / IJR / IWM / IVV live shape verified
        // 2026-05-15 against the four live CDN endpoints.
        let csv = "\u{feff}iShares Core S&P Mid-Cap ETF\n\
Fund Holdings as of,\"May 14, 2026\"\n\
Inception Date,\"May 22, 2000\"\n\
Shares Outstanding,\"1,592,500,000.00\"\n\
Stock,\"-\"\n\
Bond,\"-\"\n\
Cash,\"-\"\n\
Other,\"-\"\n \n\
Ticker,Name,Sector,Asset Class,Market Value,Weight (%),Notional Value,Quantity,Price,Location,Exchange,Currency,FX Rate,Market Currency,Accrual Date,CUSIP,ISIN,SEDOL\n\
\"AAPL\",\"APPLE INC\",\"IT\",\"Equity\",\"28900000000.00\",\"7.12\",\"28900000000.00\",\"158300000\",\"182.41\",\"US\",\"NASDAQ\",\"USD\",\"1.00\",\"USD\",\"-\",\"037833100\",\"US0378331005\",\"2046251\"\n\
\"MSFT\",\"MICROSOFT CORP\",\"IT\",\"Equity\",\"19500000000.00\",\"4.81\",\"19500000000.00\",\"47300000\",\"412.31\",\"US\",\"NASDAQ\",\"USD\",\"1.00\",\"USD\",\"-\",\"594918104\",\"US5949181045\",\"2588173\"\n";
        let rows = parse_ishares_csv(
            csv,
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            DataSource::IsharesCdn,
        )
        .unwrap();
        assert_eq!(rows.len(), 2, "expected 2 rows, got {}", rows.len());
        assert_eq!(rows[0].ticker.as_deref(), Some("AAPL"));
        assert_eq!(rows[0].cusip, "037833100");
        assert!((rows[0].weight - 0.0712).abs() < 1e-6);
        assert_eq!(rows[0].as_of, NaiveDate::from_ymd_opt(2026, 5, 14).unwrap());
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

    #[test]
    fn sponsor_urls_aum_ranked_primary() {
        // SP500 primary must be SPY (SSGA SPDR XLSX) by AUM, with IVV backup.
        let sp500 = sponsor_urls(IndexId::Sp500);
        assert_eq!(sp500.len(), 2);
        assert_eq!(sp500[0].0, DataSource::SpdrCdn);
        assert_eq!(sp500[0].1, "SPY");
        assert!(sp500[0].2.ends_with("holdings-daily-us-en-spy.xlsx"));
        assert_eq!(sp500[1].0, DataSource::IsharesCdn);
        assert_eq!(sp500[1].1, "IVV");

        // SP400: IJH primary, MDY backup.
        let sp400 = sponsor_urls(IndexId::Sp400);
        assert_eq!(sp400.len(), 2);
        assert_eq!(sp400[0].1, "IJH");
        assert_eq!(sp400[1].1, "MDY");

        // SP600: IJR primary, SLY backup.
        let sp600 = sponsor_urls(IndexId::Sp600);
        assert_eq!(sp600.len(), 2);
        assert_eq!(sp600[0].1, "IJR");
        assert_eq!(sp600[1].1, "SLY");

        // NDX: QQQ primary, QQQM backup.
        let ndx = sponsor_urls(IndexId::Ndx);
        assert_eq!(ndx.len(), 2);
        assert_eq!(ndx[0].1, "QQQ");
        assert_eq!(ndx[1].1, "QQQM");

        // DJIA: DIA only.
        let dji = sponsor_urls(IndexId::Dji);
        assert_eq!(dji.len(), 1);
        assert_eq!(dji[0].1, "DIA");

        // RUT: IWM only.
        let rut = sponsor_urls(IndexId::Rut);
        assert_eq!(rut.len(), 1);
        assert_eq!(rut[0].1, "IWM");
    }

    #[test]
    fn sponsor_url_returns_first_of_sponsor_urls() {
        for id in IndexId::ALL {
            let single = sponsor_url(id).unwrap();
            let first = sponsor_urls(id).into_iter().next().unwrap();
            assert_eq!(single.1, first.1);
            assert_eq!(single.2, first.2);
        }
    }

    #[test]
    fn parse_spdr_xlsx_dia_sample() {
        // Real SPDR DIA daily-holdings XLSX (~19 KB, 30 equity rows +
        // ~3 cash/sub-total rows + preamble). Committed under
        // crates/indexkit/tests/fixtures/.
        let bytes = include_bytes!("../tests/fixtures/spdr_dia_sample.xlsx");
        let fallback = NaiveDate::from_ymd_opt(2026, 4, 28).unwrap();
        let rows = parse_spdr_xlsx(bytes, fallback).unwrap();
        // DJIA has exactly 30 constituents.
        assert_eq!(
            rows.len(),
            30,
            "expected 30 equity rows, got {}",
            rows.len()
        );
        for r in &rows {
            assert!(r.ticker.as_deref().map(|t| !t.is_empty()).unwrap_or(false));
            assert!(!r.name.is_empty());
            assert!(matches!(r.source, DataSource::SpdrCdn));
            // Weight is a fraction in [0, 1] not a percent.
            assert!(
                r.weight >= 0.0 && r.weight <= 1.0,
                "weight out of range: {}",
                r.weight
            );
        }
        // Weights sum to ~1.0 (allow ±5% slack for cash drag).
        let total: f64 = rows.iter().map(|r| r.weight).sum();
        assert!(
            total > 0.95 && total < 1.05,
            "weight sum out of band: {total}"
        );
    }

    #[test]
    fn parse_spdr_xlsx_filters_cash_pseudo_tickers() {
        // The DIA sample has a "USD" cash row in the trailing rows. The
        // parser must drop it.
        let bytes = include_bytes!("../tests/fixtures/spdr_dia_sample.xlsx");
        let fallback = NaiveDate::from_ymd_opt(2026, 4, 28).unwrap();
        let rows = parse_spdr_xlsx(bytes, fallback).unwrap();
        assert!(
            !rows.iter().any(|r| {
                r.ticker
                    .as_deref()
                    .map(|t| t.eq_ignore_ascii_case("USD") || t.eq_ignore_ascii_case("CASH"))
                    .unwrap_or(false)
            }),
            "USD/CASH pseudo-ticker leaked through filter"
        );
    }

    #[test]
    fn parse_spdr_xlsx_extracts_as_of_from_preamble() {
        use chrono::Datelike;
        // SSGA stamps "As of MMM DD, YYYY" in the preamble. Verify we
        // pick it up rather than falling back to the caller-supplied
        // date.
        let bytes = include_bytes!("../tests/fixtures/spdr_dia_sample.xlsx");
        // Use a clearly-wrong fallback so any hit on the fallback fails.
        let fallback = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
        let rows = parse_spdr_xlsx(bytes, fallback).unwrap();
        let first = rows.first().expect("non-empty");
        assert!(
            first.as_of.year() >= 2020,
            "as_of should come from preamble, got {}",
            first.as_of
        );
    }
}
