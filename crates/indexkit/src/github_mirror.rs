//! GitHub-mirror historical constituent ingestion.
//!
//! This module pulls ticker-only index-constituent history from three open
//! OSS repositories on GitHub. All three carry permissive licenses and
//! provide orders of magnitude more historical coverage than SEC EDGAR
//! N-PORT (which only starts in Nov 2019).
//!
//! # Sources (v1.0.1)
//!
//! | Source | License | Coverage | Granularity | Fields |
//! |---|---|---|---|---|
//! | [fja05680/sp500] | MIT | 1996-01-02 -> present | daily change rows | tickers only |
//! | [yfiua/index-constituents] | Apache-2.0 | ~2018 -> present | monthly | tickers only |
//! | [hanshof/sp500_constituents] | MIT | 1996 -> present | daily change rows | tickers only |
//!
//! [fja05680/sp500]: https://github.com/fja05680/sp500
//! [yfiua/index-constituents]: https://github.com/yfiua/index-constituents
//! [hanshof/sp500_constituents]: https://github.com/hanshof/sp500_constituents
//!
//! # Output shape
//!
//! All three fetchers emit [`Constituent`] rows with the ticker populated
//! and every numeric field either zero or `f64::NAN` (weight). See
//! [`Constituent`] rustdoc for the full field-coverage matrix. Prefer
//! [`Constituent::weight_opt`] when branching on presence.
//!
//! # Forward-filling (fja05680 + hanshof)
//!
//! The fja05680 and hanshof CSVs emit one row per date the S&P 500
//! composition _changed_, not per trading day. Callers that need daily
//! rows should forward-fill within their downstream storage layer --
//! indexkit's [`fetch_fja05680_sp500`] returns the raw change-row list
//! exactly as upstream ships it.
//!
//! # Attribution
//!
//! MIT and Apache-2.0 both require copyright-notice retention in
//! distributions. See `data/licenses/` for verbatim LICENSE files shipped
//! with indexkit for each upstream.

use crate::date::YearMonth;
use crate::error::{Error, Result};
use crate::types::{Constituent, DataSource, IndexId};
use chrono::NaiveDate;
use std::time::Duration;

/// User-Agent for GitHub mirror fetches. GitHub raw endpoints do not
/// require a descriptive UA but some OSS operators rate-limit by UA.
pub const GITHUB_USER_AGENT: &str = "indexkit/1.0.1 (+https://github.com/userFRM/indexkit)";

/// Raw URL of the fja05680/sp500 historical components CSV (dated file --
/// updated through 2026-01-14 as of v1.0.1 release).
pub const FJA05680_CSV_URL: &str = "https://raw.githubusercontent.com/fja05680/sp500/master/\
     S%26P%20500%20Historical%20Components%20%26%20Changes(01-17-2026).csv";

/// Raw URL of the fja05680/sp500 historical components CSV (legacy,
/// original filename). Used as a fallback if the dated URL 404s.
pub const FJA05680_CSV_URL_LEGACY: &str =
    "https://raw.githubusercontent.com/fja05680/sp500/master/\
     S%26P%20500%20Historical%20Components%20%26%20Changes.csv";

/// Raw URL of the hanshof/sp500_constituents historical components CSV.
pub const HANSHOF_CSV_URL: &str =
    "https://raw.githubusercontent.com/hanshof/sp500_constituents/master/\
     sp_500_historical_components.csv";

/// Build the yfiua raw URL for a given index / year-month.
///
/// `github_yfiua_index_code` maps indexkit's [`IndexId`] onto yfiua's
/// filename codes (e.g. `IndexId::Ndx` -> `"nasdaq100"`).
pub fn yfiua_url(index: IndexId, ym: YearMonth) -> Option<String> {
    let code = github_yfiua_index_code(index)?;
    Some(format!(
        "https://raw.githubusercontent.com/yfiua/index-constituents/master/\
         docs/{:04}/{:02}/constituents-{code}.csv",
        ym.year(),
        ym.month(),
    ))
}

/// Map [`IndexId`] to yfiua's filename code, or `None` if unsupported.
///
/// yfiua covers `sp500`, `nasdaq100`, `dowjones` in the set indexkit
/// tracks. `sp400` / `sp600` are NOT covered by yfiua and return `None`.
pub fn github_yfiua_index_code(id: IndexId) -> Option<&'static str> {
    match id {
        IndexId::Sp500 => Some("sp500"),
        IndexId::Ndx => Some("nasdaq100"),
        IndexId::Dji => Some("dowjones"),
        IndexId::Sp400 | IndexId::Sp600 => None,
    }
}

/// Shared [`reqwest::Client`] builder with the indexkit GitHub-mirror UA.
fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(GITHUB_USER_AGENT)
        .timeout(Duration::from_secs(120))
        .build()?)
}

/// Fetch the fja05680/sp500 master CSV and return per-change-row ticker
/// lists.
///
/// The upstream CSV has two columns: `date,tickers` where `tickers` is a
/// comma-separated list inside a double-quoted field (may contain embedded
/// newlines). Each row represents ONE date on which the S&P 500
/// composition changed; in-between-change trading days share the previous
/// row's composition (callers forward-fill).
///
/// # Example
///
/// ```no_run
/// # async fn run() -> indexkit::Result<()> {
/// let rows = indexkit::github_mirror::fetch_fja05680_sp500().await?;
/// let (first_date, first_tickers) = rows.first().unwrap();
/// println!("earliest: {} with {} tickers", first_date, first_tickers.len());
/// # Ok(()) }
/// ```
///
/// # Ticker suffix normalisation
///
/// fja05680's dataset annotates removed tickers with a `-YYYYMM` suffix
/// indicating the month the ticker left the index (e.g. `AAL-199702`).
/// This parser strips those suffixes so callers receive clean tickers.
pub async fn fetch_fja05680_sp500() -> Result<Vec<(NaiveDate, Vec<String>)>> {
    let http = http_client()?;
    // Prefer the dated file (which is kept current), fall back to legacy.
    let body = match fetch_text(&http, FJA05680_CSV_URL).await {
        Ok(b) => b,
        Err(_) => fetch_text(&http, FJA05680_CSV_URL_LEGACY).await?,
    };
    parse_fja05680_csv(&body)
}

/// Fetch the hanshof/sp500_constituents master CSV.
///
/// Same shape as [`fetch_fja05680_sp500`] but hanshof's tickers have no
/// `-YYYYMM` suffixes -- they are already clean.
pub async fn fetch_hanshof_sp500() -> Result<Vec<(NaiveDate, Vec<String>)>> {
    let http = http_client()?;
    let body = fetch_text(&http, HANSHOF_CSV_URL).await?;
    parse_hanshof_csv(&body)
}

/// Fetch one yfiua monthly snapshot for a given index.
///
/// Returns the ticker universe for that (index, year-month) pair.
/// Returns [`Error::Other`] if the index is not in yfiua's universe or
/// if the remote file is missing for that month.
///
/// # Example
///
/// ```no_run
/// use indexkit::{ym, IndexId};
/// # async fn run() -> indexkit::Result<()> {
/// let tickers = indexkit::github_mirror::fetch_yfiua(
///     IndexId::Sp500, ym!(2024, 3),
/// ).await?;
/// # Ok(()) }
/// ```
pub async fn fetch_yfiua(index: IndexId, ym: YearMonth) -> Result<Vec<String>> {
    let url = yfiua_url(index, ym).ok_or_else(|| {
        Error::Other(format!(
            "yfiua does not publish {index}; supported: sp500, ndx, dji"
        ))
    })?;
    let http = http_client()?;
    let body = fetch_text(&http, &url).await?;
    Ok(parse_yfiua_csv(&body))
}

/// Fetch every available yfiua month for an index from `start` to today.
///
/// Iterates month-by-month, skipping any that 404 (yfiua publishes only
/// when tools succeed for that month -- early years have gaps).
///
/// `start` defaults to `2018-07` (yfiua's earliest published month for
/// sp500) if `None` is passed. `end` defaults to the current UTC month.
pub async fn fetch_yfiua_full(
    index: IndexId,
    start: Option<YearMonth>,
    end: Option<YearMonth>,
) -> Result<Vec<(YearMonth, Vec<String>)>> {
    let start = start.unwrap_or_else(|| YearMonth::new(2018, 7).unwrap());
    let end = end.unwrap_or_else(YearMonth::current_utc);
    if start > end {
        return Err(Error::Other(format!(
            "yfiua_full: start {start} > end {end}"
        )));
    }
    let http = http_client()?;
    let mut out = Vec::new();
    let months: Vec<YearMonth> = start.iter_to(end).collect();
    for ym in months {
        let Some(url) = yfiua_url(index, ym) else {
            return Err(Error::Other(format!(
                "yfiua does not publish {index}; supported: sp500, ndx, dji"
            )));
        };
        match fetch_text(&http, &url).await {
            Ok(body) => {
                let tickers = parse_yfiua_csv(&body);
                if !tickers.is_empty() {
                    out.push((ym, tickers));
                }
            }
            Err(e) => {
                // 404 is normal for months yfiua did not publish; log + skip.
                tracing::debug!(%index, %ym, "yfiua month not available: {e}");
            }
        }
    }
    Ok(out)
}

/// Forward-fill per-change-row data into per-trading-day data.
///
/// Given the fja05680 / hanshof change-row format:
/// `[(1996-01-02, [...487 tickers...]), (1996-01-05, [...488 tickers...])]`
/// produces one `(date, tickers)` entry for every calendar date between
/// the first and last change row (inclusive), carrying forward the last
/// seen composition.
///
/// Callers that want per-weekday data only should filter the result;
/// indexkit does not gate on `NaiveDate::weekday` because the upstream
/// change-row dates themselves are already trading days.
#[allow(clippy::needless_collect)]
pub fn forward_fill(changes: &[(NaiveDate, Vec<String>)]) -> Vec<(NaiveDate, Vec<String>)> {
    if changes.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let first = changes[0].0;
    let last = changes[changes.len() - 1].0;
    let mut cur_idx = 0usize;
    let mut cur_tickers: &[String] = &changes[0].1;
    let mut d = first;
    while d <= last {
        // Advance pointer while next change-row is at or before `d`.
        while cur_idx + 1 < changes.len() && changes[cur_idx + 1].0 <= d {
            cur_idx += 1;
            cur_tickers = &changes[cur_idx].1;
        }
        out.push((d, cur_tickers.to_vec()));
        d = match d.succ_opt() {
            Some(n) => n,
            None => break,
        };
    }
    out
}

// ---- Transform helpers ----

/// Convert a list of tickers into [`Constituent`] rows tagged with the
/// given source and `as_of` date.
///
/// `name`, `cusip`, `lei`, `shares`, `market_value_usd`, `issuer_cik`,
/// `sector` are all set to their "unknown" sentinels (empty / zero /
/// `None`). `weight` is `f64::NAN`.
pub fn tickers_to_constituents(
    tickers: &[String],
    as_of: NaiveDate,
    source: DataSource,
) -> Vec<Constituent> {
    tickers
        .iter()
        .filter_map(|t| {
            let t = t.trim();
            if t.is_empty() {
                return None;
            }
            Some(Constituent {
                ticker: Some(t.to_string()),
                name: String::new(),
                cusip: String::new(),
                lei: None,
                shares: 0.0,
                market_value_usd: 0.0,
                weight: f64::NAN,
                issuer_cik: None,
                sector: None,
                as_of,
                source: source.clone(),
            })
        })
        .collect()
}

// ---- CSV parsers ----

/// Parse the fja05680 master CSV. Strips `-YYYYMM` ticker suffixes.
pub fn parse_fja05680_csv(body: &str) -> Result<Vec<(NaiveDate, Vec<String>)>> {
    // fja05680 uses double-quoted tickers field with internal commas. We
    // rely on the fact that the date is the FIRST field (before the first
    // comma). Each logical CSV record may contain a double-quoted field
    // but never an embedded newline in the tickers column based on the
    // shipped file. Parse line-by-line, skipping the header.
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            // header: date,tickers
            if !line.starts_with("date") {
                return Err(Error::Other(format!(
                    "fja05680: unexpected header {line:?}"
                )));
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some((date_s, tickers_s)) = split_date_tickers(line) else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_s.trim(), "%Y-%m-%d") else {
            continue;
        };
        let tickers = parse_ticker_list(&tickers_s)
            .into_iter()
            .map(|t| strip_fja05680_suffix(&t))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>();
        out.push((date, tickers));
    }
    Ok(out)
}

/// Parse the hanshof master CSV. Tickers are already clean (no suffixes).
pub fn parse_hanshof_csv(body: &str) -> Result<Vec<(NaiveDate, Vec<String>)>> {
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            if !line.starts_with("date") {
                return Err(Error::Other(format!("hanshof: unexpected header {line:?}")));
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some((date_s, tickers_s)) = split_date_tickers(line) else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_s.trim(), "%Y-%m-%d") else {
            continue;
        };
        let tickers = parse_ticker_list(&tickers_s)
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>();
        out.push((date, tickers));
    }
    Ok(out)
}

/// Parse a yfiua monthly constituents CSV (`Symbol,Name` header).
pub fn parse_yfiua_csv(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            // Header: Symbol,Name (or similar). Skip whatever first row is.
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Symbol is the first comma-delimited field; the second field
        // (name) may contain a quoted comma so we take only the head.
        let sym = match split_once_csv(trimmed) {
            Some((s, _)) => s.trim().trim_matches('"').to_string(),
            None => continue,
        };
        if !sym.is_empty() {
            out.push(sym);
        }
    }
    out
}

/// Split a line into `(date_field, tickers_field)` where the tickers
/// field is the remainder after the first comma. Handles the fja05680
/// / hanshof format where tickers are wrapped in double-quotes.
fn split_date_tickers(line: &str) -> Option<(String, String)> {
    let first_comma = line.find(',')?;
    let date_s = line[..first_comma].to_string();
    let rest = line[first_comma + 1..].to_string();
    // Strip surrounding double-quotes on the tickers field.
    let rest = rest.trim();
    let rest = rest
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(rest)
        .to_string();
    Some((date_s, rest))
}

fn parse_ticker_list(s: &str) -> Vec<String> {
    s.split(',').map(|t| t.trim().to_string()).collect()
}

/// Strip fja05680's `-YYYYMM` removal-date suffix from a ticker.
///
/// Tickers like `AAL-199702` mean "ticker AAL was in the index and left
/// on 1997-02". For the purpose of building per-date composition, the
/// symbol at the time of inclusion was just `AAL` -- so we strip the
/// suffix.
///
/// Edge cases preserved:
/// - `BF.B`, `RDS.A` (class share suffix with `.`): left intact.
/// - `BRK.B` (no dash): left intact.
/// - `AZA.A-200106` (class share + removal date): strip at the dash.
fn strip_fja05680_suffix(raw: &str) -> String {
    let t = raw.trim();
    if let Some((head, tail)) = t.rsplit_once('-') {
        // Only strip if tail looks like a 6-digit YYYYMM.
        if tail.len() == 6 && tail.chars().all(|c| c.is_ascii_digit()) {
            return head.to_string();
        }
    }
    t.to_string()
}

/// Find the first comma NOT inside a double-quoted region and split there.
fn split_once_csv(line: &str) -> Option<(String, String)> {
    let mut in_quotes = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                return Some((line[..i].to_string(), line[i + 1..].to_string()));
            }
            _ => {}
        }
    }
    None
}

async fn fetch_text(http: &reqwest::Client, url: &str) -> Result<String> {
    let resp = http.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "github_mirror fetch {url}: HTTP {}",
            resp.status().as_u16()
        )));
    }
    Ok(resp.text().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fja05680_suffix_removes_six_digits() {
        assert_eq!(strip_fja05680_suffix("AAL-199702"), "AAL");
        assert_eq!(strip_fja05680_suffix("AZA.A-200106"), "AZA.A");
        assert_eq!(strip_fja05680_suffix("BF.B"), "BF.B");
        assert_eq!(strip_fja05680_suffix("AAPL"), "AAPL");
        assert_eq!(strip_fja05680_suffix("BRK.B"), "BRK.B");
    }

    #[test]
    fn strip_fja05680_suffix_keeps_short_tails() {
        // 5-digit tail isn't a YYYYMM -- don't strip.
        assert_eq!(strip_fja05680_suffix("FOO-12345"), "FOO-12345");
        // 7-digit tail isn't a YYYYMM -- don't strip.
        assert_eq!(strip_fja05680_suffix("FOO-1234567"), "FOO-1234567");
        // Alpha tail isn't YYYYMM -- don't strip.
        assert_eq!(strip_fja05680_suffix("FOO-BAR"), "FOO-BAR");
    }

    #[test]
    fn parse_fja05680_minimal() {
        let csv = r#"date,tickers
1996-01-02,"AAL-199702,AAPL,MSFT,IBM"
2024-03-15,"AAPL,MSFT,NVDA,BRK.B"
"#;
        let rows = parse_fja05680_csv(csv).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, NaiveDate::from_ymd_opt(1996, 1, 2).unwrap());
        assert_eq!(rows[0].1, vec!["AAL", "AAPL", "MSFT", "IBM"]);
        assert_eq!(rows[1].1, vec!["AAPL", "MSFT", "NVDA", "BRK.B"]);
    }

    #[test]
    fn parse_hanshof_minimal() {
        let csv = r#"date,tickers
1996-01-02,"AAL,AAPL,MSFT,IBM"
2019-01-11,"AAPL,MSFT,IBM"
"#;
        let rows = parse_hanshof_csv(csv).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, NaiveDate::from_ymd_opt(1996, 1, 2).unwrap());
        assert_eq!(rows[0].1, vec!["AAL", "AAPL", "MSFT", "IBM"]);
    }

    #[test]
    fn parse_yfiua_minimal() {
        let csv = r#"Symbol,Name
AAPL,Apple Inc.
MSFT,Microsoft Corp
META,"Meta Platforms, Inc. Class A"
BRK.B,Berkshire Hathaway Class B
"#;
        let syms = parse_yfiua_csv(csv);
        assert_eq!(syms, vec!["AAPL", "MSFT", "META", "BRK.B"]);
    }

    #[test]
    fn yfiua_url_shape() {
        let url = yfiua_url(IndexId::Sp500, YearMonth::new(2024, 3).unwrap()).unwrap();
        assert_eq!(
            url,
            "https://raw.githubusercontent.com/yfiua/index-constituents/master/\
             docs/2024/03/constituents-sp500.csv"
        );
        let url = yfiua_url(IndexId::Ndx, YearMonth::new(2024, 3).unwrap()).unwrap();
        assert!(url.ends_with("constituents-nasdaq100.csv"));
        let url = yfiua_url(IndexId::Dji, YearMonth::new(2024, 3).unwrap()).unwrap();
        assert!(url.ends_with("constituents-dowjones.csv"));
    }

    #[test]
    fn yfiua_not_available_for_sp400_sp600() {
        assert!(yfiua_url(IndexId::Sp400, YearMonth::new(2024, 3).unwrap()).is_none());
        assert!(yfiua_url(IndexId::Sp600, YearMonth::new(2024, 3).unwrap()).is_none());
    }

    #[test]
    fn tickers_to_constituents_nan_weight() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        let ts = ["AAPL".to_string(), "MSFT".to_string(), "".to_string()];
        let rows = tickers_to_constituents(&ts, d, DataSource::GithubFja05680);
        assert_eq!(rows.len(), 2, "empty tickers must be filtered");
        for r in &rows {
            assert!(r.weight.is_nan());
            assert!(r.cusip.is_empty());
            assert_eq!(r.as_of, d);
            assert_eq!(r.source, DataSource::GithubFja05680);
            assert!(r.ticker.is_some());
        }
    }

    #[test]
    fn forward_fill_carries_composition() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let changes = vec![
            (d(2024, 1, 2), vec!["A".to_string(), "B".to_string()]),
            (d(2024, 1, 4), vec!["A".to_string(), "C".to_string()]),
        ];
        let ff = forward_fill(&changes);
        assert_eq!(ff.len(), 3); // 1/2, 1/3, 1/4
        assert_eq!(ff[0].0, d(2024, 1, 2));
        assert_eq!(ff[0].1, vec!["A", "B"]);
        assert_eq!(ff[1].0, d(2024, 1, 3));
        assert_eq!(ff[1].1, vec!["A", "B"], "must carry forward to 1/3");
        assert_eq!(ff[2].0, d(2024, 1, 4));
        assert_eq!(ff[2].1, vec!["A", "C"], "must reflect change on 1/4");
    }

    #[test]
    fn forward_fill_empty() {
        let empty: Vec<(NaiveDate, Vec<String>)> = Vec::new();
        assert!(forward_fill(&empty).is_empty());
    }

    #[test]
    fn github_yfiua_index_code_mapping() {
        assert_eq!(github_yfiua_index_code(IndexId::Sp500), Some("sp500"));
        assert_eq!(github_yfiua_index_code(IndexId::Ndx), Some("nasdaq100"));
        assert_eq!(github_yfiua_index_code(IndexId::Dji), Some("dowjones"));
        assert_eq!(github_yfiua_index_code(IndexId::Sp400), None);
        assert_eq!(github_yfiua_index_code(IndexId::Sp600), None);
    }

    #[test]
    fn split_once_csv_basic() {
        let r = split_once_csv(r#"AAPL,Apple Inc."#).unwrap();
        assert_eq!(r.0, "AAPL");
        assert_eq!(r.1, "Apple Inc.");
    }

    #[test]
    fn split_once_csv_quoted_comma() {
        let r = split_once_csv(r#"META,"Meta Platforms, Inc. Class A""#).unwrap();
        assert_eq!(r.0, "META");
        assert_eq!(r.1, r#""Meta Platforms, Inc. Class A""#);
    }
}
