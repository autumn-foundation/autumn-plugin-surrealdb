//! The public error type.
//!
//! # Contract
//!
//! - A SurrealDB error keeps its kind and its message. The password and the
//!   token never reach the error: they only travel in the sign-in call.
//! - The error text shows the kind only. [`SurrealDbError::detail`] gives the
//!   full message. Do not show it to users: a query message can hold data.
//! - [`ErrorKind`] is the plugin's own copy of the SurrealDB error kind.
//!   A `surrealdb` update does not change it.
//! - A timeout gives HTTP 504. A refused connection and a missing plugin give
//!   503. A forbidden call gives 403, a missing record kind gives 404, and a
//!   bad query or value gives 400. All other errors give 500.
//! - The `Debug` output is the error text. It does not show `detail`.
//! - A timeout and a connection error are retryable.

use std::time::Duration;

use autumn_web::AutumnError;
use http::StatusCode;

use crate::config::ConfigError;

/// An error from the plugin.
///
/// The `Debug` output is the error text. It does not show `detail`.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SurrealDbError {
    /// The configuration is not valid.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// SurrealDB refused the call.
    ///
    /// The text does not show `detail`, because it can hold query data.
    #[error("SurrealDB refused the call: {kind}")]
    #[non_exhaustive]
    Database {
        /// The SurrealDB error kind.
        kind: ErrorKind,
        /// The full SurrealDB message.
        detail: String,
    },

    /// A value does not encode as JSON.
    ///
    /// The text does not show `detail`, because it can hold the value.
    #[error("the value does not encode as JSON")]
    #[non_exhaustive]
    Json {
        /// The full `serde_json` message.
        detail: String,
    },

    /// The call did not complete in time.
    #[error("the call did not complete in {timeout:?}")]
    #[non_exhaustive]
    Timeout {
        /// The timeout.
        timeout: Duration,
    },

    /// The app does not have the plugin.
    #[error("the SurrealDB plugin is not installed: add `SurrealDbPlugin` to the app")]
    NotInstalled,
}

/// The kind of a SurrealDB error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The request is not valid.
    Validation,
    /// The configuration or the feature is not supported.
    Configuration,
    /// The query failed on the server.
    Query,
    /// A value did not serialize or deserialize.
    Serialization,
    /// The credentials lack permission.
    NotAllowed,
    /// The resource does not exist.
    NotFound,
    /// The resource already exists.
    AlreadyExists,
    /// The client could not reach the server.
    Connection,
    /// The query threw a user error.
    Thrown,
    /// An internal or unknown error.
    Internal,
}

impl From<&surrealdb::types::ErrorDetails> for ErrorKind {
    fn from(details: &surrealdb::types::ErrorDetails) -> Self {
        use surrealdb::types::ErrorDetails as Details;
        match details {
            Details::Validation(_) => Self::Validation,
            Details::Configuration(_) => Self::Configuration,
            Details::Query(_) => Self::Query,
            Details::Serialization(_) => Self::Serialization,
            Details::NotAllowed(_) => Self::NotAllowed,
            Details::NotFound(_) => Self::NotFound,
            Details::AlreadyExists(_) => Self::AlreadyExists,
            Details::Connection(_) => Self::Connection,
            Details::Thrown => Self::Thrown,
            _ => Self::Internal,
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Validation => "validation",
            Self::Configuration => "configuration",
            Self::Query => "query",
            Self::Serialization => "serialization",
            Self::NotAllowed => "not allowed",
            Self::NotFound => "not found",
            Self::AlreadyExists => "already exists",
            Self::Connection => "connection",
            Self::Thrown => "thrown",
            Self::Internal => "internal",
        })
    }
}

impl std::fmt::Debug for SurrealDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The derived output shows `detail`. It can hold query data.
        f.debug_tuple("SurrealDbError")
            .field(&format_args!("{self}"))
            .finish()
    }
}

impl From<surrealdb::Error> for SurrealDbError {
    fn from(err: surrealdb::Error) -> Self {
        Self::Database {
            kind: ErrorKind::from(err.details()),
            detail: err.message().to_owned(),
        }
    }
}

impl SurrealDbError {
    /// Makes a [`SurrealDbError::Json`] error.
    pub(crate) fn json(err: &serde_json::Error) -> Self {
        Self::Json {
            detail: err.to_string(),
        }
    }

    /// The full message of a [`SurrealDbError::Database`] or [`SurrealDbError::Json`] error.
    ///
    /// The message can hold query data. Do not show it to users.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Database { detail, .. } | Self::Json { detail } => Some(detail),
            _ => None,
        }
    }

    /// The SurrealDB error kind of a [`SurrealDbError::Database`] error.
    #[must_use]
    pub const fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Database { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Returns `true` if a retry of the same call can succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. }
                | Self::Database {
                    kind: ErrorKind::Connection,
                    ..
                }
        )
    }

    /// The HTTP status for this error.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
            Self::NotInstalled
            | Self::Database {
                kind: ErrorKind::Connection,
                ..
            } => StatusCode::SERVICE_UNAVAILABLE,
            Self::Database {
                kind: ErrorKind::NotAllowed,
                ..
            } => StatusCode::FORBIDDEN,
            Self::Database {
                kind: ErrorKind::NotFound,
                ..
            } => StatusCode::NOT_FOUND,
            Self::Database {
                kind: ErrorKind::Validation | ErrorKind::Serialization,
                ..
            } => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Converts to an [`AutumnError`] with `status`.
    ///
    /// The `?` operator also converts, but always gives status 500.
    /// Autumn shows server error details only in development.
    #[must_use]
    pub fn into_autumn(self) -> AutumnError {
        let status = self.status();
        AutumnError::internal_server_error(self).with_status(status)
    }
}

/// Adds `or_http` to `Result<T, SurrealDbError>`.
pub trait SurrealDbResultExt<T> {
    /// Converts the error with [`SurrealDbError::into_autumn`].
    ///
    /// # Errors
    ///
    /// Returns the converted error.
    fn or_http(self) -> Result<T, AutumnError>;
}

impl<T> SurrealDbResultExt<T> for Result<T, SurrealDbError> {
    fn or_http(self) -> Result<T, AutumnError> {
        self.map_err(SurrealDbError::into_autumn)
    }
}

#[cfg(test)]
mod tests;
