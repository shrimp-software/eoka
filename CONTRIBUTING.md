# Contributing

Thanks for helping improve eoka.

## Development

Install a recent stable Rust toolchain and Chrome or Chromium if you want to run
browser examples. Unit and regression tests do not require a browser.

```bash
cargo fmt --all --check
cargo clippy --all-features --all-targets -- -D warnings
cargo test --lib
```

For a full local build:

```bash
cargo build --all-features
cargo build --examples
```

## Pull requests

Keep changes focused and include tests for behavior changes. If a change affects
the public API, update `README.md` and `CHANGELOG.md`.

For stealth or CDP changes, include the detector or regression case you used to
validate the behavior.
