//! The readiness check.
//!
//! # Contract
//!
//! - The check is down before the plugin starts.
//! - The check is up if a `version` round trip answers in time. The details
//!   carry the server version. They never show the endpoint or the credentials.
//! - The check waits at most 1 second, so it answers before the Autumn
//!   timeout of 2 seconds.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator};

use crate::client::SurrealDb;

/// The future type of a health check.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The longest wait of the check for the server.
pub(crate) const CHECK_WAIT: Duration = Duration::from_secs(1);

/// State that the plugin and the check share.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) handle: OnceLock<SurrealDb>,
}

/// Checks the SurrealDB connection with a `version` round trip.
pub struct SurrealDbCheck {
    shared: Arc<Shared>,
}

impl SurrealDbCheck {
    pub(crate) const fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl HealthIndicator for SurrealDbCheck {
    fn check(&self) -> BoxFuture<'_, HealthCheckOutput> {
        Box::pin(async move {
            let Some(db) = self.shared.handle.get() else {
                return HealthCheckOutput::down()
                    .with_details(details(&[("state", "not started")]));
            };
            match tokio::time::timeout(CHECK_WAIT, db.version()).await {
                Ok(Ok(version)) => {
                    HealthCheckOutput::up().with_details(details(&[("version", version.as_str())]))
                }
                _ => HealthCheckOutput::down().with_details(details(&[("state", "no round trip")])),
            }
        })
    }
}

/// Builds the details map.
fn details(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_owned(),
                serde_json::Value::String((*value).to_owned()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
