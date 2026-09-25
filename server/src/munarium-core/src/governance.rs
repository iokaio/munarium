// SPDX-License-Identifier: Apache-2.0
//! Immutable, content-addressed governance profiles carried by memory versions.
//! Claim/anchor version_id is the durable reference; transitions never edit history.
use crate::gates::{
    check_anchor_consistency_with_policies, check_ledger_conflict_with_policies, ComparisonPolicies,
};
use crate::ledger::ComparisonPolicy;
use crate::types::{Candidate, ClaimType, MeshSnapshot, ProposedClaim};
use crate::{KernelError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValuePolicy {
    pub subject: String,
    pub key: String,
    pub comparison: String,
}

/// Schema versions describe the profile format; algorithm IDs describe semantics.
/// Unknown fields/algorithms fail closed rather than dropping future obligations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernancePolicy {
    pub schema_version: u32,
    pub values: Vec<ValuePolicy>,
}

impl Default for GovernancePolicy {
    fn default() -> Self {
        Self {
            schema_version: 1,
            values: Vec::new(),
        }
    }
}
impl GovernancePolicy {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(invalid("unsupported governance schema"));
        }
        let mut keys = BTreeMap::new();
        for v in &self.values {
            for part in [&v.subject, &v.key] {
                if part.is_empty() || part.trim() != part || part.contains('.') {
                    return Err(invalid(
                        "policy subject/key must be nonempty, unpadded and dot-free",
                    ));
                }
            }
            ComparisonPolicy::from_id(&v.comparison)?;
            if keys.insert((&v.subject, &v.key), ()).is_some() {
                return Err(invalid("duplicate governance value binding"));
            }
        }
        Ok(())
    }
    pub fn revision(&self) -> Result<String> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical
            .values
            .sort_by(|a, b| (&a.subject, &a.key).cmp(&(&b.subject, &b.key)));
        let bytes = serde_json::to_vec(&canonical).map_err(|e| invalid(&e.to_string()))?;
        Ok(format!(
            "governance-v1:{}",
            hex::encode(Sha256::digest(bytes))
        ))
    }
    pub fn comparisons(&self) -> Result<ComparisonPolicies> {
        self.validate()?;
        ComparisonPolicies::try_from_bindings(
            self.values
                .iter()
                .map(|v| {
                    Ok((
                        format!("{}.{}", v.subject, v.key),
                        ComparisonPolicy::from_id(&v.comparison)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        )
    }
    pub fn from_stored_metadata(metadata: Option<&Value>) -> Result<Self> {
        let policy = Self::from_metadata(metadata)?;
        if let Some(meta) = metadata.filter(|m| m.get("governance_policy").is_some()) {
            if meta.get("governance_revision").and_then(Value::as_str)
                != Some(policy.revision()?.as_str())
            {
                return Err(invalid(
                    "missing or inconsistent stored governance revision",
                ));
            }
        }
        Ok(policy)
    }
    pub fn from_metadata(metadata: Option<&Value>) -> Result<Self> {
        let Some(value) = metadata.and_then(|m| m.get("governance_policy")) else {
            return Ok(Self::default());
        };
        let policy: Self = serde_json::from_value(value.clone())
            .map_err(|e| invalid(&format!("invalid governance policy: {e}")))?;
        policy.validate()?;
        Ok(policy)
    }
}
fn invalid(message: &str) -> KernelError {
    KernelError::InvalidInput(message.into())
}

/// Normalize version metadata inside the store's creation transaction/lock.
/// A changed child profile requires an explicit old revision and parent head.
/// The assessment is computed by the store, never accepted from the caller.
pub fn prepare_version_metadata(
    metadata: Option<Value>,
    parent_metadata: Option<&Value>,
    parent: Option<&MeshSnapshot>,
) -> Result<Option<Value>> {
    if metadata.as_ref().is_some_and(|m| {
        m.get("governance_assessment").is_some() || m.get("governance_revision").is_some()
    }) {
        return Err(invalid(
            "governance revision and assessment are server-owned",
        ));
    }
    let old = GovernancePolicy::from_stored_metadata(parent_metadata)?;
    let explicit = metadata
        .as_ref()
        .is_some_and(|m| m.get("governance_policy").is_some());
    let next = if explicit {
        GovernancePolicy::from_metadata(metadata.as_ref())?
    } else {
        old.clone()
    };
    let changed = old.revision()? != next.revision()?;
    let transition = metadata
        .as_ref()
        .and_then(|m| m.get("governance_transition"));
    if transition.is_some() && (parent.is_none() || !changed) {
        return Err(invalid(
            "governance transition requires a changed child policy",
        ));
    }
    let mut assessment = None;
    if let Some(snapshot) = parent {
        if changed {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Transition {
                from_revision: String,
                expected_head: u64,
                reason: String,
            }
            let t: Transition = serde_json::from_value(
                transition
                    .cloned()
                    .ok_or_else(|| invalid("policy change requires governance_transition"))?,
            )
            .map_err(|e| invalid(&format!("invalid governance transition: {e}")))?;
            if t.from_revision != old.revision()? || t.reason.trim().is_empty() {
                return Err(invalid(
                    "governance transition requires the current revision and a reason",
                ));
            }
            let actual = snapshot
                .as_of_seq
                .ok_or_else(|| invalid("transition requires a pinned snapshot"))?;
            if t.expected_head != actual {
                return Err(KernelError::HeadConflict {
                    expected: t.expected_head,
                    actual,
                });
            }
            let policies = next.comparisons()?;
            let mut results = Vec::new();
            // Only the last peer for each key can be canon under the existing gate.
            // Group once: avoid cloning the full snapshot for every assessed fact.
            let mut by_key: BTreeMap<String, Vec<&crate::types::Claim>> = BTreeMap::new();
            for fact in &snapshot.facts {
                by_key.entry(fact.claim_key()).or_default().push(fact);
            }
            // Comparison assessment only: preserve original dispositions and shape decisions.
            for fact in &snapshot.facts {
                let key = fact.claim_key();
                let peer = by_key[&key].iter().rev().find(|f| f.id != fact.id);
                let peers = MeshSnapshot {
                    facts: peer.map(|f| vec![(**f).clone()]).unwrap_or_default(),
                    anchors: snapshot
                        .anchors
                        .get(&key)
                        .map(|a| BTreeMap::from([(key, a.clone())]))
                        .unwrap_or_default(),
                    ..Default::default()
                };
                let candidate = Candidate {
                    scope_path: fact.scope_path.clone(),
                    claims: vec![ProposedClaim {
                        claim_type: ClaimType::Fact,
                        subject: fact.subject.clone(),
                        key: fact.key.clone(),
                        value: fact.value.clone(),
                        supersedes_id: None,
                    }],
                    ..Default::default()
                };
                let mut findings =
                    check_anchor_consistency_with_policies(&peers, &candidate, &policies);
                findings.extend(check_ledger_conflict_with_policies(
                    &peers, &candidate, &policies,
                ));
                results.push(json!({"claim_id": fact.id, "original_status": fact.status, "findings": findings}));
            }
            assessment = Some(
                json!({"schema_version": 1, "kind": "comparison-transition-v1", "from_revision": old.revision()?,
                "to_revision": next.revision()?, "parent_version_id": snapshot.version_id, "as_of_seq": actual,
                "claims": results, "anchors": snapshot.anchors.values().map(|a| &a.id).collect::<Vec<_>>() }),
            );
        }
    }
    let inherited = parent_metadata.is_some_and(|m| m.get("governance_policy").is_some());
    if !explicit && !inherited && transition.is_none() {
        return Ok(metadata);
    }
    let mut meta = metadata.unwrap_or_else(|| json!({}));
    let object = meta
        .as_object_mut()
        .ok_or_else(|| invalid("governed version metadata must be an object"))?;
    object.insert(
        "governance_policy".into(),
        serde_json::to_value(&next).map_err(|e| {
            KernelError::Storage(format!("governance profile did not serialize: {e}"))
        })?,
    );
    object.insert("governance_revision".into(), json!(next.revision()?));
    if let Some(a) = assessment {
        object.insert("governance_assessment".into(), a);
    }
    Ok(Some(meta))
}

/// Companion record for export through the existing findings transport.
pub fn policy_finding(
    version_id: &str,
    metadata: Option<&Value>,
) -> Option<crate::types::GateFinding> {
    let meta = metadata?;
    meta.get("governance_policy")?;
    Some(crate::types::GateFinding {
        rule_id: "governance.policy-revision".into(),
        severity: crate::types::Severity::Info,
        message: "Immutable governance profile and transition assessment".into(),
        scope_path: None,
        detail: Some(
            json!({"version_id": version_id, "governance_policy": meta["governance_policy"],
            "governance_revision": meta["governance_revision"], "transition": meta.get("governance_transition"),
            "assessment": meta.get("governance_assessment")}),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revision_is_order_independent_and_future_obligations_fail_closed() {
        let a = ValuePolicy {
            subject: "file".into(),
            key: "path".into(),
            comparison: "exact-string-v1".into(),
        };
        let b = ValuePolicy {
            subject: "file".into(),
            key: "label".into(),
            comparison: "legacy-text-v1".into(),
        };
        let p = GovernancePolicy {
            schema_version: 1,
            values: vec![a.clone(), b.clone()],
        };
        let q = GovernancePolicy {
            schema_version: 1,
            values: vec![b, a.clone()],
        };
        assert_eq!(p.revision().unwrap(), q.revision().unwrap());
        assert!(GovernancePolicy {
            schema_version: 2,
            ..p.clone()
        }
        .validate()
        .is_err());
        assert!(GovernancePolicy {
            values: vec![a.clone(), a],
            ..p.clone()
        }
        .validate()
        .is_err());
        for v in [
            json!({"schema_version":1,"values":[],"future_rule":true}),
            json!({"schema_version":1,"values":[{"subject":"file","key":"path","comparison":"exact-string-v2"}]}),
            json!({"schema_version":1,"values":[{"subject":"file.path","key":"name","comparison":"exact-string-v1"}]}),
        ] {
            assert!(
                GovernancePolicy::from_metadata(Some(&json!({"governance_policy":v}))).is_err()
            );
        }
        assert!(
            GovernancePolicy::from_stored_metadata(Some(&json!({"governance_policy":p}))).is_err()
        );
    }
}
