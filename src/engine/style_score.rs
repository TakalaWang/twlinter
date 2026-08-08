// Composite three-axis style scorecard.
//
// Pure aggregation: AI-likelihood, translationese density, terminology
// consistency.  Three orthogonal scores, NEVER collapsed into a single
// number — the consumer chooses which axis to act on.
//
// No new detectors, no scoring-module changes — this module reads
// `AiSignatureReport`, `TranslationeseReport`, and the issue list, then
// emits a flat scorecard.  Wired by the CLI `--detect-style` path and the
// MCP `detect_style` parameter.

use serde::{Deserialize, Serialize};

use crate::engine::ai_score::AiSignatureReport;
use crate::engine::translationese_score::TranslationeseReport;
use crate::rules::ruleset::{Issue, IssueType, Severity};

/// Three orthogonal style scores.  Each axis is `Option<f32>`: `None`
/// means the axis was not computed (e.g. AI detection disabled).  Values
/// are in [0.0, 1.0] but each axis carries its own meaning — they are
/// presented side by side, never averaged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StyleScores {
    /// AI-likelihood score.  Higher = more AI-tell signals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai: Option<f32>,
    /// Translationese density.  Higher = more 歐化 tells.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translationese: Option<f32>,
    /// Terminology consistency proxy.  Higher = more cross-strait /
    /// confusable issues per 1000 chars.  Capped at 1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency: Option<f32>,
}

/// Top contributing issues per axis.  Limited to ≤5 per axis to keep
/// payloads small.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopIssuesPerAxis {
    pub ai: Vec<TopIssue>,
    pub translationese: Vec<TopIssue>,
    pub consistency: Vec<TopIssue>,
}

/// Lightweight summary of a contributing issue (no full Issue payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopIssue {
    pub line: usize,
    pub col: usize,
    pub found: String,
    pub severity: Severity,
    pub rule_type: IssueType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

impl TopIssue {
    fn from_issue(issue: &Issue) -> Self {
        Self {
            line: issue.line,
            col: issue.col,
            found: issue.found.clone(),
            severity: issue.severity,
            rule_type: issue.rule_type,
            context: issue.context.as_ref().map(|c| c.to_string()),
        }
    }
}

/// Composite scorecard.  Three axes never combined.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StyleScorecard {
    pub style_scores: StyleScores,
    pub top_issues_per_axis: TopIssuesPerAxis,
}

impl StyleScorecard {
    /// Aggregate scores from existing scanner outputs.  `text_chars` is
    /// the character count of the analyzed text — the consistency axis
    /// scales by it.  `None` values indicate axes that were not computed
    /// for this run.
    pub fn build(
        ai: Option<&AiSignatureReport>,
        translationese: Option<&TranslationeseReport>,
        issues: &[Issue],
        text_chars: usize,
    ) -> Self {
        let consistency_score = compute_consistency_score(issues, text_chars);
        Self {
            style_scores: StyleScores {
                ai: ai.map(|s| s.score),
                translationese: translationese.map(|s| s.score),
                consistency: Some(consistency_score),
            },
            top_issues_per_axis: top_issues_per_axis(issues),
        }
    }
}

/// Consistency proxy: count of `cross_strait` + `confusable` issues per
/// 1000 chars, capped at 1.0.  Documents that mix 程式/程序 and
/// 記憶體/內存 raise this axis even when individual rules are
/// Info-severity.
fn compute_consistency_score(issues: &[Issue], text_chars: usize) -> f32 {
    if text_chars == 0 {
        return 0.0;
    }
    let count = issues
        .iter()
        .filter(|i| matches!(i.rule_type, IssueType::CrossStrait | IssueType::Confusable))
        .count();
    let per_1000 = (count as f32) * 1000.0 / (text_chars as f32);
    per_1000.min(1.0)
}

/// Top ≤5 issues per axis, ordered by severity (Error > Warning > Info)
/// then by line.  No ranking signal beyond severity — this is a triage
/// list, not a ranking.
fn top_issues_per_axis(issues: &[Issue]) -> TopIssuesPerAxis {
    let mut out = TopIssuesPerAxis::default();
    for issue in issues {
        let bucket = match issue.rule_type {
            IssueType::AiStyle => &mut out.ai,
            IssueType::Translationese => &mut out.translationese,
            IssueType::CrossStrait | IssueType::Confusable => &mut out.consistency,
            _ => continue,
        };
        bucket.push(TopIssue::from_issue(issue));
    }
    for bucket in [&mut out.ai, &mut out.translationese, &mut out.consistency] {
        bucket.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.line.cmp(&b.line)));
        bucket.truncate(5);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn issue(rule_type: IssueType, severity: Severity, line: usize) -> Issue {
        Issue {
            offset: 0,
            length: 0,
            line,
            col: 0,
            found: "x".into(),
            suggestions: Arc::from(Vec::<String>::new()),
            suggested_rewrite: None,
            rule_type,
            severity,
            context: None,
            english: None,
            context_clues: None,
            anchor_match: None,
            glossary_banned: false,
            tier2_outcome: Default::default(),
            llm_judged: false,
            spelling_rule_idx: None,
            table_cell: None,
            editorial_confidence: None,
        }
    }

    #[test]
    fn empty_inputs_zero_consistency() {
        let card = StyleScorecard::build(None, None, &[], 0);
        assert!(card.style_scores.ai.is_none());
        assert!(card.style_scores.translationese.is_none());
        assert_eq!(card.style_scores.consistency, Some(0.0));
        assert!(card.top_issues_per_axis.ai.is_empty());
    }

    #[test]
    fn three_axes_orthogonal_never_collapsed() {
        let issues = vec![
            issue(IssueType::AiStyle, Severity::Info, 1),
            issue(IssueType::Translationese, Severity::Warning, 2),
            issue(IssueType::CrossStrait, Severity::Warning, 3),
            issue(IssueType::CrossStrait, Severity::Info, 4),
        ];
        let card = StyleScorecard::build(None, None, &issues, 1000);
        // 2 cross_strait per 1000 chars = 2.0 raw → capped 1.0.
        assert_eq!(card.style_scores.consistency, Some(1.0));
        assert_eq!(card.top_issues_per_axis.ai.len(), 1);
        assert_eq!(card.top_issues_per_axis.translationese.len(), 1);
        assert_eq!(card.top_issues_per_axis.consistency.len(), 2);
    }

    #[test]
    fn top_issues_capped_at_5_per_axis() {
        let issues: Vec<Issue> = (0..10)
            .map(|i| issue(IssueType::AiStyle, Severity::Info, i))
            .collect();
        let card = StyleScorecard::build(None, None, &issues, 1000);
        assert_eq!(card.top_issues_per_axis.ai.len(), 5);
    }

    #[test]
    fn document_level_scores_survive_without_issue_entries() {
        let ai = AiSignatureReport {
            score: 0.7,
            markers: Vec::new(),
            top_signals: Vec::new(),
            sentence_variability: None,
            zero_width_count: 0,
            punctuation_profile: None,
        };
        let card = StyleScorecard::build(Some(&ai), None, &[], 1000);
        assert_eq!(card.style_scores.ai, Some(0.7));
    }

    #[test]
    fn top_issues_sorted_by_severity_then_line() {
        let issues = vec![
            issue(IssueType::AiStyle, Severity::Info, 100),
            issue(IssueType::AiStyle, Severity::Error, 200),
            issue(IssueType::AiStyle, Severity::Warning, 300),
        ];
        let card = StyleScorecard::build(None, None, &issues, 1000);
        let top = &card.top_issues_per_axis.ai;
        assert_eq!(top[0].severity, Severity::Error);
        assert_eq!(top[1].severity, Severity::Warning);
        assert_eq!(top[2].severity, Severity::Info);
    }
}
