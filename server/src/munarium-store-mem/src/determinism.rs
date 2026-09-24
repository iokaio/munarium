// SPDX-License-Identifier: Apache-2.0
//! Injectable dependencies for deterministic fixtures. Production defaults retain
//! UTC wall time and UUID v4 identities. Injected IDs must be unique per store;
//! deterministic generators do not make concurrent scheduling reproducible.
use std::sync::Arc;

pub type Clock = Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>;
pub type IdGenerator = Arc<dyn Fn() -> String + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(chrono::Utc::now)
}

pub fn random_ids() -> IdGenerator {
    Arc::new(|| uuid::Uuid::new_v4().simple().to_string())
}
