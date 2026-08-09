//! Reusable zh-TW conversion core.
//!
//! This module owns the upstream scanner, ruleset, Tier 2 disambiguation,
//! fixer, and post-fix validation.  Network providers and chat transports
//! stay outside this boundary.

use std::sync::Arc;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use crate::engine::disambig::{disambiguate_batch, DisambigConfig, DisambigStats};
use crate::engine::s2t::S2TConverter;
use crate::engine::scan::{ContentType, Scanner};
use crate::engine::zhtype::{detect_chinese_type, ChineseType};
use crate::fixer::{apply_fixes_with_context, AppliedFix, FixMode};
use crate::rules::loader::load_embedded_ruleset;
use crate::rules::ruleset::{Issue, Profile};

/// Options shared by Discord, CLI, and other direct consumers of the core.
#[derive(Debug, Clone)]
pub struct CoreOptions {
    pub profile: Profile,
    pub content_type: ContentType,
    pub fix_mode: FixMode,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            profile: Profile::Base,
            content_type: ContentType::Plain,
            fix_mode: FixMode::LexicalContextual,
        }
    }
}

/// Compiled rules and converters.  Construct once and reuse for many messages.
pub struct CoreEngine {
    scanner: Scanner,
    s2t: S2TConverter,
    options: CoreOptions,
}

impl CoreEngine {
    /// Build the engine from the embedded upstream ruleset.
    pub fn from_embedded(options: CoreOptions) -> Result<Self> {
        let ruleset = load_embedded_ruleset()?;
        Ok(Self {
            scanner: Scanner::new(ruleset.spelling_rules, ruleset.case_rules),
            s2t: S2TConverter::new(),
            options,
        })
    }

    /// Analyze text through S2T, Tier 1 scanning, and Tier 2 disambiguation.
    pub fn analyze(&self, text: &str) -> CoreAnalysis {
        let input_was_simplified = detect_chinese_type(text) == ChineseType::Simplified;
        let normalized_text = if input_was_simplified {
            self.s2t.convert(text)
        } else {
            text.to_owned()
        };

        let scan = self.scanner.scan_for_content_type_with_config(
            &normalized_text,
            self.options.content_type,
            self.options.profile.config(),
        );
        let mut issues = scan.issues;
        let disambiguation = disambiguate_batch(
            &mut issues,
            &normalized_text,
            &DisambigConfig {
                profile: self.options.profile,
                ..Default::default()
            },
        );

        CoreAnalysis {
            normalized_text,
            input_was_simplified,
            issues,
            disambiguation,
        }
    }

    /// Apply deterministic fixes plus validated external decisions.
    pub fn apply(
        &self,
        analysis: &CoreAnalysis,
        decisions: &[IssueDecision],
    ) -> Result<CoreResult> {
        let mut issues = analysis.issues.clone();
        apply_decisions(&mut issues, decisions)?;

        let fix = apply_fixes_with_context(
            &analysis.normalized_text,
            &issues,
            self.options.fix_mode,
            &[],
            Some(self.scanner.segmenter()),
        );

        let validation = self.scanner.scan_for_content_type_with_config(
            &fix.text,
            self.options.content_type,
            self.options.profile.config(),
        );

        Ok(CoreResult {
            text: fix.text,
            issues: validation.issues,
            applied_fixes: fix.applied_fixes,
            input_was_simplified: analysis.input_was_simplified,
            changed: analysis.input_was_simplified || fix.applied > 0,
        })
    }

    pub fn options(&self) -> &CoreOptions {
        &self.options
    }
}

/// Result of the deterministic and local semantic analysis.
#[derive(Debug)]
pub struct CoreAnalysis {
    pub normalized_text: String,
    pub input_was_simplified: bool,
    pub issues: Vec<Issue>,
    pub disambiguation: DisambigStats,
}

/// A bounded external decision. `selected = None` keeps the original text;
/// otherwise it must be one of the issue's existing suggestions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueDecision {
    pub offset: usize,
    pub found: String,
    pub selected: Option<String>,
}

/// User-visible conversion result after re-scanning the output.
#[derive(Debug, Clone)]
pub struct CoreResult {
    pub text: String,
    pub issues: Vec<Issue>,
    pub applied_fixes: Vec<AppliedFix>,
    pub input_was_simplified: bool,
    pub changed: bool,
}

fn apply_decisions(issues: &mut [Issue], decisions: &[IssueDecision]) -> Result<()> {
    for decision in decisions {
        let issue = issues
            .iter_mut()
            .find(|issue| issue.offset == decision.offset && issue.found == decision.found);
        let Some(issue) = issue else {
            anyhow::bail!(
                "LLM decision does not match an issue: offset={}, found={:?}",
                decision.offset,
                decision.found
            );
        };
        if let Some(selected) = &decision.selected {
            ensure!(
                issue.suggestions.iter().any(|s| s == selected),
                "LLM selected {:?}, which is not an allowed suggestion for {:?}",
                selected,
                issue.found
            );
            issue.suggestions = Arc::from(vec![selected.clone()]);
        } else {
            issue.suggestions = Arc::from(Vec::<String>::new());
        }
        issue.llm_judged = true;
    }
    Ok(())
}
