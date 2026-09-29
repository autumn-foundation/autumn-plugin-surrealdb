//! Tests for the error type. No network is used.

use std::time::Duration;

use super::*;

#[test]
fn status_mapping() {
    let timeout = SurrealDbError::Timeout {
        timeout: Duration::from_secs(5),
    };
    assert_eq!(timeout.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(timeout.is_retryable());

    let connection = SurrealDbError::Database {
        kind: ErrorKind::Connection,
        detail: "refused".into(),
    };
    assert_eq!(connection.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(connection.is_retryable());

    let not_allowed = SurrealDbError::Database {
        kind: ErrorKind::NotAllowed,
        detail: "no".into(),
    };
    assert_eq!(not_allowed.status(), StatusCode::FORBIDDEN);
    assert!(!not_allowed.is_retryable());

    let not_found = SurrealDbError::Database {
        kind: ErrorKind::NotFound,
        detail: "missing".into(),
    };
    assert_eq!(not_found.status(), StatusCode::NOT_FOUND);

    let validation = SurrealDbError::Database {
        kind: ErrorKind::Validation,
        detail: "bad".into(),
    };
    assert_eq!(validation.status(), StatusCode::BAD_REQUEST);

    let serialization = SurrealDbError::Database {
        kind: ErrorKind::Serialization,
        detail: "bad".into(),
    };
    assert_eq!(serialization.status(), StatusCode::BAD_REQUEST);

    let internal = SurrealDbError::Database {
        kind: ErrorKind::Internal,
        detail: "boom".into(),
    };
    assert_eq!(internal.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let query = SurrealDbError::Database {
        kind: ErrorKind::Query,
        detail: "boom".into(),
    };
    assert_eq!(query.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let missing = SurrealDbError::NotInstalled;
    assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);

    let config = SurrealDbError::Config(ConfigError("bad".into()));
    assert_eq!(config.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn debug_hides_detail() {
    let err = SurrealDbError::Database {
        kind: ErrorKind::Query,
        detail: "secret query data".into(),
    };
    let shown = format!("{err:?}");
    assert!(shown.contains("query"), "{shown}");
    assert!(!shown.contains("secret query data"), "{shown}");
    assert_eq!(err.detail(), Some("secret query data"));
    assert_eq!(err.kind(), Some(ErrorKind::Query));
}

#[test]
fn timeout_and_config_have_no_detail() {
    let err = SurrealDbError::Timeout {
        timeout: Duration::from_secs(1),
    };
    assert_eq!(err.detail(), None);
    assert_eq!(err.kind(), None);
}

#[test]
fn into_autumn_keeps_status() {
    let err = SurrealDbError::Database {
        kind: ErrorKind::NotFound,
        detail: "missing".into(),
    };
    let http = err.into_autumn();
    assert_eq!(http.status(), StatusCode::NOT_FOUND);
}

#[test]
fn or_http_maps_error() {
    let result: Result<(), SurrealDbError> = Err(SurrealDbError::NotInstalled);
    let http = result.or_http().unwrap_err();
    assert_eq!(http.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn json_error_shows_no_detail_in_text() {
    let err = SurrealDbError::Json {
        detail: "sensitive value".into(),
    };
    let shown = format!("{err}");
    assert!(!shown.contains("sensitive value"), "{shown}");
    assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
