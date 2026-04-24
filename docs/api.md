# API reference

Full method signatures for `indexkit::Indexkit` and the free functions. For
type definitions see the rustdoc at [docs.rs/indexkit](https://docs.rs/indexkit).

## Client construction

```rust
// Infallible. If HTTP client fails to build (TLS init on exotic platforms),
// error is deferred to the first fetch.
let client = indexkit::Indexkit::new();

// Fallible, for early detection.
let client = indexkit::Indexkit::try_new()?;

// Builder form -- override origin / mirror / cache.
let client = indexkit::Indexkit::new()
    .with_base_url("https://my-mirror.example.com/indexkit")
    .with_mirror_url(Some("https://my-cdn.example.com/indexkit".into()))
    .with_cache_dir(std::path::PathBuf::from("/tmp/indexkit"));
```

Environment overrides:

| Variable | Effect |
|---|---|
| `INDEXKIT_BASE_URL` | Primary origin (default: GitHub raw of userFRM/indexkit) |
| `INDEXKIT_CACHE_DIR` | Cache directory (default: XDG cache) |
| `INDEXKIT_MIRROR_URL` | CDN mirror (default: jsDelivr) |
| `INDEXKIT_SEC_USER_AGENT` | User-Agent for SEC requests (default: "indexkit ${contact}") |

## Per-index sugar methods

All five indices expose the same three methods:

```rust
client.sp500(ym).await?                // Vec<Constituent>
client.sp500_latest().await?           // Vec<Constituent>
client.sp500_range(start, end).await?  // Vec<IndexSnapshot>
```

Replace `sp500` with `sp400`, `sp600`, `ndx`, or `dji` as appropriate.

## Generic methods

```rust
// Any supported index by short id string.
client.constituents("sp500", ym).await?       // Vec<Constituent>
client.constituents("ndx",   ym).await?
client.constituents("dji",   ym).await?

// Typed IndexId enum.
client.constituents_by_id(IndexId::Sp500, ym).await?

// Full snapshot (index + month + holdings).
client.snapshot(IndexId::Ndx, ym).await?      // IndexSnapshot

// Convenience lookup: weight by CUSIP (preferred) or name substring.
client.weight("037833100", "sp500", ym).await?   // Option<f64>

// Ticker list -- always empty in v1.0.
client.tickers("sp500", ym).await?            // Vec<String>
```

## Blocking (sync) methods

Every async method has a `*_blocking()` twin that drives the future to
completion from either sync or async contexts. Uses
`tokio::task::block_in_place` + `Handle::block_on` inside a runtime, or a
minimal current-thread runtime outside.

```rust
let cs = client.sp500_blocking(ym!(2024, 1))?;
let cs = client.constituents_blocking("ndx", "2024-01")?;
```

## Free functions

For one-off scripts, use the process-wide shared client (no setup):

```rust
indexkit::sp500_latest().await?
indexkit::ndx_latest().await?
indexkit::dji_latest().await?
indexkit::constituents_for(IndexId::Sp500, ym!(2024, 1)).await?
indexkit::sp500_tickers_latest().await?    // empty in v1.0
```

## Error model

Every public method returns `indexkit::Result<T>` = `Result<T, indexkit::Error>`.

Match on `indexkit::Error`:

```rust
use indexkit::Error;

match client.sp500(ym!(2024, 1)).await {
    Ok(cs) => println!("{} holdings", cs.len()),
    Err(Error::SnapshotNotFound { .. }) => println!("no data for that month"),
    Err(Error::ChecksumMismatch { file, .. }) => eprintln!("tampered file: {file}"),
    Err(Error::Http(e)) => eprintln!("network: {e}"),
    Err(e) => eprintln!("{e}"),
}
```

## Types

### `YearMonth`

```rust
YearMonth::new(2024, 1)?            // explicit
YearMonth::from_yyyymm(202401)?     // compact u32
"2024-01".parse::<YearMonth>()?     // ISO string
ym!(2024, 1)                        // literal macro
```

Implements `IntoYearMonth` for `YearMonth`, `&str`, `String`, `u32`,
`(i32, u32)`, `(u32, u32)`.

### `Constituent`

```rust
pub struct Constituent {
    pub ticker: Option<String>,      // populated by CDN / GitHub-mirror sources;
                                     // None for SEC N-PORT rows.
    pub name: String,                // empty for GitHub-mirror rows
    pub cusip: String,               // 9 chars for CDN / Wayback / N-PORT;
                                     // empty string for GitHub mirrors.
    pub lei: Option<String>,
    pub shares: f64,                 // 0.0 for GitHub mirrors
    pub market_value_usd: f64,       // 0.0 for GitHub mirrors
    pub weight: f64,                 // f64::NAN for GitHub mirrors; use weight_opt()
    pub issuer_cik: Option<String>,
    pub sector: Option<Sector>,      // reserved for v1.1
    // + as_of: NaiveDate, source: DataSource
}
```

Accessors for missing data:

```rust
c.weight_opt() -> Option<f64>   // returns None when weight is NaN
snap.has_weights() -> bool      // true if at least one finite weight
```

### `IndexSnapshot`

```rust
pub struct IndexSnapshot {
    pub index: IndexId,
    pub year_month: YearMonth,
    pub constituents: Vec<Constituent>,  // sorted by weight desc
}
```

### `IndexId`

```rust
pub enum IndexId {
    Sp500, Sp400, Sp600, Ndx, Dji,
}
```

String aliases accepted by `IndexId::from_str_id`:
`"sp500"`, `"sp400"`, `"sp600"`, `"ndx" | "nasdaq100" | "nasdaq-100"`,
`"dji" | "djia" | "dow"` (case-insensitive).
