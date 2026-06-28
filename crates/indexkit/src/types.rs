//! Core domain types -- [`Constituent`], [`IndexSnapshot`], [`IndexId`],
//! [`DataSource`], [`Resolution`].

use crate::date::YearMonth;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Which upstream source produced a given row.
///
/// Rows written by different sources for the same `(index, identity, date)`
/// are reconciled by the [`crate::coalesce`] layer, which keeps a single
/// best row per key based on each source's [`priority`](DataSource::priority).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataSource {
    /// Live sponsor CDN (iShares, Invesco, State Street).
    ///
    /// Payload is the ETF issuer's own public holdings file, which they
    /// refresh daily. These CDN endpoints are covered by each sponsor's
    /// terms of service; indexkit treats them as best-effort and always
    /// keeps a Wayback snapshot as a fallback.
    IsharesCdn,
    InvescoCdn,
    SpdrCdn,
    /// Nasdaq's public list-type quote API
    /// (`api.nasdaq.com/api/quote/list-type/{listid}`).
    ///
    /// Free, unauthenticated JSON endpoint that returns the official
    /// constituent universe of a Nasdaq-published index. Used as the
    /// daily primary for NDX since Invesco's old QQQ holdings URL was
    /// retired in 2026-Q1.
    ///
    /// Payload provides ticker, company name, market cap, and last sale
    /// price (no CUSIP / LEI / explicit weight). Weight is implicit in
    /// market cap; downstream `cusip-resolver` enriches CUSIP.
    NasdaqApi,
    /// Internet Archive's Wayback Machine. Snapshots of sponsor pages
    /// captured by `archive.org` on a specific date.
    ///
    /// `YYYYMMDD` encodes the snapshot date. Coverage is patchy
    /// (typically 40-60 % of trading days).
    Wayback(String),
    /// SEC EDGAR N-PORT filing -- monthly baseline, guaranteed from
    /// 2019-11 onwards.
    SecNport,
    /// fja05680/sp500 GitHub mirror (MIT license).
    ///
    /// Daily S&P 500 component changes from 1996-01-02 onwards. Rows
    /// provide ticker only (no CUSIP / LEI / weight / shares). Maintained
    /// by Farrell J. Aultman.
    ///
    /// Source repo: <https://github.com/fja05680/sp500>.
    GithubFja05680,
    /// yfiua/index-constituents GitHub mirror (Apache-2.0 license).
    ///
    /// Monthly snapshots of major index constituents (S&P 500, Nasdaq-100,
    /// Dow Jones, etc.) from 2018 onwards. Rows provide ticker only.
    ///
    /// `month` is the `YYYY-MM` directory the row was sourced from.
    ///
    /// Source repo: <https://github.com/yfiua/index-constituents>.
    GithubYfiua {
        /// Year-month the yfiua snapshot belongs to.
        month: YearMonth,
    },
    /// hanshof/sp500_constituents GitHub mirror (MIT license).
    ///
    /// Daily S&P 500 historical components, 1996-present. Same shape as
    /// `GithubFja05680` but maintained independently; used as a cross-
    /// check layer.
    ///
    /// Source repo: <https://github.com/hanshof/sp500_constituents>.
    GithubHanshof,
}

impl DataSource {
    /// Short string tag stored in the parquet `source` column.
    pub fn tag(&self) -> String {
        match self {
            DataSource::IsharesCdn => "ishares_cdn".into(),
            DataSource::InvescoCdn => "invesco_cdn".into(),
            DataSource::SpdrCdn => "spdr_cdn".into(),
            DataSource::NasdaqApi => "nasdaq_api".into(),
            DataSource::Wayback(yyyymmdd) => format!("wayback_{yyyymmdd}"),
            DataSource::SecNport => "sec_nport".into(),
            DataSource::GithubFja05680 => "github_fja05680".into(),
            DataSource::GithubYfiua { month } => format!("github_yfiua_{month}"),
            DataSource::GithubHanshof => "github_hanshof".into(),
        }
    }

    /// Parse a `source` tag back into a [`DataSource`].
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "ishares_cdn" => Some(DataSource::IsharesCdn),
            "invesco_cdn" => Some(DataSource::InvescoCdn),
            "spdr_cdn" => Some(DataSource::SpdrCdn),
            "nasdaq_api" => Some(DataSource::NasdaqApi),
            "sec_nport" => Some(DataSource::SecNport),
            "github_fja05680" => Some(DataSource::GithubFja05680),
            "github_hanshof" => Some(DataSource::GithubHanshof),
            tag if tag.starts_with("wayback_") => Some(DataSource::Wayback(tag[8..].to_string())),
            tag if tag.starts_with("github_yfiua_") => {
                let rest = &tag[13..];
                rest.parse::<YearMonth>()
                    .ok()
                    .map(|month| DataSource::GithubYfiua { month })
            }
            _ => None,
        }
    }

    /// Priority weight. Higher wins when multiple sources cover the same
    /// `(index, identity, date)` key during coalesce.
    pub fn priority(&self) -> u8 {
        match self {
            DataSource::IsharesCdn
            | DataSource::InvescoCdn
            | DataSource::SpdrCdn
            | DataSource::NasdaqApi => 5,
            DataSource::GithubFja05680 => 4,
            DataSource::GithubYfiua { .. } => 3,
            DataSource::GithubHanshof => 3,
            DataSource::Wayback(_) => 2,
            DataSource::SecNport => 1,
        }
    }
}

/// Confidence tier of the data available for a given `(index, month)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    /// Every trading day in the month has at least one row (from CDN or
    /// Wayback).
    Daily,
    /// Some trading days are covered, others are not.
    Sparse,
    /// Only one row per month (N-PORT baseline).
    Monthly,
    /// No data.
    None,
}

/// One security held by an index ETF on a specific date.
///
/// # Field coverage by source
///
/// Different upstream sources populate different fields. Always present:
/// `name` (may be empty for ticker-only mirrors), `as_of`, `source`.
///
/// | Field        | CDN / Wayback | N-PORT (1) | GitHub mirrors (fja05680, yfiua, hanshof) |
/// |--------------|---------------|------------|-------------------------------------------|
/// | `ticker`     | ~99 % present | `None`     | always `Some(t)`                          |
/// | `cusip`      | present       | present    | empty string (`""`) -- unknown            |
/// | `lei`        | optional      | present    | `None`                                    |
/// | `shares`     | present       | present    | `0.0` -- unknown                          |
/// | `market_value_usd` | present | present    | `0.0` -- unknown                          |
/// | `weight`     | fraction of NAV | fraction | `f64::NAN` -- unknown, use [`weight_opt`][Self::weight_opt] |
///
/// (1) SEC N-PORT has no ticker column; every N-PORT `Constituent::ticker`
/// is `None`. Use `cusip` as the join key when N-PORT rows are in play.
///
/// # Primary join keys
///
/// - **CUSIP** -- preferred, always present for CDN / Wayback / N-PORT rows.
/// - **Ticker** -- preferred when joining GitHub mirror rows (cusip is empty).
/// - **LEI** -- available for most US issuers, joinable against GLEIF data.
///
/// # Missing weights
///
/// GitHub mirror rows ([`DataSource::GithubFja05680`],
/// [`DataSource::GithubYfiua`], [`DataSource::GithubHanshof`]) are
/// ticker-only -- they carry no weight, shares, or market value. The
/// `weight` field is set to `f64::NAN` for these rows as a sentinel.
/// Prefer [`weight_opt`][Self::weight_opt] for consumer code that needs
/// to branch on presence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constituent {
    /// Ticker symbol.
    pub ticker: Option<String>,
    /// Security name as reported on the source file (issuer + share class).
    pub name: String,
    /// CUSIP (9-char). Primary join key for CDN / Wayback / N-PORT rows.
    /// Empty string for GitHub mirror rows (ticker-only sources).
    pub cusip: String,
    /// Legal Entity Identifier (20-char) -- ISO 17442 issuer ID.
    pub lei: Option<String>,
    /// Shares held (floating point: allows fractional shares for some ETFs).
    /// `0.0` when unknown (GitHub mirror sources).
    pub shares: f64,
    /// Fair value in USD as reported on the source file.
    /// `0.0` when unknown (GitHub mirror sources).
    pub market_value_usd: f64,
    /// Weight as fraction of NAV in `[0.0, 1.0]`.
    ///
    /// `f64::NAN` when the source does not carry weight data (GitHub mirror
    /// sources). Use [`weight_opt`][Self::weight_opt] for an `Option<f64>`
    /// that returns `None` on `NaN`.
    pub weight: f64,
    /// SEC CIK of the issuer, if identifiable. Usually `None`.
    pub issuer_cik: Option<String>,
    /// GICS / SIC sector. Reserved for v1.1; currently always `None`.
    pub sector: Option<Sector>,
    /// Date this row represents (the business day as of which the
    /// holdings are priced). For monthly-only rows from N-PORT this is
    /// the last business day of the reporting period.
    pub as_of: NaiveDate,
    /// Upstream that produced this row.
    pub source: DataSource,
}

impl Constituent {
    /// Weight as [`Option<f64>`].
    ///
    /// Returns `None` when [`weight`][Self::weight] is `NaN` (the sentinel
    /// used by ticker-only GitHub mirror sources) or a subnormal/infinite
    /// value. Otherwise returns `Some(weight)`.
    ///
    /// # Example
    ///
    /// ```
    /// use indexkit::{Constituent, DataSource};
    /// use chrono::NaiveDate;
    ///
    /// let row = Constituent {
    ///     ticker: Some("AAPL".into()),
    ///     name: "".into(),
    ///     cusip: "".into(),
    ///     lei: None,
    ///     shares: 0.0,
    ///     market_value_usd: 0.0,
    ///     weight: f64::NAN,
    ///     issuer_cik: None,
    ///     sector: None,
    ///     as_of: NaiveDate::from_ymd_opt(2024, 1, 15).unwrap(),
    ///     source: DataSource::GithubFja05680,
    /// };
    /// assert_eq!(row.weight_opt(), None);
    /// ```
    pub fn weight_opt(&self) -> Option<f64> {
        if self.weight.is_finite() {
            Some(self.weight)
        } else {
            None
        }
    }
}

/// GICS sector placeholder.
///
/// Reserved for a v1.1 feature. N-PORT does not include GICS sector. A
/// future `indexkit-gics` module will derive sector from SEC SIC codes via
/// a SIC -> GICS cross-walk. Currently every [`Constituent::sector`] field
/// is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Sector {
    CommunicationServices,
    ConsumerDiscretionary,
    ConsumerStaples,
    Energy,
    Financials,
    HealthCare,
    Industrials,
    InformationTechnology,
    Materials,
    RealEstate,
    Utilities,
}

/// A full snapshot of index constituents for a given month.
///
/// `constituents` may contain rows from multiple calendar dates within the
/// month (when daily-resolution data exists) or just one row per holding
/// (when only monthly N-PORT data is available). Rows are sorted by
/// `(as_of, -weight)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexSnapshot {
    /// The index this snapshot represents.
    pub index: IndexId,
    /// Month of the snapshot.
    pub year_month: YearMonth,
    /// Holdings. Multi-date if daily data is available.
    pub constituents: Vec<Constituent>,
}

impl IndexSnapshot {
    /// Whether this snapshot carries weight data.
    ///
    /// Returns `true` if at least one row has a finite `weight` value
    /// (i.e. it came from a CDN, Wayback, or N-PORT source). Returns
    /// `false` if every row is ticker-only (all GitHub mirror sources)
    /// or the snapshot is empty.
    ///
    /// Useful as a quick gate for analytics code: a snapshot with
    /// `has_weights() == false` is a ticker universe only, not a weight
    /// vector.
    pub fn has_weights(&self) -> bool {
        self.constituents.iter().any(|c| c.weight.is_finite())
    }
}

/// Single-day snapshot -- every holding as of a specific date.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailySnapshot {
    /// The index this snapshot represents.
    pub index: IndexId,
    /// Date of the snapshot.
    pub date: NaiveDate,
    /// Holdings sorted by descending weight.
    pub constituents: Vec<Constituent>,
    /// Source that produced this snapshot.
    pub source: DataSource,
}

/// Supported index identifiers.
///
/// Strings: `"sp500"`, `"sp400"`, `"sp600"`, `"ndx"`, `"dji"`, `"rut"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexId {
    /// S&P 500 (via IVV -- iShares Core S&P 500 ETF).
    Sp500,
    /// S&P MidCap 400 (via IJH).
    Sp400,
    /// S&P SmallCap 600 (via IJR).
    Sp600,
    /// Nasdaq-100 (via QQQ).
    Ndx,
    /// Dow Jones Industrial Average (via DIA).
    Dji,
    /// Russell 2000 (via IWM -- iShares Russell 2000 ETF).
    Rut,
}

impl IndexId {
    /// All six indices.
    pub const ALL: [IndexId; 6] = [
        IndexId::Sp500,
        IndexId::Sp400,
        IndexId::Sp600,
        IndexId::Ndx,
        IndexId::Dji,
        IndexId::Rut,
    ];

    /// Parse from short string id.
    pub fn from_str_id(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sp500" => Some(IndexId::Sp500),
            "sp400" => Some(IndexId::Sp400),
            "sp600" => Some(IndexId::Sp600),
            "ndx" | "nasdaq100" | "nasdaq-100" => Some(IndexId::Ndx),
            "dji" | "djia" | "dow" => Some(IndexId::Dji),
            "rut" | "russell2000" | "russell-2000" => Some(IndexId::Rut),
            _ => None,
        }
    }

    /// Short string id used for parquet file prefixes.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexId::Sp500 => "sp500",
            IndexId::Sp400 => "sp400",
            IndexId::Sp600 => "sp600",
            IndexId::Ndx => "ndx",
            IndexId::Dji => "dji",
            IndexId::Rut => "rut",
        }
    }
}

impl std::fmt::Display for IndexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for IndexId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        IndexId::from_str_id(s).ok_or_else(|| format!("unknown index id: {s:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexid_roundtrip() {
        for &id in &IndexId::ALL {
            let s = id.as_str();
            assert_eq!(IndexId::from_str_id(s), Some(id));
        }
    }

    #[test]
    fn indexid_aliases() {
        assert_eq!(IndexId::from_str_id("nasdaq100"), Some(IndexId::Ndx));
        assert_eq!(IndexId::from_str_id("djia"), Some(IndexId::Dji));
        assert_eq!(IndexId::from_str_id("SP500"), Some(IndexId::Sp500));
    }

    #[test]
    fn indexid_unknown() {
        assert_eq!(IndexId::from_str_id("totally-fake"), None);
    }

    #[test]
    fn data_source_tag_roundtrip_core() {
        for ds in [
            DataSource::IsharesCdn,
            DataSource::InvescoCdn,
            DataSource::SpdrCdn,
            DataSource::NasdaqApi,
            DataSource::SecNport,
            DataSource::GithubFja05680,
            DataSource::GithubHanshof,
            DataSource::Wayback("20240315".into()),
            DataSource::GithubYfiua {
                month: YearMonth::new(2024, 3).unwrap(),
            },
        ] {
            let tag = ds.tag();
            let back = DataSource::from_tag(&tag).expect("parseable");
            assert_eq!(back, ds, "tag {tag} did not round-trip");
        }
    }

    #[test]
    fn data_source_priority_ladder() {
        assert_eq!(DataSource::IsharesCdn.priority(), 5);
        assert_eq!(DataSource::InvescoCdn.priority(), 5);
        assert_eq!(DataSource::SpdrCdn.priority(), 5);
        assert_eq!(DataSource::NasdaqApi.priority(), 5);
        assert_eq!(DataSource::GithubFja05680.priority(), 4);
        assert_eq!(
            DataSource::GithubYfiua {
                month: YearMonth::new(2024, 3).unwrap()
            }
            .priority(),
            3
        );
        assert_eq!(DataSource::GithubHanshof.priority(), 3);
        assert_eq!(DataSource::Wayback("20240315".into()).priority(), 2);
        assert_eq!(DataSource::SecNport.priority(), 1);
    }

    fn ticker_only_row(ticker: &str, date: NaiveDate, src: DataSource) -> Constituent {
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
    fn weight_opt_nan_is_none() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let row = ticker_only_row("AAPL", d, DataSource::GithubFja05680);
        assert_eq!(row.weight_opt(), None);
    }

    #[test]
    fn weight_opt_finite_is_some() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let mut row = ticker_only_row("AAPL", d, DataSource::IsharesCdn);
        row.weight = 0.072;
        assert_eq!(row.weight_opt(), Some(0.072));
    }

    #[test]
    fn weight_opt_infinity_is_none() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let mut row = ticker_only_row("AAPL", d, DataSource::IsharesCdn);
        row.weight = f64::INFINITY;
        assert_eq!(row.weight_opt(), None);
    }

    #[test]
    fn snapshot_has_weights_true_when_any_finite() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let mut with_weight = ticker_only_row("AAPL", d, DataSource::IsharesCdn);
        with_weight.weight = 0.05;
        let nan_row = ticker_only_row("MSFT", d, DataSource::GithubFja05680);
        let s = IndexSnapshot {
            index: IndexId::Sp500,
            year_month: YearMonth::new(2024, 1).unwrap(),
            constituents: vec![with_weight, nan_row],
        };
        assert!(s.has_weights());
    }

    #[test]
    fn snapshot_has_weights_false_when_all_nan() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let row1 = ticker_only_row("AAPL", d, DataSource::GithubFja05680);
        let row2 = ticker_only_row("MSFT", d, DataSource::GithubHanshof);
        let s = IndexSnapshot {
            index: IndexId::Sp500,
            year_month: YearMonth::new(2024, 1).unwrap(),
            constituents: vec![row1, row2],
        };
        assert!(!s.has_weights());
    }

    #[test]
    fn snapshot_has_weights_false_when_empty() {
        let s = IndexSnapshot {
            index: IndexId::Sp500,
            year_month: YearMonth::new(2024, 1).unwrap(),
            constituents: Vec::new(),
        };
        assert!(!s.has_weights());
    }
}
