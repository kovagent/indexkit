//! ETF -> CIK / series mapping for the five supported indices.
//!
//! All CIK and series values are verified against live SEC submissions as of
//! 2026-04-23. The static mapping is the source of truth for which trust to
//! query, and which series (if any) to filter holdings to.
//!
//! The JSON form of this map is committed to `data/cik-map.json` for external
//! consumers (scripts, non-Rust callers).

use crate::types::IndexId;
use serde::{Deserialize, Serialize};

/// Identifies the ETF trust and (optionally) series that represents an index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CikEntry {
    /// Internal short id for the index, e.g. `"sp500"`.
    pub index: String,
    /// ETF ticker used as the proxy, e.g. `"IVV"`.
    pub ticker: String,
    /// Human-readable ETF name.
    pub name: String,
    /// 10-digit zero-padded SEC CIK of the filing trust.
    pub trust_cik: String,
    /// Series identifier within the trust, or `None` for single-series trusts.
    pub series_id: Option<String>,
}

impl CikEntry {
    /// The URL for the trust's SEC submissions feed.
    pub fn submissions_url(&self) -> String {
        format!(
            "https://data.sec.gov/submissions/CIK{}.json",
            self.trust_cik
        )
    }

    /// URL of a specific filing's primary_doc.xml, given an accession number
    /// (with or without dashes).
    pub fn primary_doc_url(&self, accession: &str) -> String {
        let no_dash: String = accession.chars().filter(|c| *c != '-').collect();
        let cik_num = self
            .trust_cik
            .trim_start_matches('0')
            .parse::<u64>()
            .unwrap_or(0);
        format!("https://www.sec.gov/Archives/edgar/data/{cik_num}/{no_dash}/primary_doc.xml")
    }
}

/// Return the [`CikEntry`] for a given index.
///
/// # Example
///
/// ```
/// use indexkit::{cik::entry_for, IndexId};
/// let e = entry_for(IndexId::Sp500);
/// assert_eq!(e.ticker, "IVV");
/// assert_eq!(e.trust_cik, "0001100663");
/// ```
pub fn entry_for(index: IndexId) -> CikEntry {
    match index {
        IndexId::Sp500 => CikEntry {
            index: "sp500".into(),
            ticker: "IVV".into(),
            name: "iShares Core S&P 500 ETF".into(),
            trust_cik: "0001100663".into(),
            series_id: Some("S000004310".into()),
        },
        IndexId::Sp400 => CikEntry {
            index: "sp400".into(),
            ticker: "IJH".into(),
            name: "iShares Core S&P Mid-Cap ETF".into(),
            trust_cik: "0001100663".into(),
            series_id: Some("S000004307".into()),
        },
        IndexId::Sp600 => CikEntry {
            index: "sp600".into(),
            ticker: "IJR".into(),
            name: "iShares Core S&P Small-Cap ETF".into(),
            trust_cik: "0001100663".into(),
            series_id: Some("S000004313".into()),
        },
        IndexId::Ndx => CikEntry {
            index: "ndx".into(),
            ticker: "QQQ".into(),
            name: "Invesco QQQ Trust, Series 1".into(),
            trust_cik: "0001067839".into(),
            // Invesco QQQ Trust is effectively single-series (S000101292 was
            // assigned in 2024 and does not appear in older filings). The
            // trust's CIK only files NPORT-P for QQQ itself, so we treat it
            // as a single-series trust and keep every filing.
            series_id: None,
        },
        IndexId::Dji => CikEntry {
            index: "dji".into(),
            ticker: "DIA".into(),
            name: "SPDR Dow Jones Industrial Average ETF Trust".into(),
            trust_cik: "0001041130".into(),
            // Single-series trust -- no series ID in N-PORT filings.
            series_id: None,
        },
        IndexId::Rut => CikEntry {
            index: "rut".into(),
            ticker: "IWM".into(),
            name: "iShares Russell 2000 ETF".into(),
            trust_cik: "0001100663".into(),
            // iShares Trust master CIK; IWM has its own series.
            series_id: Some("S000004361".into()),
        },
    }
}

/// All entries, in IndexId::ALL order.
pub fn all_entries() -> Vec<CikEntry> {
    IndexId::ALL.iter().map(|id| entry_for(*id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_entries_have_correct_count() {
        assert_eq!(all_entries().len(), 5);
    }

    #[test]
    fn each_cik_is_10_digits() {
        for e in all_entries() {
            assert_eq!(
                e.trust_cik.len(),
                10,
                "cik {} not zero-padded 10 digits",
                e.trust_cik
            );
            assert!(e.trust_cik.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn series_ids_well_formed() {
        for e in all_entries() {
            if let Some(sid) = e.series_id {
                assert!(
                    sid.starts_with('S') && sid.len() == 10,
                    "bad series id: {sid}"
                );
            }
        }
    }

    #[test]
    fn dji_has_no_series() {
        // Single-series trust -- verified against live SEC data.
        assert!(entry_for(IndexId::Dji).series_id.is_none());
    }

    #[test]
    fn submissions_url_shape() {
        let e = entry_for(IndexId::Sp500);
        assert_eq!(
            e.submissions_url(),
            "https://data.sec.gov/submissions/CIK0001100663.json"
        );
    }

    #[test]
    fn primary_doc_url_strips_dashes() {
        let e = entry_for(IndexId::Sp500);
        let url = e.primary_doc_url("0001752724-24-043113");
        assert_eq!(
            url,
            "https://www.sec.gov/Archives/edgar/data/1100663/000175272424043113/primary_doc.xml"
        );
    }
}
