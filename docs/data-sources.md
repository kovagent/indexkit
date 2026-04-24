# Data sources

indexkit pulls from SEC EDGAR, the only authoritative public-domain source
of ETF portfolio holdings.

## SEC EDGAR N-PORT

**Publisher:** US Securities and Exchange Commission

**Form:** N-PORT-P (Monthly Portfolio Investments)

**Rule:** [30b1-9 under the Investment Company Act of 1940](https://www.sec.gov/rules-regulations/2016/10/modernization-information-reported-registered-investment-companies).
Investment companies must file Form N-PORT within 60 days after the end of
each month. The third month of each quarter is made publicly available 60
days after filing (so a June 30 holdings file becomes public approximately
August 29 -- a ~60 day lag). For non-quarter-ending months the SEC holds
the filing for an additional 60 days before release, so you may see an
additional 30-day lag on the intermediate months relative to the quarter-
end months.

**User-Agent:** SEC mandates a descriptive UA for all `data.sec.gov` and
`www.sec.gov/Archives` requests. The Akamai edge rejects anonymous,
`noreply@`, and library-style `(+url)` UAs. indexkit uses the format
`indexkit <contact-email>`, overridable via `INDEXKIT_SEC_USER_AGENT`.

**Rate limit:** 10 requests / second per IP. indexkit inserts a 120 ms
sleep between requests to stay comfortably under the limit.

### Endpoints

1. **Submissions feed** (list filings for a CIK):
   ```
   https://data.sec.gov/submissions/CIK{10-digit-zero-padded-cik}.json
   ```
   Returns JSON with fields `filings.recent.form`, `filingDate`,
   `accessionNumber`, `reportDate`. For CIKs with more than ~1000 total
   filings, older ones live in paginated archives listed under
   `filings.files[].name`.

2. **Filing document** (XML):
   ```
   https://www.sec.gov/Archives/edgar/data/{cik-no-leading-zeros}/{accession-no-dashes}/primary_doc.xml
   ```
   One XML per filing. Multi-MB for large-universe ETFs (IVV is ~4 MB with
   500+ holdings).

### Publication schedule

| Reporting month end | Filing deadline (T + 60d) | Public release (T + 90d) |
|---|---|---|
| 2024-06-30 | 2024-08-29 | 2024-09-28 (quarter end, fast release) |
| 2024-07-31 | 2024-09-29 | ~2024-10-29 |
| 2024-08-31 | 2024-10-30 | ~2024-11-29 |
| 2024-09-30 | 2024-11-29 | 2024-12-29 (quarter end) |

So for downstream consumers, "latest" is typically 2-3 months behind real
time. indexkit cannot close that gap -- it is imposed by the SEC for
investor-protection reasons.

### The five index proxies

Each of the five indices we support is tracked via a single iconic ETF.
The CIK and series ID for each proxy are verified against live SEC data
(see `data/cik-map.json`):

| Index | Ticker | ETF name | Trust CIK | Series ID |
|---|---|---|---|---|
| S&P 500 | IVV | iShares Core S&P 500 ETF | 0001100663 | S000004310 |
| S&P MidCap 400 | IJH | iShares Core S&P Mid-Cap ETF | 0001100663 | S000004307 |
| S&P SmallCap 600 | IJR | iShares Core S&P Small-Cap ETF | 0001100663 | S000004313 |
| Nasdaq-100 | QQQ | Invesco QQQ Trust, Series 1 | 0001067839 | S000101292 |
| DJIA | DIA | SPDR Dow Jones Industrial Average ETF Trust | 0001041130 | *none* (single-series trust) |

iShares Trust (CIK 0001100663) is a multi-series trust hosting ~130 ETFs;
indexkit must open each filing's `primary_doc.xml` to match the target
series ID. iShares Trust files hundreds of NPORT-P per month, so the
backfill is I/O-heavy on the SEC side.

QQQ's CIK 0001067839 hosts only Invesco QQQ Trust Series 1, so every
NPORT-P filing for that CIK is for QQQ. Same for DIA (CIK 0001041130).

### Coverage

N-PORT public release began in Q4 2019 (first data month: October 2019,
first public release: November 2019). Prior to that, ETFs filed Form N-Q
on a quarterly basis with less granular detail. indexkit does NOT read
N-Q -- coverage starts Nov 2019.

## Rate conversion

Unlike the Treasury and SOFR curves in our sister library `curvekit`,
N-PORT values are already denominated correctly: USD market value, share
counts, and NAV fractions. No rate conversions are needed.

`pctVal` in N-PORT is expressed as a fraction (e.g. 0.073 for a 7.3%
weight), so indexkit passes it through unchanged into
`Constituent::weight`.

## Attribution

SEC EDGAR filings are US federal records in the public domain. No license
restrictions apply to downstream use of the raw filings, and therefore
none apply to the parquet derivatives shipped here -- indexkit releases
all derived data under Apache-2.0 to match the code.
