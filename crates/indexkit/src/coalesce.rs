//! Merge rows from multiple sources into a single coherent snapshot.
//!
//! # Priority
//!
//! `DataSource::priority()` ranks sources:
//! - `3` -- live sponsor CDN (`IsharesCdn`, `InvescoCdn`, `SpdrCdn`)
//! - `2` -- Wayback Machine snapshots
//! - `1` -- SEC N-PORT baseline
//!
//! When rows from multiple sources cover the same `(index, cusip, as_of)`
//! key, the higher-priority row wins.

use crate::types::Constituent;
use std::collections::HashMap;

/// Merge multiple row vectors into one, keeping the highest-priority row
/// per `(cusip, as_of)` key.
///
/// Order within the result: sorted by `as_of` then descending `weight`.
pub fn coalesce(inputs: Vec<Vec<Constituent>>) -> Vec<Constituent> {
    let mut picked: HashMap<(String, chrono::NaiveDate), Constituent> = HashMap::new();
    for rows in inputs {
        for r in rows {
            let key = (r.cusip.clone(), r.as_of);
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
    out.sort_by(|a, b| {
        a.as_of.cmp(&b.as_of).then(
            b.weight
                .partial_cmp(&a.weight)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    out
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
}
