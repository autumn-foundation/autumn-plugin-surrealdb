//! [`SurrealDb`]: the SurrealDB handle for Autumn apps.
//!
//! # Contract
//!
//! - The plugin speaks to a SurrealDB server over the network. It never embeds
//!   a database: the `surrealdb` dependency enables the remote protocols only.
//! - `connect` opens one connection, signs in, and selects the namespace and
//!   the database. The handle is cheap to clone and shares the connection.
//! - The record helpers speak `serde`: values go in as JSON and come back as
//!   `serde_json::Value`. SurrealDB types without a JSON counterpart convert
//!   on a best-effort basis (record ids, datetimes and durations become
//!   strings, a non-finite float becomes `null`).
//! - `create_record` merges nothing: it writes the whole document. Use
//!   `update_record` to merge fields into an existing document.
//! - Every call has the `timeout_ms` timeout. A timed-out call returns an error.
//! - The error text never shows the password or the token.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use surrealdb::engine::remote::http::{Client as HttpClient, Http, Https};
use surrealdb::engine::remote::ws::{Client as WsClient, Ws, Wss};
use surrealdb::opt::auth;
use surrealdb::{Connection, Surreal};

use crate::config::{AuthScope, Protocol, SurrealDbConfig};
use crate::error::SurrealDbError;

/// The connected engine. The protocol picks the variant; the scheme type
/// (`Ws` vs `Wss`, `Http` vs `Https`) picks the TLS flag.
#[derive(Clone)]
enum Engine {
    Ws(Surreal<WsClient>),
    Http(Surreal<HttpClient>),
}

struct Inner {
    engine: Engine,
    config: SurrealDbConfig,
}

/// The SurrealDB handle. Clone it freely: clones share the connection.
///
/// Get it in a handler with the [`SurrealDb`] extractor, or outside a handler
/// with [`SurrealDb::from_state`].
///
/// The `Debug` output shows the (redacted) configuration only.
#[derive(Clone)]
pub struct SurrealDb {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SurrealDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurrealDb")
            .field("config", self.config())
            .finish()
    }
}

/// Runs a SurrealDB future with the timeout, converting the error.
async fn call<T>(
    timeout: Duration,
    future: impl Future<Output = Result<T, surrealdb::Error>>,
) -> Result<T, SurrealDbError> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| SurrealDbError::Timeout { timeout })?
        .map_err(SurrealDbError::from)
}

/// Signs in and selects the namespace and the database.
async fn finish<C: Connection>(
    db: Surreal<C>,
    config: &SurrealDbConfig,
) -> Result<Surreal<C>, SurrealDbError> {
    if let Some(token) = &config.token {
        db.authenticate(token.clone()).await?;
    } else if let (Some(username), Some(password)) = (&config.username, &config.password) {
        match config.auth_scope {
            AuthScope::Root => {
                let credentials = auth::Root {
                    username: username.clone(),
                    password: password.clone(),
                };
                db.signin(credentials).await?;
            }
            AuthScope::Namespace => {
                let credentials = auth::Namespace {
                    namespace: config.namespace.clone(),
                    username: username.clone(),
                    password: password.clone(),
                };
                db.signin(credentials).await?;
            }
            AuthScope::Database => {
                let credentials = auth::Database {
                    namespace: config.namespace.clone(),
                    database: config.database.clone(),
                    username: username.clone(),
                    password: password.clone(),
                };
                db.signin(credentials).await?;
            }
        }
    }
    db.use_ns(config.namespace.clone())
        .use_db(config.database.clone())
        .await?;
    Ok(db)
}

async fn create_one<C: Connection>(
    db: &Surreal<C>,
    table: &str,
    id: Option<&str>,
    data: serde_json::Value,
) -> Result<serde_json::Value, surrealdb::Error> {
    let created: Option<surrealdb::types::Value> = match id {
        Some(id) => db.create((table, id)).content(data).await?,
        None => db.create(table).content(data).await?,
    };
    Ok(created
        .map(surrealdb::types::Value::into_json_value)
        .unwrap_or_default())
}

async fn select_one<C: Connection>(
    db: &Surreal<C>,
    table: &str,
    id: &str,
) -> Result<Option<serde_json::Value>, surrealdb::Error> {
    let found: Option<surrealdb::types::Value> = db.select((table, id)).await?;
    Ok(found.map(surrealdb::types::Value::into_json_value))
}

async fn select_many<C: Connection>(
    db: &Surreal<C>,
    table: &str,
) -> Result<Vec<serde_json::Value>, surrealdb::Error> {
    let rows: Vec<surrealdb::types::Value> = db.select(table).await?;
    Ok(rows
        .into_iter()
        .map(surrealdb::types::Value::into_json_value)
        .collect())
}

async fn update_one<C: Connection>(
    db: &Surreal<C>,
    table: &str,
    id: &str,
    data: serde_json::Value,
) -> Result<Option<serde_json::Value>, surrealdb::Error> {
    let updated: Option<surrealdb::types::Value> = db.update((table, id)).merge(data).await?;
    Ok(updated.map(surrealdb::types::Value::into_json_value))
}

async fn delete_one<C: Connection>(
    db: &Surreal<C>,
    table: &str,
    id: &str,
) -> Result<Option<serde_json::Value>, surrealdb::Error> {
    let deleted: Option<surrealdb::types::Value> = db.delete((table, id)).await?;
    Ok(deleted.map(surrealdb::types::Value::into_json_value))
}

async fn run_query<C: Connection>(
    db: &Surreal<C>,
    sql: &str,
    vars: serde_json::Value,
) -> Result<surrealdb::IndexedResults, surrealdb::Error> {
    db.query(sql).bind(vars).await
}

impl SurrealDb {
    /// Connects to SurrealDB, signs in, and selects the namespace and database.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the configuration is not valid, the server
    /// is unreachable, the credentials are wrong, or the call times out.
    pub async fn connect(config: &SurrealDbConfig) -> Result<Self, SurrealDbError> {
        config.validate()?;
        let (host, scheme_tls) = config.split_endpoint();
        let tls = scheme_tls.unwrap_or(config.tls);
        let timeout = config.timeout();
        let setup = async {
            match (config.protocol, tls) {
                (Protocol::Ws, false) => {
                    let db: Surreal<WsClient> = Surreal::new::<Ws>(&host).await?;
                    finish(db, config).await.map(Engine::Ws)
                }
                (Protocol::Ws, true) => {
                    let db: Surreal<WsClient> = Surreal::new::<Wss>(&host).await?;
                    finish(db, config).await.map(Engine::Ws)
                }
                (Protocol::Http, false) => {
                    let db: Surreal<HttpClient> = Surreal::new::<Http>(&host).await?;
                    finish(db, config).await.map(Engine::Http)
                }
                (Protocol::Http, true) => {
                    let db: Surreal<HttpClient> = Surreal::new::<Https>(&host).await?;
                    finish(db, config).await.map(Engine::Http)
                }
            }
        };
        let engine = tokio::time::timeout(timeout, setup)
            .await
            .map_err(|_| SurrealDbError::Timeout { timeout })??;
        Ok(Self {
            inner: Arc::new(Inner {
                engine,
                config: config.clone(),
            }),
        })
    }

    /// Gets the handle from the app state, for example in a job or a task.
    #[must_use]
    pub fn from_state(state: &autumn_web::AppState) -> Option<Self> {
        state.extension::<Self>().map(|db| (*db).clone())
    }

    /// The configuration that the handle connected with.
    #[must_use]
    pub fn config(&self) -> &SurrealDbConfig {
        &self.inner.config
    }

    /// Creates a document in `table`. With `id` it writes that record,
    /// without it SurrealDB picks a random record id.
    ///
    /// Returns the created document, or JSON null if the server returned none.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the value does not encode as JSON, the
    /// server refuses the write, or the call times out.
    pub async fn create_record<T: Serialize + Sync>(
        &self,
        table: &str,
        id: Option<&str>,
        data: &T,
    ) -> Result<serde_json::Value, SurrealDbError> {
        let data = serde_json::to_value(data).map_err(|err| SurrealDbError::json(&err))?;
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => create_one(db, table, id, data.clone()).await,
                Engine::Http(db) => create_one(db, table, id, data.clone()).await,
            }
        };
        call(timeout, run).await
    }

    /// Reads one document from `table`. Returns `None` if the record is missing.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the server refuses the read or the call times out.
    pub async fn select_record(
        &self,
        table: &str,
        id: &str,
    ) -> Result<Option<serde_json::Value>, SurrealDbError> {
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => select_one(db, table, id).await,
                Engine::Http(db) => select_one(db, table, id).await,
            }
        };
        call(timeout, run).await
    }

    /// Reads every document in `table`.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the server refuses the read or the call times out.
    pub async fn select_all(&self, table: &str) -> Result<Vec<serde_json::Value>, SurrealDbError> {
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => select_many(db, table).await,
                Engine::Http(db) => select_many(db, table).await,
            }
        };
        call(timeout, run).await
    }

    /// Merges `data` into the document `table:id`. Returns `None` if the
    /// record is missing.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the value does not encode as JSON, the
    /// server refuses the write, or the call times out.
    pub async fn update_record<T: Serialize + Sync>(
        &self,
        table: &str,
        id: &str,
        data: &T,
    ) -> Result<Option<serde_json::Value>, SurrealDbError> {
        let data = serde_json::to_value(data).map_err(|err| SurrealDbError::json(&err))?;
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => update_one(db, table, id, data.clone()).await,
                Engine::Http(db) => update_one(db, table, id, data.clone()).await,
            }
        };
        call(timeout, run).await
    }

    /// Deletes the document `table:id`. Returns the deleted document,
    /// or `None` if the record was missing.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the server refuses the delete or the call times out.
    pub async fn delete_record(
        &self,
        table: &str,
        id: &str,
    ) -> Result<Option<serde_json::Value>, SurrealDbError> {
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => delete_one(db, table, id).await,
                Engine::Http(db) => delete_one(db, table, id).await,
            }
        };
        call(timeout, run).await
    }

    /// Runs raw SurrealQL with bound variables. Each `$name` in `sql` reads
    /// its value from `bindings`.
    ///
    /// Returns the per-statement results. Use [`SurrealDb::query_json`] for the
    /// JSON form, or `take` on the result for typed reads.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the server refuses the query or the call times out.
    pub async fn query_raw(
        &self,
        sql: &str,
        bindings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<surrealdb::IndexedResults, SurrealDbError> {
        let vars = serde_json::Value::Object(bindings.clone());
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => run_query(db, sql, vars.clone()).await,
                Engine::Http(db) => run_query(db, sql, vars.clone()).await,
            }
        };
        call(timeout, run).await
    }

    /// Runs raw SurrealQL with bound variables and returns each statement's
    /// rows as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the server refuses the query or the call times out.
    pub async fn query_json(
        &self,
        sql: &str,
        bindings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<Vec<serde_json::Value>>, SurrealDbError> {
        let mut results = self.query_raw(sql, bindings).await?;
        let count = results.num_statements();
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            out.push(results.take::<Vec<serde_json::Value>>(index)?);
        }
        Ok(out)
    }

    /// Asks the server for its version string.
    ///
    /// # Errors
    ///
    /// Returns [`SurrealDbError`] if the server does not answer or the call times out.
    pub async fn version(&self) -> Result<String, SurrealDbError> {
        let timeout = self.inner.config.timeout();
        let run = async {
            match &self.inner.engine {
                Engine::Ws(db) => db.version().await,
                Engine::Http(db) => db.version().await,
            }
        };
        call(timeout, run).await.map(|version| version.to_string())
    }
}

#[cfg(test)]
mod tests;
