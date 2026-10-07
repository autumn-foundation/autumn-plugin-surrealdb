# Changelog

All notable changes to this project follow
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Changed

- **Breaking:** the plugin now depends on `autumn-web` 0.8 (`>=0.8, <0.9`).
  Apps on `autumn-web` 0.7 must stay on 0.1.0. The plugin API that this crate
  uses does not change in 0.8, so the public API of this crate does not
  change. The MSRV stays at 1.88.

## [0.1.0] - 2026-09-29

### Added

- `SurrealDbPlugin` with `.configure(|c| ...)`, `.config_section(...)` and
  `.config(...)` builders. It reads `[surrealdb]` in `autumn.toml`, connects
  at startup, signs in (root, namespace, database, or token), selects the
  namespace and database, and puts the handle in the app state.
- `SurrealDb` handle and extractor: `create_record`, `select_record`,
  `select_all`, `update_record`, `delete_record`, `query_raw`, `query_json`,
  and `version()`. Cloning is cheap; clones share the connection.
- Layered configuration with `AUTUMN_SURREALDB__*` environment overrides and
  secret redaction in `Debug` output.
- Readiness check on `/ready` using the `version()` query.
- `SurrealDbError` with an `ErrorKind`, HTTP status mapping, `or_http()` for
  handlers, and `is_retryable()`.
- `examples/records.rs`: create and query a record, plus document, KV and
  graph query patterns.
