//! Tests for the `[surrealdb]` configuration. No network is used.

#![allow(
    clippy::field_reassign_with_default,
    reason = "each test changes one key of the defaults"
)]
use std::collections::HashMap;

use autumn_web::config::Env;
use proptest::prelude::*;

use super::*;

/// An [`Env`] backed by a map.
#[derive(Default)]
struct MapEnv {
    vars: HashMap<String, String>,
}

impl MapEnv {
    fn with(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_owned(), value.to_owned());
        self
    }
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Result<String, std::env::VarError> {
        self.vars
            .get(key)
            .cloned()
            .ok_or(std::env::VarError::NotPresent)
    }
}

fn resolve_env(env: &MapEnv) -> Result<SurrealDbConfig, ConfigError> {
    SurrealDbConfig::resolve_with_env(DEFAULT_SECTION, env)
}

#[test]
fn defaults_validate() {
    SurrealDbConfig::default().validate().unwrap();
}

#[test]
fn empty_env_gives_defaults() {
    let config = resolve_env(&MapEnv::default()).unwrap();
    assert_eq!(config, SurrealDbConfig::default());
}

#[test]
fn env_overrides_each_leaf() {
    let config = resolve_env(
        &MapEnv::default()
            .with("AUTUMN_SURREALDB__ENDPOINT", "db.example.com:8000")
            .with("AUTUMN_SURREALDB__PROTOCOL", "http")
            .with("AUTUMN_SURREALDB__TLS", "true")
            .with("AUTUMN_SURREALDB__NAMESPACE", "shop")
            .with("AUTUMN_SURREALDB__DATABASE", "orders")
            .with("AUTUMN_SURREALDB__USERNAME", "app")
            .with("AUTUMN_SURREALDB__PASSWORD", "s3cret")
            .with("AUTUMN_SURREALDB__AUTH_SCOPE", "database")
            .with("AUTUMN_SURREALDB__TIMEOUT_MS", "10000")
            .with("AUTUMN_SURREALDB__HEALTH_CHECK", "false"),
    )
    .unwrap();
    assert_eq!(config.endpoint, "db.example.com:8000");
    assert_eq!(config.protocol, Protocol::Http);
    assert!(config.tls);
    assert_eq!(config.namespace, "shop");
    assert_eq!(config.database, "orders");
    assert_eq!(config.username.as_deref(), Some("app"));
    assert_eq!(config.password.as_deref(), Some("s3cret"));
    assert_eq!(config.auth_scope, AuthScope::Database);
    assert_eq!(config.timeout_ms, 10_000);
    assert_eq!(config.timeout(), Duration::from_secs(10));
    assert!(!config.health_check);
}

#[test]
fn env_token_replaces_credentials() {
    let config = resolve_env(&MapEnv::default().with("AUTUMN_SURREALDB__TOKEN", "jwt")).unwrap();
    assert_eq!(config.token.as_deref(), Some("jwt"));
    assert_eq!(config.username, None);
}

#[test]
fn env_bad_timeout_ms_is_an_error() {
    let err =
        resolve_env(&MapEnv::default().with("AUTUMN_SURREALDB__TIMEOUT_MS", "soon")).unwrap_err();
    assert!(err.0.contains("AUTUMN_SURREALDB__TIMEOUT_MS"), "{err}");
}

#[test]
fn env_bad_bool_is_an_error() {
    let err = resolve_env(&MapEnv::default().with("AUTUMN_SURREALDB__TLS", "yes")).unwrap_err();
    assert!(err.0.contains("AUTUMN_SURREALDB__TLS"), "{err}");
}

#[test]
fn env_unknown_keys_are_ignored() {
    // Environment variables outside the leaf list never reach the table.
    let config = resolve_env(&MapEnv::default().with("AUTUMN_SURREALDB__WHATEVER", "1")).unwrap();
    assert_eq!(config, SurrealDbConfig::default());
}

#[test]
fn token_and_password_conflict() {
    let mut config = SurrealDbConfig::default();
    config.token = Some("jwt".into());
    config.username = Some("root".into());
    config.password = Some("root".into());
    let err = config.validate().unwrap_err();
    assert!(err.0.contains("token"), "{err}");
}

#[test]
fn username_needs_password() {
    let mut config = SurrealDbConfig::default();
    config.username = Some("root".into());
    let err = config.validate().unwrap_err();
    assert!(err.0.contains("username"), "{err}");
}

#[test]
fn password_needs_username() {
    let mut config = SurrealDbConfig::default();
    config.password = Some("root".into());
    let err = config.validate().unwrap_err();
    assert!(err.0.contains("username"), "{err}");
}

#[test]
fn blank_endpoint_fails() {
    let mut config = SurrealDbConfig::default();
    config.endpoint = "   ".into();
    let err = config.validate().unwrap_err();
    assert!(err.0.contains("endpoint"), "{err}");
}

#[test]
fn blank_namespace_and_database_fail() {
    let mut config = SurrealDbConfig::default();
    config.namespace = String::new();
    assert!(config.validate().unwrap_err().0.contains("namespace"));
    let mut config = SurrealDbConfig::default();
    config.database = String::new();
    assert!(config.validate().unwrap_err().0.contains("database"));
}

#[test]
fn namespace_with_space_fails() {
    let mut config = SurrealDbConfig::default();
    config.namespace = "my app".into();
    let err = config.validate().unwrap_err();
    assert!(err.0.contains("namespace"), "{err}");
}

#[test]
fn zero_and_huge_timeout_fail() {
    let mut config = SurrealDbConfig::default();
    config.timeout_ms = 0;
    assert!(config.validate().unwrap_err().0.contains("timeout_ms"));
    let mut config = SurrealDbConfig::default();
    config.timeout_ms = MAX_TIMEOUT_MS + 1;
    assert!(config.validate().unwrap_err().0.contains("timeout_ms"));
}

#[test]
fn split_endpoint_scheme_wins() {
    let mut config = SurrealDbConfig::default();
    config.endpoint = "wss://db.example.com:8000".into();
    assert_eq!(
        config.split_endpoint(),
        ("db.example.com:8000".to_owned(), Some(true))
    );
    config.endpoint = "http://127.0.0.1:8000".into();
    assert_eq!(
        config.split_endpoint(),
        ("127.0.0.1:8000".to_owned(), Some(false))
    );
    config.endpoint = "127.0.0.1:8000".into();
    assert_eq!(config.split_endpoint(), ("127.0.0.1:8000".to_owned(), None));
}

#[test]
fn debug_redacts_secrets() {
    let mut config = SurrealDbConfig::default();
    config.username = Some("root".into());
    config.password = Some("hunter2".into());
    config.token = Some("jwt-secret".into());
    let shown = format!("{config:?}");
    assert!(shown.contains("root"), "{shown}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(!shown.contains("jwt-secret"), "{shown}");
}

#[test]
fn section_name_appears_in_errors() {
    let mut config = SurrealDbConfig::default();
    config.endpoint = String::new();
    let err = config.validate_section("analytics").unwrap_err();
    assert!(err.0.starts_with("analytics.endpoint"), "{err}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn name_rule_accepts_word_chars(name in "[A-Za-z0-9_-]{1,128}") {
        prop_assert!(name_rule(&name).is_none());
    }

    #[test]
    fn name_rule_rejects_blank_or_long_or_spaced(name in "(| *|[A-Za-z0-9 _-]{129,200})") {
        prop_assert!(name_rule(&name).is_some());
    }

    #[test]
    fn timeout_ms_in_range_validates(ms in 1..=MAX_TIMEOUT_MS) {
        let mut config = SurrealDbConfig::default();
        config.timeout_ms = ms;
        prop_assert!(config.validate().is_ok());
    }
}
