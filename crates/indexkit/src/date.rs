//! [`YearMonth`] -- month resolution for index constituent snapshots.
//!
//! N-PORT filings are published monthly, so indexkit's date resolution is
//! year-month, not calendar-day. The [`YearMonth`] type is a thin value type
//! supporting many construction paths so callers never need to import `chrono`.
//!
//! # Examples
//!
//! ```
//! use indexkit::YearMonth;
//!
//! // From string (ISO-like)
//! let ym1: YearMonth = "2024-01".parse().unwrap();
//! // From compact string
//! let ym2: YearMonth = "202401".parse().unwrap();
//! // From components
//! let ym3 = YearMonth::new(2024, 1).unwrap();
//! // From u32 YYYYMM
//! let ym4 = YearMonth::from_yyyymm(202401).unwrap();
//!
//! assert_eq!(ym1, ym3);
//! assert_eq!(ym1.to_string(), "2024-01");
//! ```
//!
//! # Macro
//!
//! The `ym!` macro provides a terse literal form in Rust code:
//!
//! ```
//! use indexkit::{ym, YearMonth};
//!
//! let y = ym!(2024, 1);
//! assert_eq!(y, YearMonth::new(2024, 1).unwrap());
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Error produced when a value cannot be converted into a [`YearMonth`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YearMonthError(String);

impl fmt::Display for YearMonthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid year-month: {}", self.0)
    }
}

impl std::error::Error for YearMonthError {}

/// Year + month pair (month in `1..=12`).
///
/// Used by indexkit public API for snapshot selection since N-PORT filings
/// are monthly. Ordered, comparable, hashable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct YearMonth {
    year: i32,
    month: u32,
}

impl YearMonth {
    /// Construct from year + month components.
    ///
    /// # Errors
    ///
    /// Returns [`YearMonthError`] if `month` is not in `1..=12` or `year` is
    /// unreasonable (`< 1900` or `> 2200`).
    ///
    /// # Example
    ///
    /// ```
    /// use indexkit::YearMonth;
    /// let ym = YearMonth::new(2024, 1).unwrap();
    /// assert_eq!(ym.to_string(), "2024-01");
    /// ```
    pub fn new(year: i32, month: u32) -> Result<Self, YearMonthError> {
        if !(1..=12).contains(&month) {
            return Err(YearMonthError(format!("month {month} not in 1..=12")));
        }
        if !(1900..=2200).contains(&year) {
            return Err(YearMonthError(format!(
                "year {year} out of plausible range 1900..=2200"
            )));
        }
        Ok(Self { year, month })
    }

    /// Construct from a compact `u32` in `YYYYMM` form (e.g. `202401`).
    ///
    /// # Example
    ///
    /// ```
    /// use indexkit::YearMonth;
    /// let ym = YearMonth::from_yyyymm(202401).unwrap();
    /// assert_eq!(ym.to_string(), "2024-01");
    /// ```
    pub fn from_yyyymm(v: u32) -> Result<Self, YearMonthError> {
        let y = (v / 100) as i32;
        let m = v % 100;
        Self::new(y, m)
    }

    /// The year component.
    #[inline]
    pub fn year(self) -> i32 {
        self.year
    }

    /// The month component (`1..=12`).
    #[inline]
    pub fn month(self) -> u32 {
        self.month
    }

    /// Current month in UTC.
    pub fn current_utc() -> Self {
        use chrono::{Datelike, Utc};
        let now = Utc::now();
        Self {
            year: now.year(),
            month: now.month(),
        }
    }

    /// The month preceding `self`. Wraps from Jan -> Dec of previous year.
    pub fn prev(self) -> Self {
        if self.month == 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    /// The month following `self`. Wraps from Dec -> Jan of next year.
    pub fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    /// Iterate every month from `self` up to and including `end`.
    ///
    /// Empty iterator if `end < self`.
    pub fn iter_to(self, end: YearMonth) -> impl Iterator<Item = YearMonth> {
        let mut cur = Some(self);
        std::iter::from_fn(move || {
            let c = cur?;
            if c > end {
                cur = None;
                return None;
            }
            cur = Some(c.next());
            Some(c)
        })
    }
}

// ---- conversions ----

/// Parse a year-month string.
///
/// Accepted forms:
/// - `"YYYY-MM"` (e.g. `"2024-01"`)
/// - `"YYYY/MM"` (e.g. `"2024/01"`)
/// - `"YYYYMM"` compact (e.g. `"202401"`)
impl FromStr for YearMonth {
    type Err = YearMonthError;

    fn from_str(s: &str) -> Result<Self, YearMonthError> {
        let s = s.trim();
        if s.len() == 7 && (s.as_bytes()[4] == b'-' || s.as_bytes()[4] == b'/') {
            let y: i32 = s[..4]
                .parse()
                .map_err(|_| YearMonthError(format!("cannot parse year in {s:?}")))?;
            let m: u32 = s[5..]
                .parse()
                .map_err(|_| YearMonthError(format!("cannot parse month in {s:?}")))?;
            return Self::new(y, m);
        }
        if s.len() == 6 && s.bytes().all(|b| b.is_ascii_digit()) {
            let v: u32 = s
                .parse()
                .map_err(|_| YearMonthError(format!("cannot parse YYYYMM {s:?}")))?;
            return Self::from_yyyymm(v);
        }
        Err(YearMonthError(format!(
            "{s:?} is not a recognised form: expected YYYY-MM, YYYY/MM, or YYYYMM"
        )))
    }
}

impl fmt::Display for YearMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.year, self.month)
    }
}

// ---- IntoYearMonth sealed trait ----

mod private {
    pub trait Sealed {}
}

/// Convert a value into a [`YearMonth`].
///
/// Implemented for:
/// - `YearMonth` (identity)
/// - `&str`, `String` -- parses the three accepted forms
/// - `u32` -- interpreted as YYYYMM
/// - `(i32, u32)`, `(u32, u32)` -- year, month components
///
/// Used as the bound on all public year-month parameters:
/// ```text
/// pub async fn constituents(&self, index: &str, ym: impl IntoYearMonth) -> ...
/// ```
pub trait IntoYearMonth: private::Sealed {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError>;
}

impl private::Sealed for YearMonth {}
impl IntoYearMonth for YearMonth {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError> {
        Ok(self)
    }
}

impl private::Sealed for &str {}
impl IntoYearMonth for &str {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError> {
        self.parse()
    }
}

impl private::Sealed for String {}
impl IntoYearMonth for String {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError> {
        self.parse()
    }
}

impl private::Sealed for u32 {}
impl IntoYearMonth for u32 {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError> {
        YearMonth::from_yyyymm(self)
    }
}

impl private::Sealed for (i32, u32) {}
impl IntoYearMonth for (i32, u32) {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError> {
        YearMonth::new(self.0, self.1)
    }
}

impl private::Sealed for (u32, u32) {}
impl IntoYearMonth for (u32, u32) {
    fn into_year_month(self) -> Result<YearMonth, YearMonthError> {
        YearMonth::new(self.0 as i32, self.1)
    }
}

/// Terse literal form for a [`YearMonth`].
///
/// # Example
///
/// ```
/// use indexkit::{ym, YearMonth};
/// let y = ym!(2024, 1);
/// assert_eq!(y, YearMonth::new(2024, 1).unwrap());
/// ```
///
/// Panics on invalid inputs -- intended for literal constants. For runtime
/// values use [`YearMonth::new`] which returns a `Result`.
#[macro_export]
macro_rules! ym {
    ($y:expr, $m:expr) => {{
        $crate::YearMonth::new($y as i32, $m as u32).expect("invalid year-month literal")
    }};
}

// ---- tests ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso() {
        let y: YearMonth = "2024-01".parse().unwrap();
        assert_eq!(y.year(), 2024);
        assert_eq!(y.month(), 1);
    }

    #[test]
    fn parse_compact() {
        let y: YearMonth = "202412".parse().unwrap();
        assert_eq!(y.year(), 2024);
        assert_eq!(y.month(), 12);
    }

    #[test]
    fn parse_slashed() {
        let y: YearMonth = "2024/07".parse().unwrap();
        assert_eq!(y.month(), 7);
    }

    #[test]
    fn rejects_bad_month() {
        assert!(YearMonth::new(2024, 0).is_err());
        assert!(YearMonth::new(2024, 13).is_err());
    }

    #[test]
    fn rejects_bad_year() {
        assert!(YearMonth::new(1800, 1).is_err());
        assert!(YearMonth::new(3000, 1).is_err());
    }

    #[test]
    fn display_roundtrip() {
        let y = YearMonth::new(2019, 11).unwrap();
        assert_eq!(y.to_string(), "2019-11");
        let back: YearMonth = y.to_string().parse().unwrap();
        assert_eq!(back, y);
    }

    #[test]
    fn prev_next_wrap() {
        let y = YearMonth::new(2020, 1).unwrap();
        let p = y.prev();
        assert_eq!(p.year(), 2019);
        assert_eq!(p.month(), 12);
        let n = YearMonth::new(2020, 12).unwrap().next();
        assert_eq!(n.year(), 2021);
        assert_eq!(n.month(), 1);
    }

    #[test]
    fn iter_to_inclusive() {
        let a = YearMonth::new(2020, 1).unwrap();
        let b = YearMonth::new(2020, 3).unwrap();
        let v: Vec<_> = a.iter_to(b).collect();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], a);
        assert_eq!(v[2], b);
    }

    #[test]
    fn iter_to_empty_when_reversed() {
        let a = YearMonth::new(2020, 5).unwrap();
        let b = YearMonth::new(2020, 3).unwrap();
        assert_eq!(a.iter_to(b).count(), 0);
    }

    #[test]
    fn ordering() {
        let a = YearMonth::new(2020, 1).unwrap();
        let b = YearMonth::new(2020, 12).unwrap();
        let c = YearMonth::new(2021, 1).unwrap();
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn ym_macro() {
        let y = ym!(2024, 1);
        assert_eq!(y, YearMonth::new(2024, 1).unwrap());
    }

    #[test]
    fn from_yyyymm() {
        let y = YearMonth::from_yyyymm(202407).unwrap();
        assert_eq!(y.year(), 2024);
        assert_eq!(y.month(), 7);
    }

    #[test]
    fn into_year_month_u32() {
        let y = 202401u32.into_year_month().unwrap();
        assert_eq!(y.to_string(), "2024-01");
    }

    #[test]
    fn into_year_month_tuple() {
        let y = (2024i32, 7u32).into_year_month().unwrap();
        assert_eq!(y.to_string(), "2024-07");
    }
}
