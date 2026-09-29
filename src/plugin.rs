//! [`SurrealDbPlugin`]: installs a [`SurrealDb`](crate::SurrealDb) handle in an Autumn app.
//!
//! # Contract
//!
//! - `build` reads the configuration. A bad configuration stops the boot in the startup hook.
//! - The startup hook connects to the server, signs in, selects the namespace
//!   and the database, and puts the handle in the app state.
//! - The readiness check and the handlers use the same handle.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use autumn_web::app::AppBuilder;
use autumn_web::plugin::Plugin;
use autumn_web::{AppState, AutumnError};

use crate::client::SurrealDb;
use crate::config::{ConfigError, DEFAULT_SECTION, SurrealDbConfig};
use crate::error::SurrealDbError;
use crate::health::{Shared, SurrealDbCheck};

/// The plugin name in Autumn diagnostics.
pub const PLUGIN_NAME: &str = "autumn-plugin-surrealdb";

enum ConfigSource {
    Section(String),
    Explicit(Box<SurrealDbConfig>),
}

type Change = Box<dyn FnOnce(&mut SurrealDbConfig) + Send>;

/// Installs a [`SurrealDb`](crate::SurrealDb) handle in an Autumn app.
///
/// ```rust,no_run
/// use autumn_plugin_surrealdb::SurrealDbPlugin;
///
/// # async fn run() {
/// autumn_web::app()
///     .plugin(SurrealDbPlugin::new().configure(|c| {
///         c.namespace = "shop".into();
///     }))
///     .run()
///     .await;
/// # }
/// ```
pub struct SurrealDbPlugin {
    source: ConfigSource,
    changes: Vec<Change>,
}

impl Default for SurrealDbPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl SurrealDbPlugin {
    /// Makes a plugin that reads `[surrealdb]`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            source: ConfigSource::Section(DEFAULT_SECTION.to_owned()),
            changes: Vec::new(),
        }
    }

    /// Reads `[section]` instead of `[surrealdb]`.
    ///
    /// An app can have one SurrealDB plugin only. Autumn ignores a second
    /// plugin with the same name.
    #[must_use]
    pub fn config_section(mut self, section: impl Into<String>) -> Self {
        self.source = ConfigSource::Section(section.into());
        self
    }

    /// Uses `config` and reads no files or variables.
    #[must_use]
    pub fn config(mut self, config: SurrealDbConfig) -> Self {
        self.source = ConfigSource::Explicit(Box::new(config));
        self
    }

    /// Changes the configuration after the plugin reads it.
    #[must_use]
    pub fn configure(mut self, change: impl FnOnce(&mut SurrealDbConfig) + Send + 'static) -> Self {
        self.changes.push(Box::new(change));
        self
    }

    fn resolve(
        source: &ConfigSource,
        changes: Vec<Change>,
    ) -> Result<SurrealDbConfig, ConfigError> {
        let mut config = match source {
            ConfigSource::Section(section) => SurrealDbConfig::resolve(section)?,
            ConfigSource::Explicit(config) => (**config).clone(),
        };
        for change in changes {
            change(&mut config);
        }
        match source {
            ConfigSource::Section(section) => config.validate_section(section)?,
            ConfigSource::Explicit(_) => config.validate()?,
        }
        Ok(config)
    }
}

/// Makes a boot-stopping error.
fn boot_error(err: impl std::fmt::Display) -> AutumnError {
    AutumnError::internal_server_error_msg(format!("{PLUGIN_NAME}: {err}"))
}

impl Plugin for SurrealDbPlugin {
    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(PLUGIN_NAME)
    }

    fn build(self, app: AppBuilder) -> AppBuilder {
        let Self { source, changes } = self;
        let mut app = app;
        if let ConfigSource::Section(section) = &source {
            app = app.config_section(section.clone());
        }
        let resolved = Self::resolve(&source, changes);
        let shared = Arc::new(Shared {
            handle: OnceLock::new(),
        });
        if resolved.as_ref().is_ok_and(|config| config.health_check) {
            app = app.health_indicator(
                "surrealdb",
                Arc::new(SurrealDbCheck::new(Arc::clone(&shared))),
            );
        }
        let on_start = Arc::clone(&shared);
        let resolved = Arc::new(resolved);
        app.on_startup(move |state| {
            let shared = Arc::clone(&on_start);
            let resolved = Arc::clone(&resolved);
            async move {
                let config = resolved.as_ref().clone().map_err(|err| boot_error(&err))?;
                let db = SurrealDb::connect(&config)
                    .await
                    .map_err(|err| boot_error(&err))?;
                state.insert_extension(db.clone());
                let _ = shared.handle.set(db);
                tracing::info!("the SurrealDB plugin is ready");
                Ok(())
            }
        })
    }
}

impl axum::extract::FromRequestParts<AppState> for SurrealDb {
    type Rejection = AutumnError;

    fn from_request_parts(
        _parts: &mut http::request::Parts,
        state: &AppState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            Self::from_state(state).ok_or_else(|| SurrealDbError::NotInstalled.into_autumn()),
        )
    }
}

#[cfg(test)]
mod tests;
