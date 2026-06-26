# indexkit

Daily index constituents for the S&P 500, S&P 400/600, Nasdaq-100 and Dow Jones, for Rust. No API keys, offline after the first fetch.

```toml
[dependencies]
indexkit = "1.0.1"
```

```rust,no_run
#[tokio::main]
async fn main() -> indexkit::Result<()> {
    let sp500 = indexkit::sp500_latest().await?;
    println!("{} holdings", sp500.len());
    Ok(())
}
```

Full documentation: <https://github.com/userFRM/indexkit>

Licensed under MIT OR Apache-2.0.
