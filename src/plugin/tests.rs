//! Tests for the plugin wiring. No network is used.

#![allow(
    clippy::field_reassign_with_default,
    reason = "each test changes one key of the defaults"
)]
use autumn_web::plugin::Plugin;

use super::*;

#[test]
fn plugin_name_matches_constant() {
    let plugin = SurrealDbPlugin::new();
    assert_eq!(plugin.name(), PLUGIN_NAME);
    let plugin = SurrealDbPlugin::new().config_section("analytics");
    assert_eq!(plugin.name(), PLUGIN_NAME);
}

#[test]
fn resolve_applies_changes_and_validates() {
    let mut config = SurrealDbConfig::default();
    config.namespace = "shop".into();
    config.database = "orders".into();
    let source = ConfigSource::Explicit(Box::new(config));
    let resolved = SurrealDbPlugin::resolve(
        &source,
        vec![Box::new(|c: &mut SurrealDbConfig| {
            c.endpoint = "db.internal:8000".into();
        })],
    )
    .expect("the config is valid");
    assert_eq!(resolved.namespace, "shop");
    assert_eq!(resolved.endpoint, "db.internal:8000");
}

#[test]
fn resolve_rejects_an_invalid_change() {
    let source = ConfigSource::Explicit(Box::default());
    let err = SurrealDbPlugin::resolve(
        &source,
        vec![Box::new(|c: &mut SurrealDbConfig| {
            c.namespace = String::new();
        })],
    )
    .expect_err("a blank namespace fails");
    assert!(err.0.contains("namespace"), "{err}");
}

#[test]
fn plugin_has_a_default() {
    let plugin = SurrealDbPlugin::default();
    assert_eq!(plugin.name(), PLUGIN_NAME);
}
