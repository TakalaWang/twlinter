// Project-level glossary (35.9) — `banned`, `preferred`, `proper_nouns`.
//
// Layered above the embedded ruleset and pack store but below banned-term
// enforcement and translation memory.  Precedence per TODO 35.9:
// glossary `banned` > TM > glossary `preferred` > domain pack > embedded
// ruleset.
//
// `banned`     — terms that must always fire, regardless of context_clues.
// `preferred`  — TW forms used by 35.1 to choose the canonical suggestion.
// `proper_nouns` — never flag (added to the suppression list).

use crate::engine::excluded::{is_excluded, ByteRange};
use crate::rules::ruleset::{Issue, IssueType, Severity};

/// Runtime glossary used by the scan post-processor.
#[derive(Debug, Default, Clone)]
pub struct ProjectGlossary {
    pub banned: Vec<String>,
    pub preferred: Vec<String>,
    pub proper_nouns: Vec<String>,
}

impl ProjectGlossary {
    pub fn is_empty(&self) -> bool {
        self.banned.is_empty() && self.preferred.is_empty() && self.proper_nouns.is_empty()
    }
}

/// Mark an issue as protected by glossary banned-term precedence.
pub fn mark_glossary_banned(issue: &mut Issue) {
    issue.glossary_banned = true;
}

/// Per TODO 35.9 precedence (banned > TM), TM must NOT downgrade these.
pub fn is_glossary_banned(issue: &Issue) -> bool {
    issue.glossary_banned
}

/// Apply glossary precedence to a freshly-scanned issue list.
///
/// 1. Suppress any issue whose `found` text exactly matches a proper noun
///    (highest-priority suppression after TM, lowest noise to authors).
/// 2. Inject a synthetic CrossStrait `Error` for each occurrence of a
///    banned term that the embedded ruleset failed to flag (e.g. because
///    `context_clues` didn't match) AND whose offset falls outside any
///    `excluded` range.  Banned-term enforcement respects code blocks,
///    URLs, suppression markers, and YAML frontmatter exclusions just
///    like regular rules do.  Banned-term enforcement is project-wide
///    truth: the author asked for these to always fire.
///
/// Returns the modified issue list, sorted by offset.  Synthetic
/// banned-term issues carry `line: 0, col: 0` — callers must run
/// [LineIndex::fill_line_col_sorted] before reporting.
pub fn apply_glossary(
    text: &str,
    excluded: &[ByteRange],
    mut issues: Vec<Issue>,
    glossary: &ProjectGlossary,
) -> Vec<Issue> {
    if glossary.is_empty() {
        return issues;
    }

    // -- (1) Proper-noun suppression.
    if !glossary.proper_nouns.is_empty() {
        issues.retain(|i| !glossary.proper_nouns.iter().any(|pn| pn == &i.found));
    }

    // -- (2) Banned-term injection.  For each occurrence:
    //   - If an existing issue covers it, upgrade that issue in place
    //     (severity → Error, internal glossary-banned flag).
    //     Upgrading instead of injecting prevents duplicate output AND
    //     guarantees the banned-term report survives TM downgrade,
    //     which honors the documented `banned > TM` precedence.
    //   - Otherwise inject a synthetic Error issue.
    for banned in &glossary.banned {
        if banned.is_empty() {
            continue;
        }
        let pattern_len = banned.len();
        let mut start = 0;
        while let Some(rel) = text[start..].find(banned.as_str()) {
            let abs = start + rel;
            // Skip matches that fall inside an exclusion zone (code
            // fences, URLs, file paths, frontmatter, suppression
            // markers).  Without this guard, banned terms would fire
            // inside blocks the rest of the pipeline carefully respects.
            if is_excluded(abs, abs + pattern_len, excluded) {
                start = abs + pattern_len;
                continue;
            }
            let covering_idx = issues
                .iter()
                .position(|i| i.offset <= abs && abs + pattern_len <= i.offset + i.length);
            match covering_idx {
                Some(idx) => {
                    let i = &mut issues[idx];
                    i.severity = Severity::Error;
                    mark_glossary_banned(i);
                }
                None => {
                    let mut synthetic = Issue::new(
                        abs,
                        pattern_len,
                        banned.clone(),
                        Vec::new(),
                        IssueType::CrossStrait,
                        Severity::Error,
                    );
                    mark_glossary_banned(&mut synthetic);
                    issues.push(synthetic);
                }
            }
            start = abs + pattern_len;
        }
    }

    issues.sort_by_key(|i| i.offset);
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn issue(offset: usize, found: &str) -> Issue {
        Issue::new(
            offset,
            found.len(),
            found,
            vec!["X".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        )
    }

    #[test]
    fn empty_glossary_passes_through() {
        let glossary = ProjectGlossary::default();
        let issues = vec![issue(0, "線程")];
        let out = apply_glossary("線程", &[], issues, &glossary);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn proper_noun_suppresses_matching_issue() {
        let glossary = ProjectGlossary {
            proper_nouns: vec!["TSMC".into()],
            ..ProjectGlossary::default()
        };
        let issues = vec![issue(0, "TSMC"), issue(10, "線程")];
        let out = apply_glossary("TSMC ... 線程", &[], issues, &glossary);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].found, "線程");
    }

    #[test]
    fn banned_term_injects_synthetic_when_not_already_flagged() {
        let glossary = ProjectGlossary {
            banned: vec!["線程".into()],
            ..ProjectGlossary::default()
        };
        let text = "我們的線程實作。";
        let out = apply_glossary(text, &[], Vec::new(), &glossary);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].found, "線程");
        assert_eq!(out[0].severity, Severity::Error);
        assert_eq!(out[0].rule_type, IssueType::CrossStrait);
    }

    #[test]
    fn banned_term_upgrades_existing_same_span_issue() {
        // Covering issue keeps its human-facing context but gets
        // Severity::Error plus the internal glossary-banned marker so
        // TM cannot downgrade the only report.
        let glossary = ProjectGlossary {
            banned: vec!["線程".into()],
            ..ProjectGlossary::default()
        };
        let text = "線程";
        let mut existing = issue(0, "線程");
        existing.context = Some(Arc::from("@domain IT。原始說明"));
        let out = apply_glossary(text, &[], vec![existing], &glossary);
        assert_eq!(out.len(), 1, "existing issue must not be duplicated");
        assert_eq!(out[0].severity, Severity::Error);
        assert_eq!(out[0].context.as_deref(), Some("@domain IT。原始說明"));
        assert!(is_glossary_banned(&out[0]));
    }

    #[test]
    fn banned_term_upgrades_larger_covering_issue() {
        // Banned 用戶 inside an existing 用戶介面 issue: the compound
        // issue is the user-visible alert, but it must carry
        // glossary-banned provenance so TM does not downgrade it.
        let glossary = ProjectGlossary {
            banned: vec!["用戶".into()],
            ..ProjectGlossary::default()
        };
        let text = "用戶介面";
        let existing = issue(0, "用戶介面");
        let out = apply_glossary(text, &[], vec![existing], &glossary);
        assert_eq!(
            out.len(),
            1,
            "covering issue must absorb the banned hit, not duplicate"
        );
        assert_eq!(out[0].found, "用戶介面");
        assert_eq!(out[0].severity, Severity::Error);
        assert!(is_glossary_banned(&out[0]));
    }

    #[test]
    fn banned_term_finds_multiple_occurrences() {
        let glossary = ProjectGlossary {
            banned: vec!["線程".into()],
            ..ProjectGlossary::default()
        };
        let text = "線程一、線程二";
        let out = apply_glossary(text, &[], Vec::new(), &glossary);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|i| i.found == "線程"));
    }

    #[test]
    fn banned_and_proper_noun_compose() {
        let glossary = ProjectGlossary {
            banned: vec!["內存".into()],
            proper_nouns: vec!["MediaTek".into()],
            ..ProjectGlossary::default()
        };
        let text = "MediaTek 在內存設計上的優勢";
        let issues = vec![issue(0, "MediaTek"), issue(11, "優化")];
        let out = apply_glossary(text, &[], issues, &glossary);
        // MediaTek issue suppressed.
        assert!(!out.iter().any(|i| i.found == "MediaTek"));
        // 內存 banned synthetic added.
        assert!(out
            .iter()
            .any(|i| i.found == "內存" && i.severity == Severity::Error));
    }
}
