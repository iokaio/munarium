// SPDX-License-Identifier: Apache-2.0
//! the ONE task-level model resolver (plan Part 3). Every model-using
//! task — session-turn completion, AI-assisted runbook validation
//!, BYOK embedding at index build — resolves its provider/model
//! through this chain:
//!
//!   API request override (if the runbook's allowOverrides policy permits)
//!     → spec.models.tasks[<task>]
//!     → spec.models.default
//!     → the tenant's `default` provider fallback chain (existing behavior)
//!
//! The resolved choice (and whether it was an override) is recorded with the
//! turn/interaction so cost reports can split native vs overridden spend.

use crate::error::CustomError;
use munarium_core::{KernelError, Result};
use munarium_runbooks::{ModelSpec, ModelsSpec, RunbookDoc};

/// An API-level override request (turn body / validate query params).
#[derive(Debug, Clone, Default)]
pub struct ModelOverride {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tier: Option<String>,
}

impl ModelOverride {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.tier.is_none()
    }
}

/// What the resolver hands to the provider gateway (`op_complete`/`op_embed`
/// take name + optional model + optional tier).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    /// ProviderConfig name ("default" engages the tenant fallback chain).
    pub provider_name: String,
    pub model: Option<String>,
    pub tier: Option<String>,
    /// True when an API override supplied any part of the choice.
    pub was_override: bool,
    /// Which layer decided: "override" | "task" | "runbook-default" | "tenant-default".
    pub source: &'static str,
}

impl ResolvedModel {
    pub fn audit_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider_name,
            "model": self.model,
            "tier": self.tier,
            "was_override": self.was_override,
            "source": self.source,
        })
    }
}

fn from_spec(spec: &ModelSpec, source: &'static str) -> ResolvedModel {
    ResolvedModel {
        provider_name: spec.provider.clone().unwrap_or_else(|| "default".into()),
        model: spec.model.clone(),
        tier: spec.tier.clone(),
        was_override: false,
        source,
    }
}

/// Resolve the model for `task` under a runbook's `models:` block.
/// `override_req` is rejected with the override-not-allowed slug unless the
/// runbook's policy permits the requested provider.
pub fn resolve_model(
    doc: &RunbookDoc,
    task: &str,
    override_req: Option<&ModelOverride>,
) -> std::result::Result<ResolvedModel, crate::error::ApiError> {
    let models = doc.spec.models.as_ref();
    if let Some(o) = override_req.filter(|o| !o.is_empty()) {
        let policy = models
            .map(|m| m.allow_overrides.clone())
            .unwrap_or_default();
        // An override must name its provider explicitly — a bare model/tier
        // override rides the runbook's resolved provider, which still
        // requires the policy to be open for that provider.
        let base = resolve_default(models, task);
        let provider = o.provider.clone().unwrap_or(base.provider_name.clone());
        if !policy.permits(&provider) {
            return Err(crate::error::ApiError::Custom(
                CustomError::override_not_allowed(&provider, &doc.runbook_ref()),
            ));
        }
        return Ok(ResolvedModel {
            provider_name: provider,
            model: o.model.clone().or(base.model),
            tier: o.tier.clone().or(base.tier),
            was_override: true,
            source: "override",
        });
    }
    Ok(resolve_default(models, task))
}

fn resolve_default(models: Option<&ModelsSpec>, task: &str) -> ResolvedModel {
    if let Some(m) = models {
        if let Some(spec) = m.tasks.get(task).filter(|s| !s.is_empty()) {
            return from_spec(spec, "task");
        }
        if let Some(spec) = m.default.as_ref().filter(|s| !s.is_empty()) {
            return from_spec(spec, "runbook-default");
        }
    }
    ResolvedModel {
        provider_name: "default".into(),
        model: None,
        tier: None,
        was_override: false,
        source: "tenant-default",
    }
}

/// Validate a task name against the known task levels. (Reserved for the
/// gRPC twins of the model-resolving endpoints.)
#[allow(dead_code)]
pub fn check_task(task: &str) -> Result<()> {
    if munarium_runbooks::TASK_LEVELS.contains(&task) {
        Ok(())
    } else {
        Err(KernelError::InvalidInput(format!(
            "unknown model task '{task}' (known: {})",
            munarium_runbooks::TASK_LEVELS.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_runbooks::parse_runbook;

    fn doc(models_yaml: &str) -> RunbookDoc {
        parse_runbook(&format!(
            r#"
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: t, version: 1 }}
spec:
  collections: [{{ name: a, shape: s@1 }}]
{models_yaml}
  steps: [{{ buildIndex: {{}} }}]
"#
        ))
        .expect("parses")
    }

    #[test]
    fn resolution_chain_orders_correctly() {
        let d = doc(r#"  models:
    default: { provider: anthropic-main, tier: capable }
    tasks:
      completion: { provider: anthropic-main, model: claude-fable-5 }"#);
        let c = resolve_model(&d, "completion", None).unwrap();
        assert_eq!(c.model.as_deref(), Some("claude-fable-5"));
        assert_eq!(c.source, "task");
        let v = resolve_model(&d, "validation", None).unwrap();
        assert_eq!(v.provider_name, "anthropic-main");
        assert_eq!(v.tier.as_deref(), Some("capable"));
        assert_eq!(v.source, "runbook-default");

        let bare = doc("");
        let t = resolve_model(&bare, "completion", None).unwrap();
        assert_eq!(t.provider_name, "default");
        assert_eq!(t.source, "tenant-default");
    }

    #[test]
    fn overrides_default_closed() {
        let d = doc("  models:\n    default: { provider: anthropic-main }");
        let o = ModelOverride {
            provider: Some("openai-main".into()),
            ..Default::default()
        };
        assert!(resolve_model(&d, "completion", Some(&o)).is_err());
        // empty override object is not an override
        assert!(resolve_model(&d, "completion", Some(&ModelOverride::default())).is_ok());
    }

    #[test]
    fn allowlist_governs_and_merges_with_base() {
        let d = doc(r#"  models:
    default: { provider: anthropic-main, tier: capable }
    allowOverrides: [openai-main]"#);
        let ok = ModelOverride {
            provider: Some("openai-main".into()),
            model: Some("gpt-5.4".into()),
            tier: None,
        };
        let r = resolve_model(&d, "completion", Some(&ok)).unwrap();
        assert!(r.was_override);
        assert_eq!(r.provider_name, "openai-main");
        assert_eq!(r.model.as_deref(), Some("gpt-5.4"));
        // base tier carries through when the override doesn't name one
        assert_eq!(r.tier.as_deref(), Some("capable"));

        let denied = ModelOverride {
            provider: Some("groq".into()),
            ..Default::default()
        };
        assert!(resolve_model(&d, "completion", Some(&denied)).is_err());

        // bare model override rides the runbook's provider — still gated
        let bare_model = ModelOverride {
            provider: None,
            model: Some("claude-opus-5".into()),
            tier: None,
        };
        assert!(resolve_model(&d, "completion", Some(&bare_model)).is_err());
    }

    #[test]
    fn allow_all_permits_any_provider() {
        let d = doc("  models:\n    allowOverrides: true");
        let o = ModelOverride {
            provider: Some("anything".into()),
            ..Default::default()
        };
        let r = resolve_model(&d, "completion", Some(&o)).unwrap();
        assert_eq!(r.provider_name, "anything");
        assert!(r.was_override);
    }
}
