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
//! When sources overlap on a day, the coalesce layer keeps the row from the
//! source with the higher [`DataSource::priority`]: sponsor files, then
//! Wayback snapshots, then N-PORT.
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
pub const SPONSOR_USER_AGENT: &str = "indexkit/1.0 (+https://github.com/kovagent/indexkit)";

/// Ordered list of sponsor-CDN endpoints for an ETF proxy index, ranked by
/// AUM (primary first, backups follow). [`SponsorClient::fetch_today`] walks
/// the list and returns the first response that parses as a holdings file,
/// falling back to the next entry on a network failure, a non-2xx status, or
/// a 2xx body that is not holdings (sponsors answer some retired URLs with an
/// HTML page and status 200).
///
/// AUM ranking is approximate (late-2025 / early-2026 figures) and prefers
/// data-source robustness as a tie-breaker (clean XLSX/CSV endpoints over
/// JS-rendered HTML pages). VOO outranks SPY by AUM but is omitted because
/// Vanguard's holdings page has no clean machine-readable endpoint at the
/// time of writing.
///
/// | Index | Primary                 | Backup(s)                                       |
/// |-------|-------------------------|--------------------------------------------------|
/// | SP500 | SPY (SSGA SPDR XLSX)    | IVV (iShares CSV)                               |
/// | SP400 | IJH (iShares CSV)       | MDY (SSGA SPDR XLSX)                            |
/// | SP600 | IJR (iShares CSV)       | SPSM (SSGA SPDR XLSX)                           |
/// | NDX   | Nasdaq API (list-type)  | Invesco DNG QQQ JSON, then Invesco DNG QQQM JSON |
/// | DJIA  | DIA (SSGA SPDR XLSX)    | (none — no comparable second)                   |
/// | RUT   | IWM (iShares CSV)       | (none — VTWO needs JS scraper)                  |
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
                "https://www.ishares.com/us/products/239726/ishares-core-sp-500-etf/latest-holdings.csv",
            ),
        ],
        IndexId::Sp400 => vec![
            (
                DataSource::IsharesCdn,
                "IJH",
                "https://www.ishares.com/us/products/239763/ishares-core-sp-midcap-etf/latest-holdings.csv",
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
                "https://www.ishares.com/us/products/239774/ishares-core-sp-smallcap-etf/latest-holdings.csv",
            ),
            (
                DataSource::SpdrCdn,
                "SPSM",
                "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-spsm.xlsx",
            ),
        ],
        IndexId::Ndx => vec![
            // Nasdaq's own public list-type API -- official source of the
            // NDX constituent universe, free, unauthenticated, no geo-block.
            // Returns the full 100+ ticker universe with market cap and
            // last sale price. Primary because the legacy Invesco
            // download URL was retired in 2026-Q1 (HTTP 301 -> homepage).
            (
                DataSource::NasdaqApi,
                "NDX",
                "https://api.nasdaq.com/api/quote/list-type/nasdaq100",
            ),
            // Invesco DNG (Distribution Next-Gen) holdings JSON for QQQ
            // -- the endpoint Invesco's own QQQ product page calls to
            // render its all-holdings modal. Reachable from US egress;
            // EU edge currently returns HTTP 406 (geo-block). Without a
            // `loadType` it returns every holding; `loadType=initial` is
            // the page's first render and stops at the top ten.
            (
                DataSource::InvescoCdn,
                "QQQ",
                "https://dng-api.invesco.com/cache/v1/accounts/en_US/shareclasses/QQQ/holdings/fund?idType=ticker&interval=daily&productType=ETF",
            ),
            // Same DNG endpoint for QQQM (Invesco Nasdaq-100 ETF, sister
            // share-class, same underlying constituents).
            (
                DataSource::InvescoCdn,
                "QQQM",
                "https://dng-api.invesco.com/cache/v1/accounts/en_US/shareclasses/QQQM/holdings/fund?idType=ticker&interval=daily&productType=ETF",
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
            "https://www.ishares.com/us/products/239710/ishares-russell-2000-etf/latest-holdings.csv",
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
    /// back to each backup on a network failure, a non-2xx response, or a
    /// body that [`parse_holdings`] rejects. Returns the source tag and
    /// bytes of the first endpoint that served a holdings file.
    ///
    /// Errors only when every endpoint fails or the index has no sponsor
    /// entries at all.
    pub async fn fetch_today(&self, index: IndexId) -> Result<(DataSource, bytes::Bytes)> {
        let endpoints = sponsor_urls(index);
        if endpoints.is_empty() {
            return Err(Error::Other(format!("no sponsor url for {index}")));
        }
        self.fetch_first(index, &endpoints).await
    }

    async fn fetch_first(
        &self,
        index: IndexId,
        endpoints: &[(DataSource, &str, &str)],
    ) -> Result<(DataSource, bytes::Bytes)> {
        let today = chrono::Utc::now().date_naive();
        let mut last_err: Option<String> = None;
        for (src, ticker, url) in endpoints {
            let failure = match self.http.get(*url).send().await {
                Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                    Ok(body) => match parse_holdings(src, &body, today) {
                        Ok(_) => return Ok((src.clone(), body)),
                        Err(e) => format!("{ticker}: not a holdings file: {e}"),
                    },
                    Err(e) => format!("{ticker}: body read failed: {e}"),
                },
                Ok(resp) => format!(
                    "{ticker}: HTTP {} {}",
                    resp.status().as_u16(),
                    resp.status().canonical_reason().unwrap_or("")
                ),
                Err(e) => format!("{ticker}: {e}"),
            };
            tracing::warn!(%index, "sponsor fetch failed, trying next: {failure}");
            last_err = Some(failure);
        }
        Err(Error::Other(format!(
            "all sponsor endpoints failed for {index}: {}",
            last_err.unwrap_or_else(|| "unknown".into())
        )))
    }
}

/// Parse a sponsor holdings body with the parser for `source`.
///
/// Errors when the body is not that sponsor's holdings file or carries no
/// equity rows, so a caller can tell a wrong file from an empty index.
pub fn parse_holdings(
    source: &DataSource,
    body: &[u8],
    as_of_fallback: NaiveDate,
) -> Result<Vec<Constituent>> {
    let text = || {
        std::str::from_utf8(body)
            .map_err(|e| Error::Other(format!("{} body is not UTF-8: {e}", source.tag())))
    };
    let rows = match source {
        DataSource::IsharesCdn => parse_ishares_csv(text()?, as_of_fallback, source.clone())?,
        DataSource::InvescoCdn => parse_invesco_dng_json(body, as_of_fallback)?,
        DataSource::SpdrCdn => parse_spdr_xlsx(body, as_of_fallback)?,
        DataSource::NasdaqApi => parse_nasdaq_ndx_json(body, as_of_fallback)?,
        other => {
            return Err(Error::Other(format!(
                "{} is not a sponsor source",
                other.tag()
            )))
        }
    };
    if rows.is_empty() {
        return Err(Error::Other(format!(
            "{} holdings file has no equity rows",
            source.tag()
        )));
    }
    Ok(rows)
}

/// Parse an iShares CSV holdings file into [`Constituent`]s.
///
/// iShares files have a ~9-line preamble with trust metadata before the
/// header row. The header appears when a line starts with `"Ticker"`.
/// Errors if the header is not found, so a page served in place of the file
/// is not mistaken for an empty index.
///
/// The current `latest-holdings.csv` export carries no CUSIP, ISIN or SEDOL
/// column; its rows are keyed by ticker instead.
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
        return Err(Error::Other(
            "iShares csv: 'Ticker' header row not found".into(),
        ));
    };

    let idx = |want: &str| header.iter().position(|h| h.eq_ignore_ascii_case(want));

    let ticker_i = idx("Ticker");
    let name_i = idx("Name");
    let cusip_i = idx("CUSIP");
    let isin_i = idx("ISIN");
    let asset_i = idx("Asset Class");
    let type_i = idx("Type");
    let shares_i = idx("Shares").or_else(|| idx("Quantity"));
    let weight_i = idx("Weight (%)")
        .or_else(|| idx("Weight(%)"))
        .or_else(|| idx("Weight"))
        .or_else(|| idx("Market Weight"));
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
        // Where the file types each line, equity swaps and warrants also sit
        // under the `Equity` asset class and repeat a constituent's ticker.
        if let Some(ti) = type_i {
            let v = row.get(ti).map(|s| s.as_str()).unwrap_or("");
            if !v.eq_ignore_ascii_case("EQUITY") {
                continue;
            }
        }
        let ticker = ticker_i
            .and_then(|i| row.get(i))
            .filter(|s| !s.is_empty() && *s != "-")
            .cloned();
        let name = name_i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
        let cusip = cusip_i
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_default();
        // Skip a row with no identifier at all -- we can't join it.
        if cusip.is_empty() && ticker.is_none() {
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
            ticker,
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
/// Errors if the first line is not a header naming a ticker or name column.
pub fn parse_invesco_csv(csv: &str, as_of_fallback: NaiveDate) -> Result<Vec<Constituent>> {
    let mut lines = csv.lines();
    let header_line = lines.next().unwrap_or_default();
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
    if ticker_i.is_none() && name_i.is_none() {
        return Err(Error::Other("Invesco csv: header row not found".into()));
    }

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

/// Parse Invesco's holdings JSON (the `dng-api.invesco.com` feed behind its
/// product pages, used for QQQ / QQQM) into [`Constituent`] rows.
///
/// Response shape (verified 2026-09-23 against QQQ):
///
/// ```json
/// {
///   "effectiveDate": "2026-09-22",
///   "holdings": [
///     { "ticker": "NVDA", "issuerName": "NVIDIA Corp", "units": 180314828,
///       "percentageOfTotalNetAssets": 8.277194,
///       "securityTypeName": "Common Stock", "cusip": "67066G104" }
///   ]
/// }
/// ```
///
/// Common stock and depositary receipts are kept; currency, collateral,
/// futures and synthetic cash lines are dropped. Weights arrive in percent.
pub fn parse_invesco_dng_json(body: &[u8], as_of_fallback: NaiveDate) -> Result<Vec<Constituent>> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| Error::Other(format!("invesco json parse: {e}")))?;
    let as_of = v
        .get("effectiveDate")
        .and_then(|s| s.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or(as_of_fallback);
    let holdings = v
        .get("holdings")
        .and_then(|h| h.as_array())
        .ok_or_else(|| Error::Other("invesco json: missing holdings[]".into()))?;
    let str_field = |h: &serde_json::Value, k: &str| {
        h.get(k)
            .and_then(|s| s.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let mut out: Vec<Constituent> = holdings
        .iter()
        .filter(|h| {
            let kind = h
                .get("securityTypeName")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            kind == "Common Stock" || kind.starts_with("American Depository Receipt")
        })
        .map(|h| {
            let ticker = str_field(h, "ticker");
            Constituent {
                name: str_field(h, "issuerName"),
                ticker: (!ticker.is_empty()).then_some(ticker),
                cusip: str_field(h, "cusip"),
                lei: None,
                shares: h.get("units").and_then(|n| n.as_f64()).unwrap_or(0.0),
                market_value_usd: 0.0,
                weight: h
                    .get("percentageOfTotalNetAssets")
                    .and_then(|n| n.as_f64())
                    .unwrap_or(0.0)
                    / 100.0,
                issuer_cik: None,
                sector: None,
                as_of,
                source: DataSource::InvescoCdn,
            }
        })
        .collect();
    if out.is_empty() {
        return Err(Error::Other("invesco json: zero equity holdings".into()));
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

/// Parse Nasdaq's public list-type quote API response into
/// [`Constituent`] rows.
///
/// Endpoint: `https://api.nasdaq.com/api/quote/list-type/nasdaq100`
/// (or any other Nasdaq-published listid).
///
/// Response shape (verified 2026-05-15 against the live nasdaq100
/// listid):
///
/// ```json
/// {
///   "data": {
///     "totalrecords": 101,
///     "date": "May 15, 2026 10:30 AM",
///     "data": {
///       "headers": { "symbol": "Symbol", "companyName": "Name", ... },
///       "rows": [
///         {
///           "symbol": "AAPL",
///           "companyName": "Apple Inc. Common Stock",
///           "marketCap": "4,401,506,846,080",
///           "lastSalePrice": "$299.66",
///           ...
///         }
///       ]
///     }
///   }
/// }
/// ```
///
/// Per-row fields populated:
/// - `ticker` = `symbol`
/// - `name`   = `companyName` (with trailing " Common Stock" stripped)
/// - `shares` = 0.0 (Nasdaq's list-type response does not expose
///   shares-outstanding; downstream consumers can join against
///   `companyfacts` for that)
/// - `market_value_usd` = parsed `marketCap`
/// - `weight` = market-cap weight within the universe (mcap / Σmcap)
///
/// CUSIP / LEI / sector are not provided by this endpoint; downstream
/// `cusip-resolver` enriches them.
pub fn parse_nasdaq_ndx_json(body: &[u8], as_of_fallback: NaiveDate) -> Result<Vec<Constituent>> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| Error::Other(format!("nasdaq json parse: {e}")))?;
    let as_of = v
        .get("data")
        .and_then(|d| d.get("date"))
        .and_then(|s| s.as_str())
        .and_then(parse_nasdaq_date)
        .unwrap_or(as_of_fallback);

    let rows = v
        .get("data")
        .and_then(|d| d.get("data"))
        .and_then(|d| d.get("rows"))
        .and_then(|r| r.as_array())
        .ok_or_else(|| Error::Other("nasdaq json: missing data.data.rows[]".into()))?;

    // First pass: collect (symbol, name, mcap) so we can compute weights.
    let mut tmp: Vec<(String, String, f64)> = Vec::with_capacity(rows.len());
    for row in rows {
        let symbol = row
            .get("symbol")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(symbol) = symbol else { continue };
        let name = row
            .get("companyName")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim()
            .trim_end_matches(" Common Stock")
            .trim_end_matches(" Common Shares")
            .to_string();
        let mcap = row
            .get("marketCap")
            .and_then(|s| s.as_str())
            .and_then(parse_number)
            .unwrap_or(0.0);
        tmp.push((symbol.to_string(), name, mcap));
    }
    if tmp.is_empty() {
        return Err(Error::Other("nasdaq json: zero rows".into()));
    }

    let total_mcap: f64 = tmp.iter().map(|(_, _, m)| *m).sum();
    let mut out = Vec::with_capacity(tmp.len());
    for (symbol, name, mcap) in tmp {
        let weight = if total_mcap > 0.0 {
            mcap / total_mcap
        } else {
            0.0
        };
        out.push(Constituent {
            ticker: Some(symbol.clone()),
            name: if name.is_empty() { symbol } else { name },
            cusip: String::new(),
            lei: None,
            shares: 0.0,
            market_value_usd: mcap,
            weight,
            issuer_cik: None,
            sector: None,
            as_of,
            source: DataSource::NasdaqApi,
        });
    }
    out.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// Parse Nasdaq's `data.date` string (`"May 15, 2026 10:30 AM"`) into a
/// [`NaiveDate`]. Returns `None` on any parse failure.
fn parse_nasdaq_date(s: &str) -> Option<NaiveDate> {
    // Strip any trailing time component -- keep only the date portion.
    let head = s.split(" 1").next().unwrap_or(s);
    let head = head.split(" 0").next().unwrap_or(head);
    NaiveDate::parse_from_str(head.trim(), "%b %d, %Y")
        .or_else(|_| NaiveDate::parse_from_str(s.trim(), "%b %d, %Y %I:%M %p"))
        .or_else(|_| NaiveDate::parse_from_str(s.trim(), "%B %d, %Y"))
        .ok()
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

    /// The `latest-holdings.csv` export iShares serves now: no CUSIP, ISIN or
    /// SEDOL column, weight under `Market Weight`, and a `Type` column that
    /// separates the stock from equity swaps and warrants on the same ticker.
    /// Rows are the real IJR file's, trimmed.
    #[test]
    fn parse_ishares_latest_holdings_export() {
        let csv = "iShares Core S&P Small-Cap ETF\n\
Fund Holdings as of,\"Sep 21, 2026\"\n\
Inception Date,\"May 22, 2000\"\n\
\n\
Ticker,Name,Type,Sector,Asset Class,Market Value,Notional Value,Quantity,Price,Location,Exchange,Currency,FX Rate,Market Currency,Accrual Date,Market Weight,Notional Weight\n\
\"VSAT\",\"VIASAT INC\",\"EQUITY\",\"Information Technology\",\"Equity\",\"1,050,000,000.00\",\"1,050,000,000.00\",\"20,000,000.00\",\"52.50\",\"United States\",\"NASDAQ\",\"USD\",\"1.00\",\"USD\",\"-\",\"0.66\",\"0.66\"\n\
\"JXN\",\"JACKSON FINANCIAL CLASS A\",\"EQUITY\",\"Financials\",\"Equity\",\"900,000,000.00\",\"900,000,000.00\",\"9,000,000.00\",\"100.00\",\"United States\",\"NYSE\",\"USD\",\"1.00\",\"USD\",\"-\",\"0.57\",\"0.57\"\n\
\"JXN\",\"JACKSON FINANCIAL CLASS A\",\"SWAP\",\"Financials\",\"Equity\",\"0.00\",\"12,000,000.00\",\"120,000.00\",\"100.00\",\"United States\",\"-\",\"USD\",\"1.00\",\"USD\",\"-\",\"-\",\"0.01\"\n\
\"XTSLA\",\"BLK CSH FND TREASURY SL AGENCY\",\"STIF\",\"Cash and/or Derivatives\",\"Money Market\",\"0.78\",\"-43,991,966.67\",\"1.00\",\"1.00\",\"United States\",\"-\",\"USD\",\"1.00\",\"USD\",\"-\",\"-\",\"-0.04\"\n";
        let rows = parse_ishares_csv(
            csv,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            DataSource::IsharesCdn,
        )
        .unwrap();
        let tickers: Vec<_> = rows.iter().filter_map(|r| r.ticker.as_deref()).collect();
        assert_eq!(tickers, ["VSAT", "JXN"]);
        assert!((rows[0].weight - 0.0066).abs() < 1e-9);
        assert_eq!(rows[0].cusip, "");
        assert_eq!(rows[0].as_of, NaiveDate::from_ymd_opt(2026, 9, 21).unwrap());
    }

    /// IWM's export has no `Type` column; an unlisted line with no ticker
    /// and no identifier cannot be joined and is dropped.
    #[test]
    fn parse_ishares_latest_holdings_drops_unidentifiable_rows() {
        let csv = "Ticker,Name,Sector,Asset Class,Market Value,Weight (%),Notional Value,Quantity,Price,Location,Exchange,Currency,FX Rate,Market Currency,Accrual Date\n\
\"TWST\",\"TWIST BIOSCIENCE\",\"Health Care\",\"Equity\",\"276,882,702.74\",\"0.36\",\"276,882,702.74\",\"1,669,678.00\",\"165.83\",\"United States\",\"NASDAQ\",\"USD\",\"1.00\",\"USD\",\"-\"\n\
\"-\",\"OMNIAB INC $12.50 VESTING Prvt\",\"Health Care\",\"Equity\",\"1.31\",\"0.00\",\"1.31\",\"130,676.00\",\"0.00\",\"United States\",\"NO MARKET (E.G. UNLISTED)\",\"USD\",\"1.00\",\"USD\",\"-\"\n";
        let rows = parse_ishares_csv(
            csv,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            DataSource::IsharesCdn,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ticker.as_deref(), Some("TWST"));
    }

    /// iShares answers retired holdings URLs with its product page and a 200.
    /// That is a wrong file, not an empty index.
    #[test]
    fn a_page_in_place_of_a_holdings_file_is_an_error() {
        let page =
            "<!DOCTYPE html><html><head><title>iShares</title></head><body>Holdings</body></html>";
        let d = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        assert!(parse_ishares_csv(page, d, DataSource::IsharesCdn).is_err());
        assert!(parse_invesco_csv(page, d).is_err());
        for src in [
            DataSource::IsharesCdn,
            DataSource::InvescoCdn,
            DataSource::SpdrCdn,
            DataSource::NasdaqApi,
        ] {
            assert!(parse_holdings(&src, page.as_bytes(), d).is_err(), "{src:?}");
        }
    }

    #[test]
    fn parse_invesco_dng_json_sample() {
        // Real QQQ response captured 2026-09-23 from the DNG endpoint with no
        // `loadType`, which returns every holding.
        let bytes = include_bytes!("../tests/fixtures/invesco_qqq_sample.json");
        let rows =
            parse_invesco_dng_json(bytes, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap()).unwrap();
        // 98 common stocks + 3 depositary receipts; currency, collateral,
        // the index future and synthetic cash are dropped.
        assert_eq!(rows.len(), 101);
        assert_eq!(rows[0].ticker.as_deref(), Some("NVDA"));
        assert_eq!(rows[0].cusip, "67066G104");
        assert!((rows[0].weight - 0.08277194).abs() < 1e-9);
        assert_eq!(rows[0].as_of, NaiveDate::from_ymd_opt(2026, 9, 22).unwrap());
        assert!(rows.iter().any(|r| r.ticker.as_deref() == Some("ASML")));
        assert!(rows.iter().all(|r| r.source == DataSource::InvescoCdn));
    }

    /// `fetch_today` must move on to the backup when the primary serves a
    /// 200 page that is not a holdings file.
    #[tokio::test]
    async fn fetch_falls_through_a_200_page_to_the_backup() {
        use wiremock::matchers::path;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(path("/primary"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<!DOCTYPE html><html><body>product page</body></html>"),
            )
            .mount(&server)
            .await;
        let holdings = "Ticker,Name,Sector,Asset Class,Market Value,Weight (%),Quantity\n\
\"AAPL\",\"APPLE INC\",\"IT\",\"Equity\",\"1,000.00\",\"7.12\",\"10.00\"\n";
        Mock::given(path("/backup"))
            .respond_with(ResponseTemplate::new(200).set_body_string(holdings))
            .mount(&server)
            .await;

        let primary = format!("{}/primary", server.uri());
        let backup = format!("{}/backup", server.uri());
        let endpoints = [
            (DataSource::IsharesCdn, "P", primary.as_str()),
            (DataSource::IsharesCdn, "B", backup.as_str()),
        ];
        let (_, body) = SponsorClient::new()
            .unwrap()
            .fetch_first(IndexId::Sp400, &endpoints)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), holdings.as_bytes());
    }

    #[test]
    fn parse_nasdaq_ndx_sample() {
        // Real Nasdaq list-type response captured 2026-05-15 against the
        // live `nasdaq100` listid. Committed under
        // crates/indexkit/tests/fixtures/.
        let bytes = include_bytes!("../tests/fixtures/nasdaq_ndx_sample.json");
        let fallback = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
        let rows = parse_nasdaq_ndx_json(bytes, fallback).unwrap();
        // NDX has 100 members; Nasdaq's list-type response returns 101
        // because a few names (e.g. GOOG/GOOGL) trade as two share
        // classes that both count in the index.
        assert!(
            (100..=110).contains(&rows.len()),
            "expected ~101 NDX rows, got {}",
            rows.len()
        );
        // Weights must sum to ~1.0 (mcap / Σmcap).
        let total_weight: f64 = rows.iter().map(|r| r.weight).sum();
        assert!(
            (total_weight - 1.0).abs() < 1e-6,
            "weights should sum to 1.0, got {total_weight}"
        );
        // Every row has a ticker and a non-zero market cap.
        for r in &rows {
            assert!(r.ticker.as_deref().map(|t| !t.is_empty()).unwrap_or(false));
            assert!(r.market_value_usd > 0.0);
            assert!(matches!(r.source, DataSource::NasdaqApi));
        }
        // Sanity-check the top of the universe -- AAPL, MSFT, NVDA, GOOG,
        // GOOGL are the largest names in NDX and should all be in the
        // top 10 by market cap.
        let top10: Vec<&str> = rows
            .iter()
            .take(10)
            .filter_map(|r| r.ticker.as_deref())
            .collect();
        assert!(top10.contains(&"AAPL"), "AAPL not in top-10: {top10:?}");
        assert!(top10.contains(&"MSFT"), "MSFT not in top-10: {top10:?}");
        assert!(top10.contains(&"NVDA"), "NVDA not in top-10: {top10:?}");
    }

    #[test]
    fn parse_nasdaq_ndx_json_rejects_empty() {
        let err = parse_nasdaq_ndx_json(
            br#"{"data":{"data":{"rows":[]}}}"#,
            NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn parse_nasdaq_ndx_strips_common_stock_suffix() {
        let body = br#"{
            "data": {
                "date": "May 15, 2026 10:30 AM",
                "data": {
                    "headers": {},
                    "rows": [
                        {"symbol": "AAPL", "companyName": "Apple Inc. Common Stock", "marketCap": "1,000"},
                        {"symbol": "MSFT", "companyName": "Microsoft Corp Common Shares", "marketCap": "1,000"}
                    ]
                }
            }
        }"#;
        let rows =
            parse_nasdaq_ndx_json(body, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        let aapl = rows
            .iter()
            .find(|r| r.ticker.as_deref() == Some("AAPL"))
            .unwrap();
        assert_eq!(aapl.name, "Apple Inc.");
        let msft = rows
            .iter()
            .find(|r| r.ticker.as_deref() == Some("MSFT"))
            .unwrap();
        assert_eq!(msft.name, "Microsoft Corp");
        assert_eq!(aapl.as_of, NaiveDate::from_ymd_opt(2026, 5, 15).unwrap());
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

        // SP600: IJR primary, SPSM backup (SSGA's S&P 600 fund; the SLY
        // file is gone).
        let sp600 = sponsor_urls(IndexId::Sp600);
        assert_eq!(sp600.len(), 2);
        assert_eq!(sp600[0].1, "IJR");
        assert_eq!(sp600[1].1, "SPSM");

        // NDX: Nasdaq's public list-type API is primary (official, free,
        // no geo-block). Invesco DNG endpoints for QQQ + QQQM follow as
        // backups.
        let ndx = sponsor_urls(IndexId::Ndx);
        assert_eq!(ndx.len(), 3);
        assert_eq!(ndx[0].0, DataSource::NasdaqApi);
        assert_eq!(ndx[0].1, "NDX");
        assert!(ndx[0].2.contains("api.nasdaq.com"));
        assert_eq!(ndx[1].0, DataSource::InvescoCdn);
        assert_eq!(ndx[1].1, "QQQ");
        assert!(ndx[1].2.contains("dng-api.invesco.com"));
        assert_eq!(ndx[2].1, "QQQM");

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
