//! Provider-neutral contracts for bounded semantic decisions and rewrites.

use serde::{Deserialize, Serialize};

use crate::core::CoreAnalysis;
use crate::rules::ruleset::Tier2Outcome;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequest {
    pub locale: String,
    pub original_text: String,
    pub issues: Vec<ContextIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextIssue {
    pub offset: usize,
    pub found: String,
    pub suggestions: Vec<String>,
    pub context: Option<String>,
    pub english: Option<String>,
    pub context_clues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub decisions: Vec<ContextDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextDecision {
    pub offset: usize,
    pub found: String,
    pub selected: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteRequest {
    pub locale: String,
    pub original_text: String,
    pub deterministic_draft: String,
    pub issues: Vec<ContextIssue>,
    pub protected_spans: Vec<ProtectedSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedSpan {
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteResponse {
    pub rewritten_text: String,
}

impl ContextRequest {
    pub fn from_analysis(original_text: &str, analysis: &CoreAnalysis) -> Self {
        Self {
            locale: "zh-TW".to_string(),
            original_text: original_text.to_string(),
            issues: analysis
                .issues
                .iter()
                .filter(|issue| {
                    issue.suggestions.len() > 1 || issue.tier2_outcome == Tier2Outcome::GrayZone
                })
                .map(|issue| ContextIssue {
                    offset: issue.offset,
                    found: issue.found.clone(),
                    suggestions: issue.suggestions.to_vec(),
                    context: issue.context.as_deref().map(str::to_string),
                    english: issue.english.as_deref().map(str::to_string),
                    context_clues: issue.context_clues.as_deref().unwrap_or_default().to_vec(),
                })
                .collect(),
        }
    }
}

/// Reject model output unless every decision points to the exact issue and
/// an allowlisted suggestion from the upstream ruleset.
pub fn validate_context_response(
    request: &ContextRequest,
    response: ContextResponse,
) -> anyhow::Result<Vec<crate::core::IssueDecision>> {
    let mut decisions = Vec::with_capacity(response.decisions.len());
    for decision in response.decisions {
        let issue = request
            .issues
            .iter()
            .find(|issue| issue.offset == decision.offset && issue.found == decision.found)
            .ok_or_else(|| anyhow::anyhow!("LLM decision does not match an issue"))?;
        anyhow::ensure!(
            issue.suggestions.iter().any(|s| s == &decision.selected),
            "LLM selected a term outside the ruleset candidate list"
        );
        decisions.push(crate::core::IssueDecision {
            offset: decision.offset,
            found: decision.found,
            selected: decision.selected,
        });
    }
    Ok(decisions)
}
