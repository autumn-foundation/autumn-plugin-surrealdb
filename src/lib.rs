//! Autumn plugin for SurrealDB.
//!
//! Add [`SurrealDbPlugin`] to the app. Then use the [`SurrealDb`] extractor in a handler.
//!
//! ```rust,no_run
//! use autumn_plugin_surrealdb::{SurrealDb, SurrealDbPlugin, SurrealDbResultExt as _};
//! use autumn_web::prelude::*;
//!
//! #[derive(serde::Deserialize, serde::Serialize)]
//! struct Profile {
//!     name: String,
//! }
//!
//! #[get("/profiles/{id}")]
//! async fn profile(db: SurrealDb, Path(id): Path<String>) -> AutumnResult<Json<serde_json::Value>> {
//!     let profile = db
//!         .select_record("profile", &id)
//!         .await
//!         .or_http()?
//!         .map(Json)
//!         .ok_or_else(|| AutumnError::not_found_msg("no profile"));
//!     profile
//! }
//!
//! # async fn run() {
//! autumn_web::app()
//!     .plugin(
//!         SurrealDbPlugin::new().configure(|c| {
//!             c.endpoint = "127.0.0.1:8000".into();
//!             c.namespace = "app".into();
//!             c.database = "app".into();
//!         }),
//!     )
//!     .routes(routes![profile])
//!     .run()
//!     .await;
//! # }
//! ```
//!
//! The plugin reads `[surrealdb]` in `autumn.toml`. See [`config`] for the keys.
//!
//! # What the plugin gives
//!
//! - [`SurrealDb`]: document CRUD (`create_record`, `select_record`, `select_all`,
//!   `update_record`, `delete_record`) with `serde` types, raw SurrealQL with bound
//!   variables (`query_json`), and `version()`.
//! - [`SurrealDbPlugin`]: connects at startup, signs in, selects the namespace and
//!   database, and puts the handle in the app state.
//! - A readiness check that runs the `version()` query.
//!
//! # Limits
//!
//! - The plugin talks to a SurrealDB server over the network. It never embeds
//!   a database: the `surrealdb` dependency enables the remote protocols only.
//! - Each call has a timeout (`timeout_ms`). A timed-out call returns an error.
//! - Logs, error text and `Debug` output never show the password or the token.

mod client;
pub mod config;
mod error;
mod health;
mod plugin;

pub use client::SurrealDb;
pub use config::{AuthScope, ConfigError, DEFAULT_SECTION, Protocol, SurrealDbConfig};
pub use error::{ErrorKind, SurrealDbError, SurrealDbResultExt};
pub use health::SurrealDbCheck;
pub use plugin::{PLUGIN_NAME, SurrealDbPlugin};

/// The `surrealdb` crate that the plugin uses. Use it for typed access.
pub use surrealdb;
