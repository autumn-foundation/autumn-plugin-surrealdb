# CLAUDE.md — autumn-plugin-surrealdb

## Commands

Run all with `CARGO_BUILD_JOBS=1` (shared box, 7.7 GiB RAM, no swap) and the
shared arena target dir. Never `cargo clean` the target dir.

```text
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=1
export CARGO_TARGET_DIR=~/workspace/autumn-arena/target
export TMPDIR=~/workspace/.tmp-cargo   # /tmp is a 512MB tmpfs

cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

The test suite does not need a live server. One ignored test
(`client::tests::live_round_trip`) does — run it with `-- --ignored` against
a local `surreal start --user root --pass root`.

## Architecture

| Module   | Owns |
| -------- | ---- |
| `config` | `SurrealDbConfig`: layered `[surrealdb]` section + env overrides, validation, secret redaction in `Debug` |
| `error`  | `SurrealDbError` + `ErrorKind`: surrealdb error mapping, HTTP status mapping, `or_http()` |
| `client` | `SurrealDb`: connect/signin/use_ns/use_db, record CRUD, `query_raw`/`query_json`, `version()` |
| `plugin` | `SurrealDbPlugin`: `Plugin::build`, startup hook, `FromRequestParts` extractor |
| `health` | `SurrealDbCheck`: readiness check with a `version()` round trip |
| `fleet` | `SurrealKvEngine`, `SurrealDbFleetPlugin`, `FleetSurrealDb` (feature `fleet`): one embedded SurrealKV database for each tenant or slot |

## Rules

- README Rust snippets are doctests. Make them compile; use `no_run`, not `ignore`.
  `tests/doc_snippets.rs` checks the TOML snippets and the version pins in the docs.
- By default the plugin is a **remote client only** (`protocol-ws`,
  `protocol-http`, `rustls`). Only the opt-in `fleet` feature adds an
  embedded backend: `kv-surrealkv`. Never add `kv-mem`, `kv-rocksdb` or
  `kv-tikv`, and never put `kv-surrealkv` in the default features. Embedded
  backends add a lot of compile time.
- A fleet snapshot is a SurrealQL export, not a copy of the store. A
  SurrealKV store changes its files on each open, so a file copy is not the
  same bytes for unchanged data. Keep the export deterministic.
- A closed SurrealKV client lets go of its store in the background. Code
  that opens, copies or deletes a store waits for the `LOCK` file first.
- No `unwrap`/`expect`/`panic`/`todo`/`unimplemented` in production code —
  the lints deny them.
- Never log or print the password or the token. The config `Debug` impl
  redacts both.
- Autumn API grounding: never answer from training memory — use
  `~/workspace/skills/autumn-mcp/bin/mcp.py` against the docs MCP.
- Docs and comments use ASD-STE100 style: short sentences, active voice,
  simple present tense.
