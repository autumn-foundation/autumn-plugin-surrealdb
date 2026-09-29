# autumn-plugin-surrealdb

A SurrealDB client for the [Autumn](https://autumn-web.app) web framework.
Register the plugin, then use the `SurrealDb` extractor in a handler.

The plugin speaks to a SurrealDB **server** over the network (WebSocket RPC
or HTTPS). It never embeds a database: the `surrealdb` dependency enables the
remote protocols only (`protocol-ws`, `protocol-http`, `rustls`).

## Quickstart

Add the crate, then wire the plugin in `src/main.rs`:

```rust,ignore
use autumn_plugin_surrealdb::{SurrealDb, SurrealDbPlugin, SurrealDbResultExt as _};
use autumn_web::prelude::*;

#[derive(serde::Deserialize, serde::Serialize)]
struct Profile {
    name: String,
}

#[get("/profiles/{id}")]
async fn profile(db: SurrealDb, Path(id): Path<String>) -> AutumnResult<Json<serde_json::Value>> {
    let profile = db
        .select_record("profile", &id)
        .await
        .or_http()?
        .map(Json)
        .ok_or_else(|| AutumnError::not_found_msg("no profile"));
    profile
}

#[autumn_web::main]
async fn main() {
    autumn_web::app()
        .plugin(SurrealDbPlugin::new().configure(|c| {
            c.endpoint = "127.0.0.1:8000".into();
            c.namespace = "app".into();
            c.database = "app".into();
        }))
        .routes(routes![profile])
        .run()
        .await;
}
```

## Configuration

The plugin reads `[surrealdb]` in `autumn.toml`:

```toml
[surrealdb]
endpoint = "127.0.0.1:8000"   # host:port; or ws://, wss://, http://, https://
protocol = "ws"               # ws or http
tls = false                   # true gives wss:// or https://
namespace = "app"
database = "app"
username = "root"             # or set AUTUMN_SURREALDB__USERNAME
password = "..."              # or set AUTUMN_SURREALDB__PASSWORD
# token = "..."               # a JWT; replaces username/password
auth_scope = "root"           # root, namespace, or database
timeout_ms = 5000             # per-call timeout
health_check = true           # readiness check on /ready
```

Secrets come from `AUTUMN_SURREALDB__PASSWORD` and `AUTUMN_SURREALDB__TOKEN`
environment variables — never from checked-in files. Each layer overrides the
previous one: defaults, `autumn.toml`, `[profile.<name>.surrealdb]`,
`autumn-<profile>.toml`, then the environment.

## What the plugin gives

- **Record helpers**: `create_record`, `select_record`, `select_all`,
  `update_record`, `delete_record` — `serde` types in, JSON out.
- **Raw SurrealQL**: `query_json` runs any query with bound variables
  (`SELECT ... WHERE age > $min_age`). Graph patterns work: relate records
  with `RELATE`, then walk edges with `->edge->record.field`.
- **Key-value pattern**: one document per key in a table (see
  `examples/records.rs`).
- **Extractor**: take `SurrealDb` as a handler argument. It fails with 503
  before the plugin starts.
- **Health check**: `/ready` runs the `version()` query.
- **Errors**: `SurrealDbError` maps to HTTP statuses — 504 for timeouts, 503
  for connection loss, 403 for forbidden, 404 for missing, 400 for bad input.
  Use `.or_http()?` in handlers. Details never show to users.

## Limits

- Needs a reachable SurrealDB server. There is no embedded mode.
- Each call has a `timeout_ms` timeout. A timed-out call returns an error.
- Logs, error text, and `Debug` output never show the password or the token.

## Running the example

```text
surreal start --user root --pass root
export SURREALDB_USER=root SURREALDB_PASS=root
cargo run --example records
```

Unit tests do not need a server. One ignored test (`live_round_trip`)
exercises a server at `127.0.0.1:8000` — run it with `-- --ignored`.

## License

Apache-2.0.
