# Planning: autumn-plugin-surrealdb

## Goal

A SurrealDB remote client for Autumn apps (lane 3: embedded/external crate).
Mark's lane: new plugin ideas in `madmax983`; winners move to
`autumn-foundation`.

## Scope

- Remote client only (`protocol-ws`, `protocol-http`, `rustls`). No embedded
  storage backends — they bloat compile time enormously.
- Document CRUD with `serde`, raw SurrealQL with bound variables, `version()`
  health check, layered `[surrealdb]` config with env-var overrides.
- One crate at the repo root. Private GitHub repo `madmax983/autumn-plugin-surrealdb`.

## Design notes

- The `surrealdb` 3.x SDK generic is `Surreal<C: Connection>`. The client
  keeps an `Engine` enum over the four typed engines (`Ws`, `Wss`, `Http`,
  `Https`) instead of `engine::any::Any`, so the endpoint scheme selects the
  connection at connect time and the type stays exact.
- Auth: token wins; else username+password at the configured `AuthScope`
  (root, namespace, database).
- Every call has a `timeout_ms` deadline. Timeouts give HTTP 504.
- Errors keep the SDK kind (`ErrorKind`) and map to HTTP statuses. The
  password and token never reach logs or `Debug` output.

## Query patterns (documented in README and examples/records.rs)

- Document: `create_record` / `select_record` / `update_record` /
  `delete_record`.
- KV: one document per key in a `kv` table.
- Graph: `RELATE person:ada->knows->person:jaime`, then walk
  `->knows->person.name`.

## Open questions

- Should the plugin expose the typed `surrealdb` CRUD builders (`.content()`,
  `.merge()`, `.patch()`) directly instead of the JSON facade? Current answer:
  JSON facade first; the crate re-exports `surrealdb` for typed escape hatches.
- Session/cache adapters (like rocksdb's) do not fit SurrealDB well; left out.
