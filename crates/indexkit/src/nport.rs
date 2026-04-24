//! SEC N-PORT `primary_doc.xml` parser.
//!
//! N-PORT is the monthly portfolio disclosure form required by SEC Rule 30b1-9.
//! Funds file three months after period end; the first 60 days of each filing
//! are embargoed from public view but the third-month version is published.
//!
//! # Schema summary
//!
//! ```xml
//! <edgarSubmission>
//!   <headerData>
//!     <filerInfo>
//!       <seriesClassInfo>
//!         <seriesId>S000004310</seriesId>
//!       </seriesClassInfo>
//!     </filerInfo>
//!   </headerData>
//!   <formData>
//!     <genInfo>
//!       <seriesId>S000004310</seriesId>        <!-- for multi-series trusts -->
//!       <seriesName>iShares Core S&P 500</seriesName>
//!       <repPdDate>2024-06-30</repPdDate>      <!-- reporting period end -->
//!     </genInfo>
//!     <invstOrSecs>
//!       <invstOrSec>
//!         <name>Apple Inc.</name>
//!         <lei>HWUPKR0MPOU8FGXBT394</lei>
//!         <title>Apple Inc., Common Stock</title>
//!         <cusip>037833100</cusip>
//!         <balance>197950000.00000000</balance>
//!         <valUSD>28900000000.00</valUSD>
//!         <pctVal>0.073</pctVal>
//!         <assetCat>EC</assetCat>
//!       </invstOrSec>
//!       ...
//!     </invstOrSecs>
//!   </formData>
//! </edgarSubmission>
//! ```
//!
//! ## Coverage limitations
//!
//! - N-PORT does NOT include ticker symbols. Only CUSIP + ISIN + name + LEI.
//! - N-PORT does NOT include issuer CIK. Only LEI for ~99 % of US issuers.
//! - Non-equity holdings (cash, repos, futures) are present but filtered out
//!   by default via the `assetCat == "EC"` check.

use crate::error::{Error, Result};
use crate::types::{Constituent, DataSource};
use chrono::NaiveDate;
use quick_xml::events::Event;
use quick_xml::Reader;

/// Parsed `<genInfo>` metadata.
#[derive(Debug, Clone, Default)]
pub struct NportHeader {
    /// `seriesId` value (e.g. `"S000004310"`). `None` for single-series trusts.
    pub series_id: Option<String>,
    /// `seriesName` value.
    pub series_name: Option<String>,
    /// `repPdDate` -- reporting period end (ISO `YYYY-MM-DD`).
    pub reporting_period_end: Option<String>,
}

/// Parsed N-PORT filing: header + holdings.
#[derive(Debug, Clone, Default)]
pub struct NportFiling {
    /// Filing header / generic info.
    pub header: NportHeader,
    /// Parsed holdings. Non-equity holdings (bonds, cash, repos) are included
    /// here; downstream filters may keep only `assetCat == "EC"`.
    pub holdings: Vec<RawHolding>,
}

/// One holding exactly as reported, before equity filtering.
#[derive(Debug, Clone, Default)]
pub struct RawHolding {
    pub name: String,
    pub lei: Option<String>,
    pub title: Option<String>,
    pub cusip: String,
    pub balance: f64,
    pub val_usd: f64,
    pub pct_val: f64,
    /// Asset category: `"EC"` = common stock, `"CORP"` = corporate debt, etc.
    pub asset_cat: Option<String>,
}

/// Parse an N-PORT `primary_doc.xml` byte slice into a [`NportFiling`].
///
/// The parser is single-pass over the document (streaming via `quick-xml`)
/// and does not allocate more than necessary for the result.
pub fn parse_nport(xml: &[u8]) -> Result<NportFiling> {
    let mut reader = Reader::from_reader(xml);
    // `config_mut` is the stable way to set options on quick-xml 0.36.
    reader.config_mut().trim_text(true);

    let mut filing = NportFiling::default();
    let mut path: Vec<String> = Vec::with_capacity(16);
    let mut buf = Vec::new();
    let mut current_holding: Option<RawHolding> = None;
    // Track whether we're in headerData's seriesClassInfo (where seriesId
    // also appears) so we don't overwrite a later genInfo seriesId.
    let mut seen_geninfo_series = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| Error::Xml(e.to_string()))?
        {
            Event::Start(ref e) => {
                let name = local_name(e.name().as_ref());
                path.push(name.clone());
                if name == "invstOrSec" {
                    current_holding = Some(RawHolding::default());
                }
            }
            Event::End(ref e) => {
                let name = local_name(e.name().as_ref());
                if name == "invstOrSec" {
                    if let Some(h) = current_holding.take() {
                        filing.holdings.push(h);
                    }
                }
                path.pop();
            }
            Event::Text(t) => {
                let text = t
                    .xml_content()
                    .map_err(|e: quick_xml::encoding::EncodingError| Error::Xml(e.to_string()))?
                    .to_string();
                handle_text(
                    &path,
                    &text,
                    &mut filing,
                    &mut current_holding,
                    &mut seen_geninfo_series,
                );
            }
            Event::Empty(_) => {
                // Self-closing tags (e.g. <isin value="US..."/>) -- not used
                // for our fields of interest.
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(filing)
}

fn local_name(bytes: &[u8]) -> String {
    // Strip xmlns prefix if present (e.g. "ns:name" -> "name").
    let s = std::str::from_utf8(bytes).unwrap_or("");
    match s.find(':') {
        Some(i) => s[i + 1..].to_string(),
        None => s.to_string(),
    }
}

fn handle_text(
    path: &[String],
    text: &str,
    filing: &mut NportFiling,
    current: &mut Option<RawHolding>,
    seen_geninfo_series: &mut bool,
) {
    let n = path.len();
    if n == 0 {
        return;
    }
    let leaf = &path[n - 1];
    let parent = if n >= 2 { path[n - 2].as_str() } else { "" };
    // Compose a simple path tail for disambiguation.
    let in_geninfo = path.iter().any(|s| s == "genInfo");
    let in_header = path.iter().any(|s| s == "headerData");

    // ---- header / genInfo fields ----
    if in_geninfo {
        match leaf.as_str() {
            "seriesId" => {
                filing.header.series_id = Some(text.to_string());
                *seen_geninfo_series = true;
            }
            "seriesName" => {
                filing.header.series_name = Some(text.to_string());
            }
            "repPdDate" => {
                filing.header.reporting_period_end = Some(text.to_string());
            }
            _ => {}
        }
        return;
    }
    if in_header && !*seen_geninfo_series && leaf == "seriesId" {
        // seriesId may also appear under headerData/filerInfo/seriesClassInfo.
        // Use it as a fallback if genInfo has not yet populated one.
        filing.header.series_id = Some(text.to_string());
        return;
    }

    // ---- holdings fields ----
    if let Some(h) = current.as_mut() {
        match (parent, leaf.as_str()) {
            ("invstOrSec", "name") => h.name = text.to_string(),
            ("invstOrSec", "lei") => h.lei = Some(text.to_string()),
            ("invstOrSec", "title") => h.title = Some(text.to_string()),
            ("invstOrSec", "cusip") => h.cusip = text.to_string(),
            ("invstOrSec", "balance") => {
                if let Ok(v) = text.parse::<f64>() {
                    h.balance = v;
                }
            }
            ("invstOrSec", "valUSD") => {
                if let Ok(v) = text.parse::<f64>() {
                    h.val_usd = v;
                }
            }
            ("invstOrSec", "pctVal") => {
                if let Ok(v) = text.parse::<f64>() {
                    h.pct_val = v;
                }
            }
            ("invstOrSec", "assetCat") => h.asset_cat = Some(text.to_string()),
            _ => {}
        }
    }
}

/// Convert a [`NportFiling`] to a vec of [`Constituent`], filtering to common
/// stock (`assetCat == "EC"`) and sorting by descending weight.
///
/// Holdings with missing CUSIP, zero market value, or non-`"EC"` asset
/// category are dropped. Each emitted row is stamped with
/// [`DataSource::SecNport`] and `as_of` equal to the filing's reporting
/// period end date.
pub fn holdings_to_constituents(filing: &NportFiling) -> Vec<Constituent> {
    let as_of = filing
        .header
        .reporting_period_end
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2019, 11, 30).unwrap());
    let mut cs: Vec<Constituent> = filing
        .holdings
        .iter()
        .filter(|h| h.asset_cat.as_deref() == Some("EC"))
        .filter(|h| !h.cusip.is_empty())
        .filter(|h| h.val_usd > 0.0)
        .map(|h| Constituent {
            ticker: None,
            name: h.name.clone(),
            cusip: h.cusip.clone(),
            lei: h.lei.clone(),
            shares: h.balance,
            market_value_usd: h.val_usd,
            // N-PORT pctVal is expressed as a percent (0..100), e.g. 7.22
            // for a 7.22 % weight. Normalise to fraction (0..1) so downstream
            // consumers match the documented Constituent::weight semantics.
            weight: h.pct_val / 100.0,
            issuer_cik: None,
            sector: None,
            as_of,
            source: DataSource::SecNport,
        })
        .collect();
    cs.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    cs
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<edgarSubmission xmlns="http://www.sec.gov/edgar/nport">
  <headerData>
    <filerInfo>
      <seriesClassInfo>
        <seriesId>S000004310</seriesId>
      </seriesClassInfo>
    </filerInfo>
  </headerData>
  <formData>
    <genInfo>
      <seriesName>iShares Core S&amp;P 500 ETF</seriesName>
      <seriesId>S000004310</seriesId>
      <repPdDate>2024-06-30</repPdDate>
    </genInfo>
    <invstOrSecs>
      <invstOrSec>
        <name>Apple Inc.</name>
        <lei>HWUPKR0MPOU8FGXBT394</lei>
        <title>Apple Inc., Common Stock</title>
        <cusip>037833100</cusip>
        <balance>100.0</balance>
        <valUSD>50000.0</valUSD>
        <pctVal>5.0</pctVal>
        <assetCat>EC</assetCat>
      </invstOrSec>
      <invstOrSec>
        <name>Cash</name>
        <cusip>CASHCASH0</cusip>
        <balance>1000.0</balance>
        <valUSD>1000.0</valUSD>
        <pctVal>0.1</pctVal>
        <assetCat>OCS</assetCat>
      </invstOrSec>
      <invstOrSec>
        <name>Microsoft Corp</name>
        <lei>INR2EJN1ERAN0W5ZP974</lei>
        <title>Microsoft Corp, Common Stock</title>
        <cusip>594918104</cusip>
        <balance>50.0</balance>
        <valUSD>30000.0</valUSD>
        <pctVal>3.0</pctVal>
        <assetCat>EC</assetCat>
      </invstOrSec>
    </invstOrSecs>
  </formData>
</edgarSubmission>"#;

    #[test]
    fn parses_header_and_holdings() {
        let f = parse_nport(FIXTURE).unwrap();
        assert_eq!(f.header.series_id.as_deref(), Some("S000004310"));
        assert_eq!(f.header.reporting_period_end.as_deref(), Some("2024-06-30"));
        assert_eq!(f.holdings.len(), 3);
    }

    #[test]
    fn filters_to_common_stock() {
        let f = parse_nport(FIXTURE).unwrap();
        let cs = holdings_to_constituents(&f);
        assert_eq!(cs.len(), 2);
        // Sorted by weight desc -- Apple (0.05) > Microsoft (0.03).
        assert_eq!(cs[0].name, "Apple Inc.");
        assert_eq!(cs[1].name, "Microsoft Corp");
    }

    #[test]
    fn constituent_fields_populated() {
        let f = parse_nport(FIXTURE).unwrap();
        let cs = holdings_to_constituents(&f);
        let apple = &cs[0];
        assert_eq!(apple.cusip, "037833100");
        assert_eq!(apple.lei.as_deref(), Some("HWUPKR0MPOU8FGXBT394"));
        assert!((apple.shares - 100.0).abs() < 1e-9);
        assert!((apple.market_value_usd - 50000.0).abs() < 1e-9);
        // Fixture sets pctVal=5.0 (5.00 %) which normalises to 0.05 fraction.
        assert!((apple.weight - 0.05).abs() < 1e-9);
        // Ticker always None when parsed from N-PORT.
        assert!(apple.ticker.is_none());
        // as_of pulled from repPdDate, source is SecNport.
        assert_eq!(apple.as_of, NaiveDate::from_ymd_opt(2024, 6, 30).unwrap());
        assert_eq!(apple.source, DataSource::SecNport);
    }
}
