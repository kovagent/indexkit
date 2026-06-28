//! Merge rows from multiple sources into a single coherent snapshot.
//!
//! # Precedence
//!
//! When several sources cover one row, the row from the source with the
//! higher [`DataSource::priority`][crate::types::DataSource::priority] is
//! kept. Full-field rows outrank ticker-only rows.
//!
//! # Identity key
//!
//! Rows coalesce by `(identity, as_of)` where `identity` is the first
//! non-empty of `(cusip, ticker, name)`. This handles:
//! - CUSIP-first join for CDN/Wayback/N-PORT rows (which always have CUSIP).
//! - Ticker-first join for GitHub mirror rows (which have empty CUSIP).
//! - Name-fallback when neither is available.
//!
//! When rows from multiple sources cover the same `(identity, as_of)` key,
//! the higher-priority source wins (its full row replaces any lower-
//! priority row). Cross-source field enrichment (e.g. take CUSIP from
//! N-PORT + ticker from GitHub) is deferred to v1.1.

use crate::types::Constituent;
use std::collections::HashMap;

/// The coalesce identity key for a row.
///
/// Prefers CUSIP, falls back to ticker, then name. This is what allows
/// CDN (CUSIP-bearing) rows and GitHub mirror (ticker-only) rows to
/// coexist without hash collisions on empty CUSIP.
fn identity_key(r: &Constituent) -> String {
    if !r.cusip.is_empty() {
        return r.cusip.clone();
    }
    if let Some(t) = r.ticker.as_deref() {
        if !t.is_empty() {
            return format!("T:{t}");
        }
    }
    format!("N:{}", r.name)
}

/// Merge multiple row vectors into one, keeping the highest-priority row
/// per `(identity, as_of)` key. See module docs for the identity rule.
///
/// Order within the result: sorted by `as_of` then descending `weight`
/// (NaN weights sort last).
pub fn coalesce(inputs: Vec<Vec<Constituent>>) -> Vec<Constituent> {
    let mut picked: HashMap<(String, chrono::NaiveDate), Constituent> = HashMap::new();
    for rows in inputs {
        for r in rows {
            let key = (identity_key(&r), r.as_of);
            let prio = r.source.priority();
            picked
                .entry(key)
                .and_modify(|existing| {
                    if prio > existing.source.priority() {
                        *existing = r.clone();
                    }
                })
                .or_insert(r);
        }
    }
    let mut out: Vec<Constituent> = picked.into_values().collect();
    // NaN-safe weight compare: treat NaN as "less than" any finite value so
    // finite-weight rows sort ahead of ticker-only rows within a date.
    out.sort_by(|a, b| {
        a.as_of.cmp(&b.as_of).then_with(|| {
            let av = finite_or_neg_inf(a.weight);
            let bv = finite_or_neg_inf(b.weight);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    out
}

fn finite_or_neg_inf(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        f64::NEG_INFINITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DataSource;
    use chrono::NaiveDate;

    fn row(cusip: &str, as_of: NaiveDate, weight: f64, src: DataSource) -> Constituent {
        Constituent {
            ticker: None,
            name: "x".into(),
            cusip: cusip.into(),
            lei: None,
            shares: 1.0,
            market_value_usd: 1.0,
            weight,
            issuer_cik: None,
            sector: None,
            as_of,
            source: src,
        }
    }

    #[test]
    fn cdn_beats_nport_same_day() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
        let nport = vec![row("CUSIP1", d, 0.05, DataSource::SecNport)];
        let cdn = vec![row("CUSIP1", d, 0.06, DataSource::IsharesCdn)];
        let merged = coalesce(vec![nport, cdn]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, DataSource::IsharesCdn);
        assert!((merged[0].weight - 0.06).abs() < 1e-9);
    }

    #[test]
    fn wayback_beats_nport() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
        let nport = vec![row("CUSIP1", d, 0.05, DataSource::SecNport)];
        let wb = vec![row(
            "CUSIP1",
            d,
            0.055,
            DataSource::Wayback("20240401".into()),
        )];
        let merged = coalesce(vec![nport, wb]);
        assert_eq!(merged.len(), 1);
        assert!(matches!(merged[0].source, DataSource::Wayback(_)));
    }

    #[test]
    fn cdn_beats_wayback() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
        let cdn = vec![row("CUSIP1", d, 0.05, DataSource::IsharesCdn)];
        let wb = vec![row(
            "CUSIP1",
            d,
            0.055,
            DataSource::Wayback("20240401".into()),
        )];
        let merged = coalesce(vec![cdn, wb]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, DataSource::IsharesCdn);
    }

    #[test]
    fn distinct_days_all_kept() {
        let d1 = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2024, 3, 2).unwrap();
        let a = vec![row("CUSIP1", d1, 0.05, DataSource::IsharesCdn)];
        let b = vec![row("CUSIP1", d2, 0.06, DataSource::IsharesCdn)];
        let merged = coalesce(vec![a, b]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].as_of, d1);
        assert_eq!(merged[1].as_of, d2);
    }

    fn ticker_only(ticker: &str, date: NaiveDate, src: DataSource) -> Constituent {
        Constituent {
            ticker: Some(ticker.into()),
            name: String::new(),
            cusip: String::new(),
            lei: None,
            shares: 0.0,
            market_value_usd: 0.0,
            weight: f64::NAN,
            issuer_cik: None,
            sector: None,
            as_of: date,
            source: src,
        }
    }

    #[test]
    fn ticker_only_rows_dedupe_by_ticker_not_empty_cusip() {
        // Two ticker-only rows for the same (date, ticker) from different
        // GitHub sources -- higher-priority source (fja05680) wins the key.
        let d = NaiveDate::from_ymd_opt(1996, 1, 2).unwrap();
        let fja = vec![ticker_only("AAPL", d, DataSource::GithubFja05680)];
        let hanshof = vec![ticker_only("AAPL", d, DataSource::GithubHanshof)];
        let merged = coalesce(vec![fja, hanshof]);
        assert_eq!(
            merged.len(),
            1,
            "ticker-only rows must dedupe by ticker, not empty cusip"
        );
        assert_eq!(merged[0].source, DataSource::GithubFja05680);
    }

    #[test]
    fn different_tickers_not_collapsed_on_empty_cusip() {
        let d = NaiveDate::from_ymd_opt(1996, 1, 2).unwrap();
        let aapl = vec![ticker_only("AAPL", d, DataSource::GithubFja05680)];
        let msft = vec![ticker_only("MSFT", d, DataSource::GithubFja05680)];
        let merged = coalesce(vec![aapl, msft]);
        assert_eq!(
            merged.len(),
            2,
            "distinct ticker-only rows must not collide on empty cusip"
        );
    }

    #[test]
    fn cdn_beats_github_mirror() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        let github = vec![ticker_only("AAPL", d, DataSource::GithubFja05680)];
        let mut cdn_row = ticker_only("AAPL", d, DataSource::IsharesCdn);
        cdn_row.cusip = "037833100".into();
        cdn_row.weight = 0.07;
        let cdn = vec![cdn_row];
        let merged = coalesce(vec![github, cdn]);
        // Distinct identity keys (github uses `T:AAPL`, cdn uses `037833100`)
        // so both rows survive. CDN row sorts first (finite weight).
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].source, DataSource::IsharesCdn);
    }

    #[test]
    fn github_fja05680_beats_github_yfiua() {
        let d = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        let ym = crate::date::YearMonth::new(2024, 3).unwrap();
        let yfiua = vec![ticker_only(
            "AAPL",
            d,
            DataSource::GithubYfiua { month: ym },
        )];
        let fja = vec![ticker_only("AAPL", d, DataSource::GithubFja05680)];
        let merged = coalesce(vec![yfiua, fja]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, DataSource::GithubFja05680);
    }
}
