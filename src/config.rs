//! The `[surrealdb]` section of `autumn.toml`.
//!
//! # Contract
//!
//! Each layer overrides the layers before it:
//!
//! 1. The defaults.
//! 2. `[surrealdb]` in `autumn.toml`.
//! 3. `[profile.<name>.surrealdb]` in `autumn.toml`.
//! 4. `[surrealdb]` in `autumn-<profile>.toml`.
//! 5. `AUTUMN_SURREALDB__` variables. `AUTUMN_SURREALDB__TIMEOUT_MS` sets `timeout_ms`.
//!
//! The result must pass [`SurrealDbConfig::validate`]. Unknown keys are errors.
//! The password and the token only come from the file or the environment;
//! the plugin never logs them.
//!
//! ```toml
//! [surrealdb]
//! endpoint = "127.0.0.1:8000"
//! protocol = "ws" # or "http"
//! tls = false # true gives wss:// or https://
//! namespace = "app"
//! database = "app"
//! username = "root" # or set AUTUMN_SURREALDB__USERNAME
//! password = "..." # or set AUTUMN_SURREALDB__PASSWORD
//! # token = "..." # a JWT; use it instead of username and password
//! auth_scope = "root" # or "namespace", "database"
//! timeout_ms = 5000
//! health_check = true
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use autumn_web::config::Env;
use serde::{Deserialize, Serialize};

/// The default section name.
pub const DEFAULT_SECTION: &str = "surrealdb";

/// A configuration that is not valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub(crate) String);

/// The remote protocol that the plugin speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Protocol {
    /// The WebSocket protocol. `tls = true` gives `wss://`.
    #[default]
    Ws,
    /// The HTTP protocol. `tls = true` gives `https://`.
    Http,
}

/// The level that username and password sign in at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthScope {
    /// The root user of the server.
    #[default]
    Root,
    /// A user of the namespace.
    Namespace,
    /// A user of the database.
    Database,
}

/// The plugin settings.
///
/// The `Debug` output redacts `password` and `token`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct SurrealDbConfig {
    /// The server address as `host:port`. It may carry a `ws://`, `wss://`,
    /// `http://` or `https://` prefix; the prefix wins over `protocol` and `tls`.
    pub endpoint: String,
    /// The remote protocol.
    pub protocol: Protocol,
    /// If `true`, the plugin uses `wss://` or `https://`.
    pub tls: bool,
    /// The namespace that the plugin selects after it connects.
    pub namespace: String,
    /// The database that the plugin selects after it connects.
    pub database: String,
    /// The sign-in username. Needs `password`. The environment overrides the file.
    pub username: Option<String>,
    /// The sign-in password. Needs `username`. The environment overrides the file.
    pub password: Option<String>,
    /// A JWT for token authentication. It replaces username and password.
    /// The environment overrides the file.
    pub token: Option<String>,
    /// The level that username and password sign in at.
    pub auth_scope: AuthScope,
    /// The call timeout in milliseconds. It covers the connect and each call.
    pub timeout_ms: u64,
    /// If `true`, the plugin adds a readiness check.
    pub health_check: bool,
}

impl Default for SurrealDbConfig {
    fn default() -> Self {
        Self {
            endpoint: "127.0.0.1:8000".to_owned(),
            protocol: Protocol::Ws,
            tls: false,
            namespace: "autumn".to_owned(),
            database: "autumn".to_owned(),
            username: None,
            password: None,
            token: None,
            auth_scope: AuthScope::Root,
            timeout_ms: 5_000,
            health_check: true,
        }
    }
}

impl std::fmt::Debug for SurrealDbConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurrealDbConfig")
            .field("endpoint", &self.endpoint)
            .field("protocol", &self.protocol)
            .field("tls", &self.tls)
            .field("namespace", &self.namespace)
            .field("database", &self.database)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("token", &self.token.as_ref().map(|_| "***"))
            .field("auth_scope", &self.auth_scope)
            .field("timeout_ms", &self.timeout_ms)
            .field("health_check", &self.health_check)
            .finish()
    }
}

/// The type of a configuration leaf, for environment values.
#[derive(Clone, Copy)]
enum Kind {
    Text,
    Unsigned,
    Bool,
}

/// Each leaf key and its type.
const LEAVES: &[(&str, Kind)] = &[
    ("endpoint", Kind::Text),
    ("protocol", Kind::Text),
    ("tls", Kind::Bool),
    ("namespace", Kind::Text),
    ("database", Kind::Text),
    ("username", Kind::Text),
    ("password", Kind::Text),
    ("token", Kind::Text),
    ("auth_scope", Kind::Text),
    ("timeout_ms", Kind::Unsigned),
    ("health_check", Kind::Bool),
];

/// The largest call timeout: one day.
const MAX_TIMEOUT_MS: u64 = 86_400_000;

/// The longest namespace or database name.
const MAX_NAME_LEN: usize = 128;

impl SurrealDbConfig {
    /// Reads `[section]` from the app files and the environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if a file is not valid TOML or a value is not valid.
    pub fn resolve(section: &str) -> Result<Self, ConfigError> {
        autumn_web::dotenv::os_env_with_dotenv().map_or_else(
            |_| Self::resolve_with_env(section, &autumn_web::config::OsEnv),
            |env| Self::resolve_with_env(section, &env),
        )
    }

    /// Reads `[section]` with `env` as the environment.
    ///
    /// # Errors
    ///
    /// See `resolve`.
    pub fn resolve_with_env(section: &str, env: &dyn Env) -> Result<Self, ConfigError> {
        let (selected, profile) = active_profile(env);
        let mut merged = toml::Table::new();
        if let Some(base) = read_toml(&config_file("autumn.toml", env))? {
            merge_section(&mut merged, base.get(section), section)?;
            for name in inline_profile_names(&profile) {
                let inline = base
                    .get("profile")
                    .and_then(|p| p.get(name))
                    .and_then(|p| p.get(section));
                merge_section(&mut merged, inline, section)?;
            }
        }
        for name in autumn_web::config::profile_override_file_lookup_names(&profile, &selected) {
            if let Some(file) = read_toml(&config_file(&format!("autumn-{name}.toml"), env))? {
                merge_section(&mut merged, file.get(section), section)?;
                break;
            }
        }
        apply_env(&mut merged, section, env)?;
        let config: Self = toml::Value::Table(merged)
            .try_into()
            .map_err(|err| ConfigError(format!("[{section}]: {err}")))?;
        config.validate_section(section)?;
        Ok(config)
    }

    /// Checks each value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] that names the first key that is not valid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_section(DEFAULT_SECTION)
    }

    /// Checks each value. The errors name keys in `section`.
    pub(crate) fn validate_section(&self, section: &str) -> Result<(), ConfigError> {
        let fail = |key: &str, rule: &str| Err(ConfigError(format!("{section}.{key} {rule}")));
        if self.endpoint.trim().is_empty() {
            return fail("endpoint", "must be a `host:port` address");
        }
        if self.endpoint.trim() != self.endpoint {
            return fail("endpoint", "must not start or end with white space");
        }
        let (host, _) = self.split_endpoint();
        if host.trim().is_empty() || host.contains(char::is_whitespace) {
            return fail("endpoint", "must be a `host:port` address");
        }
        if let Some(rule) = name_rule(&self.namespace) {
            return fail("namespace", rule);
        }
        if let Some(rule) = name_rule(&self.database) {
            return fail("database", rule);
        }
        match (&self.token, &self.username, &self.password) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                return fail("token", "must not be set with `username` or `password`");
            }
            (None, Some(_), None) | (None, None, Some(_)) => {
                return fail(
                    "username",
                    "needs `password`, and `password` needs `username`",
                );
            }
            _ => {}
        }
        if !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return fail("timeout_ms", "must be from 1 to 86400000 (one day)");
        }
        Ok(())
    }

    /// The call timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// Splits the endpoint into the bare `host:port` part and a TLS override.
    ///
    /// A `ws://`, `wss://`, `http://` or `https://` prefix sets the override.
    /// Without a prefix the function returns `None` and the caller uses `tls`.
    #[must_use]
    pub fn split_endpoint(&self) -> (String, Option<bool>) {
        for (prefix, tls) in [
            ("wss://", true),
            ("https://", true),
            ("ws://", false),
            ("http://", false),
        ] {
            if let Some(rest) = self.endpoint.strip_prefix(prefix) {
                return (rest.to_owned(), Some(tls));
            }
        }
        (self.endpoint.clone(), None)
    }
}

/// Gives the broken rule of a namespace or database name, if any.
fn name_rule(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("must not be empty");
    }
    if name.len() > MAX_NAME_LEN {
        return Some("must be 128 characters or fewer");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Some("must have `A-Z a-z 0-9 _ -` characters only");
    }
    None
}

/// Gives the selected profile text and the normalized profile, as Autumn does.
fn active_profile(env: &dyn Env) -> (String, String) {
    let selected = ["AUTUMN_ENV", "AUTUMN_PROFILE"]
        .iter()
        .filter_map(|key| env.var(key).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let release = env.var("AUTUMN_IS_DEBUG").is_ok_and(|v| v == "0");
            if release { "prod" } else { "dev" }.to_owned()
        });
    let profile =
        autumn_web::config::normalize_profile_name(&selected).unwrap_or_else(|| "dev".to_owned());
    (selected, profile)
}

/// The inline profile names to read, in order. The canonical name is last.
fn inline_profile_names(profile: &str) -> Vec<&str> {
    match profile {
        "prod" => vec!["production", "prod"],
        "dev" => vec!["development", "dev"],
        _ => vec![profile],
    }
}

/// Finds a config file in `AUTUMN_MANIFEST_DIR`, or else in the working directory.
fn config_file(name: &str, env: &dyn Env) -> PathBuf {
    env.var("AUTUMN_MANIFEST_DIR")
        .ok()
        .map(|dir| Path::new(&dir).join(name))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn read_toml(path: &Path) -> Result<Option<toml::Table>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map(Some)
            .map_err(|err| ConfigError(format!("{}: {err}", path.display()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ConfigError(format!("{}: {err}", path.display()))),
    }
}

fn merge_section(
    into: &mut toml::Table,
    layer: Option<&toml::Value>,
    section: &str,
) -> Result<(), ConfigError> {
    match layer {
        None => Ok(()),
        Some(toml::Value::Table(table)) => {
            deep_merge(into, table);
            Ok(())
        }
        Some(_) => Err(ConfigError(format!("[{section}] must be a table"))),
    }
}

fn deep_merge(into: &mut toml::Table, layer: &toml::Table) {
    for (key, value) in layer {
        match (into.get_mut(key), value) {
            (Some(toml::Value::Table(old)), toml::Value::Table(new)) => deep_merge(old, new),
            _ => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

fn apply_env(into: &mut toml::Table, section: &str, env: &dyn Env) -> Result<(), ConfigError> {
    let name: String = section
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    for (key, kind) in LEAVES {
        let name = format!("AUTUMN_{name}__{}", key.to_ascii_uppercase());
        let Ok(raw) = env.var(&name) else {
            continue;
        };
        let bad = || ConfigError(format!("{name}: can not read {raw:?}"));
        let value = match kind {
            Kind::Text => toml::Value::String(raw.clone()),
            Kind::Unsigned => {
                let value: i64 = raw.trim().parse().map_err(|_| bad())?;
                if value < 0 {
                    return Err(bad());
                }
                toml::Value::Integer(value)
            }
            Kind::Bool => match raw.trim() {
                "true" | "1" => toml::Value::Boolean(true),
                "false" | "0" => toml::Value::Boolean(false),
                _ => return Err(bad()),
            },
        };
        into.insert((*key).to_owned(), value);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
