# indexkit

Daily index constituents for the S&P 500, S&P 400/600, Nasdaq-100, Dow Jones and Russell 2000, for Rust. No API keys, offline after the first fetch.

```toml
[dependencies]
indexkit = "2.0.0"
```

```rust,no_run
#[tokio::main]
async fn main() -> indexkit::Result<()> {
    let sp500 = indexkit::sp500_latest().await?;
    println!("{} holdings", sp500.len());
    Ok(())
}
```

Full documentation: <https://github.com/kovagent/indexkit>

Licensed under MIT OR Apache-2.0.
