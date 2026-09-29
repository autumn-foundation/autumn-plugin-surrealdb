//! Tests for the client. The tests that need a server are ignored by default.

#![allow(
    clippy::field_reassign_with_default,
    reason = "each test changes one key of the defaults"
)]
use super::*;
use crate::ErrorKind;

fn short_config() -> SurrealDbConfig {
    let mut config = SurrealDbConfig::default();
    config.timeout_ms = 500;
    config
}

#[test]
fn connect_rejects_bad_config_without_touching_the_network() {
    let mut config = short_config();
    config.endpoint = "   ".into();
    let err = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(SurrealDb::connect(&config)))
        .expect("the runtime builds")
        .expect_err("a blank endpoint fails");
    assert!(matches!(err, SurrealDbError::Config(_)), "{err}");
}

#[test]
fn connect_to_a_closed_port_reports_a_connection_error() {
    // Nothing listens on 127.0.0.1:9 (discard). The connect fails fast.
    let mut config = short_config();
    config.endpoint = "127.0.0.1:9".into();
    let err = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(SurrealDb::connect(&config)))
        .expect("the runtime builds")
        .expect_err("nothing listens on the discard port");
    assert!(
        matches!(
            err,
            SurrealDbError::Database {
                kind: ErrorKind::Connection,
                ..
            }
        ),
        "expected a connection error, got {err:?}"
    );
}

#[test]
#[ignore = "needs a SurrealDB server at 127.0.0.1:8000"]
fn live_round_trip() {
    let config = short_config();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the runtime builds");
    rt.block_on(async {
        let db = SurrealDb::connect(&config).await.expect("connect works");
        let version = db.version().await.expect("version works");
        assert!(!version.is_empty(), "the version is not blank");
    });
}
