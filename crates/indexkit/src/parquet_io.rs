//! Parquet reader / writer for monthly index constituent snapshots.
//!
//! # File layout
//!
//! ```text
//! data/{index}/
//! |- {index}-YYYY-MM.parquet
//! |- ...
//! ```
//!
//! # Schema
//!
//! | Column | Arrow type | Description |
//! |---|---|---|
//! | `as_of` | `Date32` | Business day the holdings are priced as of |
//! | `source` | `Utf8` | Origin of the row (cdn / wayback_YYYYMMDD / sec_nport) |
//! | `ticker` | `Utf8` nullable | Ticker symbol (present for CDN rows, null for N-PORT) |
//! | `name` | `Utf8` | Security name |
//! | `cusip` | `Utf8` | 9-char CUSIP (primary join key) |
//! | `lei` | `Utf8` nullable | 20-char LEI |
//! | `shares` | `Float64` | Balance |
//! | `market_value_usd` | `Float64` | Fair value USD |
//! | `weight` | `Float64` | Fraction of NAV in 0..1 |
//! | `issuer_cik` | `Utf8` nullable | Reserved |
//!
//! Compression: ZSTD level 3. Row group size: 10 000 rows.
//!
//! # Multi-date rows
//!
//! When daily data is available (sponsor CDN or Wayback), each file
//! contains ~22 (trading days per month) x N (holdings) rows. When only
//! N-PORT monthly data is available the file contains N rows dated
//! `YYYY-MM-last-business-day`.

use arrow::array::{Array, Date32Array, Float64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::NaiveDate;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::types::{Constituent, DataSource};

const ROW_GROUP_SIZE: usize = 10_000;

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("as_of", DataType::Date32, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("ticker", DataType::Utf8, true),
        Field::new("name", DataType::Utf8, false),
        Field::new("cusip", DataType::Utf8, false),
        Field::new("lei", DataType::Utf8, true),
        Field::new("shares", DataType::Float64, false),
        Field::new("market_value_usd", DataType::Float64, false),
        Field::new("weight", DataType::Float64, false),
        Field::new("issuer_cik", DataType::Utf8, true),
    ]))
}

fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).expect("valid zstd level"),
        ))
        .set_max_row_group_row_count(Some(ROW_GROUP_SIZE))
        .build()
}

fn to_date32(d: NaiveDate) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    (d - epoch).num_days() as i32
}

fn from_date32(days: i32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(1970, 1, 1)?.checked_add_signed(chrono::Duration::days(days as i64))
}

/// Write a monthly snapshot to `{data_dir}/{index}/{index}-YYYY-MM.parquet`.
///
/// Any existing file at that path is overwritten. The enclosing directory is
/// created if missing. Rows are sorted by `(as_of, -weight)` on write.
pub fn write_month(
    data_dir: &Path,
    index: &str,
    year_month: &str,
    rows: &[Constituent],
) -> Result<()> {
    let dir = data_dir.join(index);
    fs::create_dir_all(&dir)?;

    let mut sorted: Vec<&Constituent> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        a.as_of.cmp(&b.as_of).then(
            b.weight
                .partial_cmp(&a.weight)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    let as_of: Date32Array = sorted.iter().map(|r| Some(to_date32(r.as_of))).collect();
    let source: StringArray = sorted.iter().map(|r| Some(r.source.tag())).collect();
    let tickers: StringArray = sorted.iter().map(|r| r.ticker.as_deref()).collect();
    let names: StringArray = sorted.iter().map(|r| Some(r.name.as_str())).collect();
    let cusips: StringArray = sorted.iter().map(|r| Some(r.cusip.as_str())).collect();
    let leis: StringArray = sorted.iter().map(|r| r.lei.as_deref()).collect();
    let shares: Float64Array = sorted.iter().map(|r| Some(r.shares)).collect();
    let mv: Float64Array = sorted.iter().map(|r| Some(r.market_value_usd)).collect();
    let weights: Float64Array = sorted.iter().map(|r| Some(r.weight)).collect();
    let ciks: StringArray = sorted.iter().map(|r| r.issuer_cik.as_deref()).collect();

    let batch = RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(as_of),
            Arc::new(source),
            Arc::new(tickers),
            Arc::new(names),
            Arc::new(cusips),
            Arc::new(leis),
            Arc::new(shares),
            Arc::new(mv),
            Arc::new(weights),
            Arc::new(ciks),
        ],
    )?;

    let path = dir.join(format!("{index}-{year_month}.parquet"));
    let file = fs::File::create(&path)?;
    let mut w = ArrowWriter::try_new(file, schema(), Some(writer_props()))?;
    w.write(&batch)?;
    w.close()?;
    tracing::info!("wrote {} rows -> {}", sorted.len(), path.display());
    Ok(())
}

/// Read a monthly snapshot parquet into a vec of [`Constituent`].
///
/// Rows are returned in the order they appear in the file (i.e. sorted
/// `(as_of, -weight)`).
pub fn read_month(path: &Path) -> Result<Vec<Constituent>> {
    let file = fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let reader = builder.build()?;

    let mut out: Vec<Constituent> = Vec::new();
    for batch in reader {
        let batch = batch?;
        let n = batch.num_rows();

        let as_of = batch
            .column(0)
            .as_any()
            .downcast_ref::<Date32Array>()
            .ok_or_else(|| Error::Parquet("as_of column type mismatch".into()))?;
        let source = column_as_str(&batch, 1, "source")?;
        let ticker = column_as_str(&batch, 2, "ticker")?;
        let name = column_as_str(&batch, 3, "name")?;
        let cusip = column_as_str(&batch, 4, "cusip")?;
        let lei = column_as_str(&batch, 5, "lei")?;
        let shares = column_as_f64(&batch, 6, "shares")?;
        let mv = column_as_f64(&batch, 7, "market_value_usd")?;
        let weight = column_as_f64(&batch, 8, "weight")?;
        let cik = column_as_str(&batch, 9, "issuer_cik")?;

        for i in 0..n {
            let d = from_date32(as_of.value(i))
                .ok_or_else(|| Error::Parquet("bad Date32 value".into()))?;
            let src = DataSource::from_tag(source.value(i)).ok_or_else(|| {
                Error::Parquet(format!("unknown source tag: {}", source.value(i)))
            })?;
            out.push(Constituent {
                ticker: opt_str(ticker, i),
                name: name.value(i).to_string(),
                cusip: cusip.value(i).to_string(),
                lei: opt_str(lei, i),
                shares: shares.value(i),
                market_value_usd: mv.value(i),
                weight: weight.value(i),
                issuer_cik: opt_str(cik, i),
                sector: None,
                as_of: d,
                source: src,
            });
        }
    }
    Ok(out)
}

fn column_as_str<'a>(batch: &'a RecordBatch, idx: usize, name: &str) -> Result<&'a StringArray> {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::Parquet(format!("{name} column type mismatch")))
}

fn column_as_f64<'a>(batch: &'a RecordBatch, idx: usize, name: &str) -> Result<&'a Float64Array> {
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or_else(|| Error::Parquet(format!("{name} column type mismatch")))
}

fn opt_str(col: &StringArray, i: usize) -> Option<String> {
    if col.is_null(i) {
        None
    } else {
        Some(col.value(i).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> Vec<Constituent> {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        vec![
            Constituent {
                ticker: Some("AAPL".into()),
                name: "Apple Inc.".to_string(),
                cusip: "037833100".to_string(),
                lei: Some("HWUPKR0MPOU8FGXBT394".to_string()),
                shares: 100.0,
                market_value_usd: 50_000.0,
                weight: 0.05,
                issuer_cik: None,
                sector: None,
                as_of: d,
                source: DataSource::IsharesCdn,
            },
            Constituent {
                ticker: None,
                name: "Microsoft Corp".to_string(),
                cusip: "594918104".to_string(),
                lei: None,
                shares: 50.0,
                market_value_usd: 30_000.0,
                weight: 0.03,
                issuer_cik: None,
                sector: None,
                as_of: d,
                source: DataSource::SecNport,
            },
        ]
    }

    #[test]
    fn write_read_roundtrip() {
        let d = tempdir().unwrap();
        let data = d.path();
        let rows = sample();

        write_month(data, "sp500", "2024-01", &rows).unwrap();

        let out = read_month(&data.join("sp500").join("sp500-2024-01.parquet")).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].cusip, "037833100");
        assert_eq!(out[0].ticker.as_deref(), Some("AAPL"));
        assert_eq!(out[0].source, DataSource::IsharesCdn);
        assert_eq!(out[1].source, DataSource::SecNport);
        assert_eq!(out[0].as_of, NaiveDate::from_ymd_opt(2024, 1, 15).unwrap());
    }
}
