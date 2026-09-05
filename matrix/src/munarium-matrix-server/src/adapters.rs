// SPDX-License-Identifier: Apache-2.0
//! The adapter extension point.
//!
//! Munarium Matrix core links the adapters for the databases an application
//! already runs — PostgreSQL, MySQL, SQL Server — and for file and blob
//! sources. Adapters for analytics platforms an enterprise buys and administers
//! separately are **not** in this repository: they are Munarium Matrix
//! Enterprise, and they reach the runtime through this registry rather than
//! through a patch to [`crate::runtime::open_adapter`].
//!
//! The shape is deliberate, and it is the same one the `SourceAdapter` trait
//! already uses one level down: a build **declares** what it can do, and the
//! layers above **refuse rather than assume**. A kind nobody registered is a
//! [`Refusal::adapter_not_available`] naming what would serve it — never a
//! panic, never a silent fallback to a different adapter, and never a compile
//! error in a build that is simply smaller than another one.
//!
//! The asset grammar is unaffected. Every [`AdapterKind`] stays in the enum and
//! in the validator whichever adapters a binary links, so an asset written for
//! Databricks parses, validates and applies against a core build, and is
//! refused only when something tries to execute it. That is what lets one set
//! of assets move between a core deployment and an Enterprise one.
//!
//! # Registering an adapter out of tree
//!
//! ```ignore
//! use munarium_matrix_server::adapters::{AdapterFactory, AdapterRegistry};
//!
//! struct OracleFactory;
//!
//! #[async_trait::async_trait]
//! impl AdapterFactory for OracleFactory {
//!     async fn open(
//!         &self,
//!         state: &AppState,
//!         doc: &DataSourceDoc,
//!     ) -> Result<Box<dyn SourceAdapter>, Refusal> {
//!         // ... build the adapter from doc.spec, resolving credentials
//!         // through the same helpers core uses.
//!     }
//! }
//!
//! let mut registry = AdapterRegistry::new();
//! registry.register(AdapterKind::Oracle, Arc::new(OracleFactory));
//! ```
//!
//! The binary then builds its `AppState` with that registry, and everything
//! above the seam — the query plane, the sync role, reconcile — is unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use munarium_matrix_adapter::SourceAdapter;
use munarium_matrix_core::Refusal;
use munarium_matrix_types::assets::{AdapterKind, DataSourceDoc};

use crate::state::AppState;

/// Builds one adapter kind from an applied `DataSource`.
///
/// Implementors resolve their own credentials and enforce their own egress
/// checks, exactly as core's built-in construction does; the registry adds no
/// policy of its own, because a seam that silently applied policy would be a
/// second place to look for a refusal.
#[async_trait::async_trait]
pub trait AdapterFactory: Send + Sync {
    async fn open(
        &self,
        state: &AppState,
        doc: &DataSourceDoc,
    ) -> Result<Box<dyn SourceAdapter>, Refusal>;
}

/// The adapters a build carries beyond the ones core links directly.
///
/// Empty in a stock core build, which is the honest default: a stock build
/// refuses every kind it cannot serve, by name.
#[derive(Default)]
pub struct AdapterRegistry {
    factories: HashMap<AdapterKind, Arc<dyn AdapterFactory>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a factory for one kind. A second registration for the same kind
    /// replaces the first and returns it, so a host that wants to wrap or
    /// decorate an adapter can, and one that registers twice by mistake gets a
    /// value back rather than two live factories.
    pub fn register(
        &mut self,
        kind: AdapterKind,
        factory: Arc<dyn AdapterFactory>,
    ) -> Option<Arc<dyn AdapterFactory>> {
        self.factories.insert(kind, factory)
    }

    pub fn get(&self, kind: AdapterKind) -> Option<&Arc<dyn AdapterFactory>> {
        self.factories.get(&kind)
    }

    /// The kinds this build can serve through the registry, for `/version` and
    /// for the admin console — so an operator can see what a binary carries
    /// without provoking a refusal to find out.
    pub fn kinds(&self) -> Vec<AdapterKind> {
        let mut v: Vec<AdapterKind> = self.factories.keys().copied().collect();
        v.sort_by_key(|k| k.as_str());
        v
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

impl std::fmt::Debug for AdapterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterRegistry")
            .field(
                "kinds",
                &self.kinds().iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Never;

    #[async_trait::async_trait]
    impl AdapterFactory for Never {
        async fn open(
            &self,
            _state: &AppState,
            _doc: &DataSourceDoc,
        ) -> Result<Box<dyn SourceAdapter>, Refusal> {
            unreachable!("never opened in this test")
        }
    }

    #[test]
    fn a_stock_registry_is_empty_and_says_so() {
        let r = AdapterRegistry::new();
        assert!(r.is_empty());
        assert!(r.kinds().is_empty());
        assert!(r.get(AdapterKind::Databricks).is_none());
    }

    #[test]
    fn a_registered_kind_is_found_and_listed() {
        let mut r = AdapterRegistry::new();
        assert!(r
            .register(AdapterKind::Databricks, Arc::new(Never))
            .is_none());
        assert!(r.get(AdapterKind::Databricks).is_some());
        assert_eq!(r.kinds(), vec![AdapterKind::Databricks]);
        // Re-registering hands the previous factory back rather than leaving
        // two live registrations for one kind.
        assert!(r
            .register(AdapterKind::Databricks, Arc::new(Never))
            .is_some());
        assert_eq!(r.kinds().len(), 1);
    }

    #[test]
    fn the_refusal_names_the_kind_and_where_it_lives() {
        let r = Refusal::adapter_not_available(AdapterKind::Snowflake.as_str());
        let rendered = format!("{r:?}");
        assert!(rendered.contains("snowflake"), "{rendered}");
        assert!(rendered.contains("Enterprise"), "{rendered}");
    }
}
