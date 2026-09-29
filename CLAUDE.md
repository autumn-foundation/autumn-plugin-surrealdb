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

## Rules

- The plugin is a **remote client only**. Never enable embedded storage
  features (`kv-mem`, `kv-rocksdb`, `kv-surrealkv`, `kv-tikv`) — they bloat
  compile time enormously. Current deps: `protocol-ws`, `protocol-http`,
  `rustls`.
- No `unwrap`/`expect`/`panic`/`todo`/`unimplemented` in production code —
  the lints deny them.
- Never log or print the password or the token. The config `Debug` impl
  redacts both.
- Autumn API grounding: never answer from training memory — use
  `~/workspace/skills/autumn-mcp/bin/mcp.py` against the docs MCP.
- Docs and comments use ASD-STE100 style: short sentences, active voice,
  simple present tense.
