// SPDX-License-Identifier: Apache-2.0
//! Explicit controls for the qualified Claude models. No implicit budget growth.
use munarium_core::{provider::CompletionRequest, KernelError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicPolicy {
    #[serde(default)]
    pub models: BTreeMap<String, AnthropicModelPolicy>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicModelPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    Adaptive,
    Disabled,
}

// Recognize aliases and dated snapshots, without treating a future family as
// qualified merely because its name shares a prefix.
fn model_is(model: &str, alias: &str) -> bool {
    model == alias
        || model.strip_prefix(alias).is_some_and(|suffix| {
            suffix
                .strip_prefix('-')
                .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn qualified(model: &str) -> bool {
    model_is(model, "claude-sonnet-5") || model_is(model, "claude-fable-5-1")
}

impl AnthropicPolicy {
    pub fn validate(&self) -> Result<()> {
        for (model, policy) in &self.models {
            if !qualified(model) {
                return Err(KernelError::InvalidInput(
                    "anthropic model controls support Sonnet 5 and Fable 5.1 only".into(),
                ));
            }
            if policy.thinking == Some(Thinking::Disabled) && model_is(model, "claude-fable-5-1") {
                return Err(KernelError::InvalidInput(
                    "thinking cannot be disabled for Fable 5.1".into(),
                ));
            }
        }
        Ok(())
    }

    /// Called before gateway estimation/admission and by direct adapter users.
    /// Modern models do not support caller-controlled sampling; omit it even
    /// when an internal structured task requested deterministic temperature 0.
    pub fn prepare(&self, request: &mut CompletionRequest) -> Result<()> {
        self.validate()?;
        if qualified(&request.model) {
            request.temperature = None;
        }
        Ok(())
    }

    pub(crate) fn apply(&self, model: &str, body: &mut serde_json::Value) {
        if let Some(policy) = self.models.get(model) {
            if let Some(effort) = policy.effort {
                body["output_config"]["effort"] = serde_json::json!(effort);
            }
            if let Some(thinking) = policy.thinking {
                body["thinking"] = serde_json::json!({"type": thinking});
            }
        }
    }
}
