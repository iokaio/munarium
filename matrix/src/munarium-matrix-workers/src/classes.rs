// SPDX-License-Identifier: Apache-2.0
//! Authorization equivalence classes.
//!
//! Mode A's hardest question is not "how do I copy rows" — it is **"which of
//! these rows may which reader see?"** The answer must be decided before a
//! single document is written, because a document's collection carries its
//! access level and compartments, and a collection cannot be re-classified
//! after the fact without re-ingesting everything.
//!
//! So: rows are grouped into *authorization equivalence classes*, one
//! collection per class, and a source whose rows cannot be classified is
//! refused for mode A. There is deliberately no "default class" fallback — a
//! row that lands in a default is a row nobody decided about.

use munarium_matrix_core::{AuthorizationClass, Refusal};
use munarium_matrix_types::assets::{AuthorizationSpec, AuthorizationStrategy};

/// One class, with the collection it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClass {
    pub name: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    /// The credential this class reads under, when the source uses per-class
    /// principals. `None` under source-native policy.
    pub credential_ref: Option<String>,
}

impl ResolvedClass {
    pub fn as_core(&self) -> AuthorizationClass {
        AuthorizationClass {
            name: Some(self.name.clone()),
            access_level: self.access_level,
            compartments: self.compartments.clone(),
        }
    }

    /// The collection name a class's records land in. Deterministic, so a
    /// re-sync writes to the same place.
    pub fn collection_name(&self, source: &str, entity: &str) -> String {
        format!("{source}-{entity}-{}", self.name)
    }
}

/// Work out the classes a source's records fall into.
///
/// The refusals here are the point:
///
/// - `per_class_principals` with no classes declared is refused: the operator
///   asked for classification and supplied none.
/// - More classes than the cap is refused (`too_many_classes`): an unbounded
///   class count means an unbounded collection count, which is a resource
///   exhaustion an attacker could drive.
/// - `refuse` means the operator already decided this source cannot be
///   classified safely, and we honour it.
pub fn resolve_classes(spec: &AuthorizationSpec) -> Result<Vec<ResolvedClass>, Refusal> {
    match spec.strategy {
        AuthorizationStrategy::Refuse => Err(Refusal::policy_delegation_unavailable(
            "this source declares authorization strategy 'refuse': it cannot be materialized",
        )),
        AuthorizationStrategy::SourceNative => {
            // The source filters per principal, so every row this principal can
            // read belongs to one class. Its level and compartments come from
            // the single declared class when there is one, or default to the
            // most restrictive thing we can say: level 0 and no compartments
            // means "no additional restriction", which is only safe because
            // the SOURCE already filtered.
            let class = spec
                .classes
                .first()
                .map(|c| ResolvedClass {
                    name: c.name.clone(),
                    access_level: c.access_level,
                    compartments: c.compartments.clone(),
                    credential_ref: c.credential_ref.clone(),
                })
                .unwrap_or_else(|| ResolvedClass {
                    name: "source-native".to_string(),
                    access_level: 0,
                    compartments: vec![],
                    credential_ref: None,
                });
            Ok(vec![class])
        }
        AuthorizationStrategy::PerClassPrincipals => {
            if spec.classes.is_empty() {
                return Err(Refusal::invalid(
                    "not_covered",
                    "strategy per_class_principals declares no classes, so no row can be \
                     classified; materialization is refused rather than defaulted",
                ));
            }
            if spec.classes.len() > spec.max_authorization_classes {
                return Err(Refusal::too_many_classes(
                    spec.classes.len(),
                    spec.max_authorization_classes,
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            let mut out = Vec::new();
            for c in &spec.classes {
                if !seen.insert(&c.name) {
                    return Err(Refusal::invalid(
                        "not_covered",
                        format!("authorization class '{}' is declared twice", c.name),
                    ));
                }
                if c.credential_ref.is_none() {
                    return Err(Refusal::policy_delegation_unavailable(format!(
                        "class '{}' has no principal to read under",
                        c.name
                    )));
                }
                out.push(ResolvedClass {
                    name: c.name.clone(),
                    access_level: c.access_level,
                    compartments: c.compartments.clone(),
                    credential_ref: c.credential_ref.clone(),
                });
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_types::assets::AuthorizationClassSpec;

    fn class(name: &str, level: i32, cred: Option<&str>) -> AuthorizationClassSpec {
        AuthorizationClassSpec {
            name: name.into(),
            compartments: vec!["sales".into()],
            access_level: level,
            credential_ref: cred.map(String::from),
        }
    }

    #[test]
    fn source_native_yields_exactly_one_class() {
        let spec = AuthorizationSpec {
            strategy: AuthorizationStrategy::SourceNative,
            classes: vec![],
            max_authorization_classes: 16,
        };
        let classes = resolve_classes(&spec).unwrap();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "source-native");
        assert!(classes[0].credential_ref.is_none());
    }

    #[test]
    fn per_class_principals_needs_classes_and_principals() {
        let empty = AuthorizationSpec {
            strategy: AuthorizationStrategy::PerClassPrincipals,
            classes: vec![],
            max_authorization_classes: 16,
        };
        assert!(resolve_classes(&empty).is_err());

        let no_principal = AuthorizationSpec {
            strategy: AuthorizationStrategy::PerClassPrincipals,
            classes: vec![class("emea", 2, None)],
            max_authorization_classes: 16,
        };
        let err = resolve_classes(&no_principal).unwrap_err();
        assert_eq!(err.code, "policy_delegation_unavailable");
    }

    #[test]
    fn the_class_cap_is_enforced_at_seventeen() {
        let classes: Vec<_> = (0..17)
            .map(|i| class(&format!("c{i}"), 1, Some("ref")))
            .collect();
        let spec = AuthorizationSpec {
            strategy: AuthorizationStrategy::PerClassPrincipals,
            classes,
            max_authorization_classes: 16,
        };
        let err = resolve_classes(&spec).unwrap_err();
        assert_eq!(err.code, "too_many_classes");
        assert!(err.message.contains("17"), "{}", err.message);
    }

    #[test]
    fn sixteen_classes_are_accepted() {
        let classes: Vec<_> = (0..16)
            .map(|i| class(&format!("c{i}"), 1, Some("ref")))
            .collect();
        let spec = AuthorizationSpec {
            strategy: AuthorizationStrategy::PerClassPrincipals,
            classes,
            max_authorization_classes: 16,
        };
        assert_eq!(resolve_classes(&spec).unwrap().len(), 16);
    }

    #[test]
    fn a_duplicate_class_name_is_refused() {
        let spec = AuthorizationSpec {
            strategy: AuthorizationStrategy::PerClassPrincipals,
            classes: vec![class("emea", 2, Some("a")), class("emea", 3, Some("b"))],
            max_authorization_classes: 16,
        };
        assert!(resolve_classes(&spec).is_err());
    }

    #[test]
    fn a_source_declared_unclassifiable_is_refused_for_materialization() {
        let spec = AuthorizationSpec {
            strategy: AuthorizationStrategy::Refuse,
            classes: vec![],
            max_authorization_classes: 16,
        };
        let err = resolve_classes(&spec).unwrap_err();
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::Denied);
    }

    #[test]
    fn collection_names_are_deterministic() {
        let c = ResolvedClass {
            name: "sales-emea".into(),
            access_level: 2,
            compartments: vec!["sales".into()],
            credential_ref: None,
        };
        assert_eq!(
            c.collection_name("crm", "opportunities"),
            "crm-opportunities-sales-emea"
        );
    }
}
