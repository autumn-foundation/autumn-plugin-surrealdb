# autumn-plugin-surrealdb

A SurrealDB client for the [Autumn](https://autumn-web.app) web framework.
Register the plugin, then use the `SurrealDb` extractor in a handler.

The plugin speaks to a SurrealDB **server** over the network (WebSocket RPC
or HTTPS). By default it embeds no database: the `surrealdb` dependency
enables the remote protocols only (`protocol-ws`, `protocol-http`, `rustls`).
The opt-in `fleet` feature adds embedded SurrealKV databases, one for each
tenant or slot. See [Fleets](#fleets).

## Quickstart

Add the crate, then wire the plugin in `src/main.rs`:

```rust,no_run
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

## Fleets

The `fleet` feature gives each tenant (or each slot) its own embedded
SurrealKV database. Autumn core runs the fleet: keys, paths, opens, idle
closes, the memory budget, slot ownership and fencing. The plugin gives the
SurrealKV engine and the `FleetSurrealDb` extractor. The feature adds the
`kv-surrealkv` engine of the `surrealdb` crate, so the first build takes
longer. The fleet needs an Autumn database backend: turn on the `postgres`
or the `sqlite` feature of `autumn-web`.

```toml
[dependencies]
autumn-plugin-surrealdb = { version = "0.4", features = ["fleet"] }
```

```rust,no_run
# #[cfg(feature = "fleet")]
# mod example {
use autumn_plugin_surrealdb::fleet::{FleetSurrealDb, Migration, SurrealDbFleetPlugin};
use autumn_plugin_surrealdb::SurrealDbResultExt as _;
use autumn_web::prelude::*;

const NOTES: Migration = Migration::new("0001_notes", "DEFINE TABLE IF NOT EXISTS note SCHEMALESS");

#[get("/notes/count")]
async fn count(db: FleetSurrealDb) -> AutumnResult<String> {
    let notes = db.select_all("note").await.or_http()?;
    Ok(notes.len().to_string())
}

pub async fn run() {
    autumn_web::app()
        .plugin(SurrealDbFleetPlugin::new().migrations([NOTES]))
        .routes(routes![count])
        .run()
        .await;
}
# }
# fn main() {}
```

`FleetSurrealDb` derefs to `SurrealDb`, so the record helpers and
`query_json` work the same. The plugin reads `[surrealdb_fleet]`, with the
same layers as `[surrealdb]`. The Autumn fleet keys (as in
`[database.fleet]`) and the engine keys share the section:

```toml
[surrealdb_fleet]
mode = "tenant"                      # One database for each tenant. "slot" gives one for each slot.
root = "data/tenants"
create_on_demand = true
max_resident_bytes = 1073741824      # The memory budget of all open databases.
namespace = "app"                    # The namespace that each handle selects.
database = "app"                     # The database that each handle selects.
timeout_ms = 5000                    # The call timeout. The open and the snapshot use it too.
ship_every_secs = 60                 # The interval between two replica generations.
resident_bytes = 67108864            # The memory that one open database counts.

[surrealdb_fleet.ownership]
owner = "host-a"
```

`AUTUMN_SURREALDB_FLEET__ROOT` and `AUTUMN_SURREALDB_FLEET__OWNERSHIP__OWNER`
set `root` and `ownership.owner`.

- The extractor opens the database of the request tenant. It gives `400`
  with no tenant and `421` when another host owns the slot. In a job, use
  `FleetSurrealDb::for_key(&state, tenant)`.
- Each database is a directory. The SurrealKV store is in `store/`.
- The plugin applies the migrations to each database when it opens. The
  table `autumn_fleet_migration` records them. Write each step so that a
  second run does no harm.
- The record `autumn_fleet_identity:stamp` holds the tenant key and the
  ownership epoch. A database of another key, or of a newer epoch, does not
  open.
- A snapshot is a SurrealQL export (`snapshot.surql`). Unchanged data gives
  the same bytes, so a replica ships only after a write. The next open after
  a restore imports the export into a new store.
- The plugin closes idle databases, and all databases at shutdown, in each
  role.
- The engine passes the Autumn fleet conformance kit
  (`autumn_web::plugin_conformance::fleet`).

## Limits

- `SurrealDbPlugin` needs a reachable SurrealDB server. Only the `fleet`
  feature embeds databases.
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
