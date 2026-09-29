//! Tests for the health check. No network is used.

use super::*;
use autumn_web::actuator::{HealthIndicator, HealthStatus};

#[test]
fn check_is_down_before_the_plugin_starts() {
    let shared = Arc::new(Shared::default());
    let check = SurrealDbCheck::new(shared);
    let output = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(check.check()))
        .expect("the runtime builds");
    let debug = format!("{output:?}");
    assert!(debug.contains("not started"), "{debug}");
    assert_eq!(output.status, HealthStatus::Down);
}
