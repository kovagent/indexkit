//! Core domain types -- [`Constituent`], [`IndexSnapshot`], [`IndexId`],
//! [`DataSource`], [`Resolution`].

use crate::date::YearMonth;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Which upstream source produced a given row.
///
/// Rows written by different sources for the same `(index, date)` are
/// coalesced by the [`crate::coalesce`] layer with the priority
/// `Cdn > Wayback > Nport`.
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
    /// Internet Archive's Wayback Machine. Snapshots of sponsor pages
    /// captured by `archive.org` on a specific date.
    ///
    /// `YYYYMMDD` encodes the snapshot date. Coverage is patchy
    /// (typically 40-60 % of trading days).
    Wayback(String),
    /// SEC EDGAR N-PORT filing -- monthly baseline, guaranteed from
    /// 2019-11 onwards.
    SecNport,
}

impl DataSource {
    /// Short string tag stored in the parquet `source` column.
    pub fn tag(&self) -> String {
        match self {
            DataSource::IsharesCdn => "ishares_cdn".into(),
            DataSource::InvescoCdn => "invesco_cdn".into(),
            DataSource::SpdrCdn => "spdr_cdn".into(),
            DataSource::Wayback(yyyymmdd) => format!("wayback_{yyyymmdd}"),
            DataSource::SecNport => "sec_nport".into(),
        }
    }

    /// Parse a `source` tag back into a [`DataSource`].
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "ishares_cdn" => Some(DataSource::IsharesCdn),
            "invesco_cdn" => Some(DataSource::InvescoCdn),
            "spdr_cdn" => Some(DataSource::SpdrCdn),
            "sec_nport" => Some(DataSource::SecNport),
            tag if tag.starts_with("wayback_") => Some(DataSource::Wayback(tag[8..].to_string())),
            _ => None,
        }
    }

    /// Priority weight. Higher wins when multiple sources cover the same
    /// `(index, date)`.
    pub fn priority(&self) -> u8 {
        match self {
            DataSource::IsharesCdn | DataSource::InvescoCdn | DataSource::SpdrCdn => 3,
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
/// The full primary join key is **CUSIP**. LEI is available for most
/// US issuers and can be joined against GLEIF data.
///
/// Ticker coverage depends on the source:
/// - **Sponsor CDN** (`IsharesCdn`, `InvescoCdn`, `SpdrCdn`): ticker is
///   typically present (~99 % of holdings).
/// - **Wayback snapshots**: same as the underlying CDN at capture time.
/// - **SEC N-PORT** (`SecNport`): ticker is always `None` -- N-PORT does
///   not include tickers. Use CUSIP as the join key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Constituent {
    /// Ticker symbol.
    pub ticker: Option<String>,
    /// Security name as reported on the source file (issuer + share class).
    pub name: String,
    /// CUSIP (9-char). Primary join key; always present.
    pub cusip: String,
    /// Legal Entity Identifier (20-char) -- ISO 17442 issuer ID.
    pub lei: Option<String>,
    /// Shares held (floating point: allows fractional shares for some ETFs).
    pub shares: f64,
    /// Fair value in USD as reported on the source file.
    pub market_value_usd: f64,
    /// Weight as fraction of NAV in `[0.0, 1.0]`.
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
/// Strings: `"sp500"`, `"sp400"`, `"sp600"`, `"ndx"`, `"dji"`.
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
}

impl IndexId {
    /// All five indices.
    pub const ALL: [IndexId; 5] = [
        IndexId::Sp500,
        IndexId::Sp400,
        IndexId::Sp600,
        IndexId::Ndx,
        IndexId::Dji,
    ];

    /// Parse from short string id.
    pub fn from_str_id(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sp500" => Some(IndexId::Sp500),
            "sp400" => Some(IndexId::Sp400),
            "sp600" => Some(IndexId::Sp600),
            "ndx" | "nasdaq100" | "nasdaq-100" => Some(IndexId::Ndx),
            "dji" | "djia" | "dow" => Some(IndexId::Dji),
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
}
