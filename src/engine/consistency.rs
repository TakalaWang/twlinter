// 35.1 — Document-wide terminology consistency report.
//
// Groups scan issues by their `english` field (natural equivalence
// class), then for each group checks whether the canonical zh-TW form
// also appears elsewhere in the document.  Mixed usage produces a
// `Consistency` diagnostic alerting the author that the same concept
// is referred to with both regional variants.
//
// TM-suppressed issues are excluded from consistency grouping — those
// are user-approved overrides, not inadvertent inconsistency.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::rules::glossary::ProjectGlossary;
use crate::rules::ruleset::{Issue, IssueType, Severity};

/// One occurrence of a calque in the document — used to anchor the
/// consistency diagnostic.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsistencyOccurrence {
    pub offset: usize,
    pub line: usize,
    pub col: usize,
    pub found: String,
}

/// Aggregated consistency record for one equivalence class.  All fields
/// are populated only when both the calque AND a canonical zh-TW form
/// appear in the same document.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsistencyGroup {
    /// English anchor (natural equivalence-class key).
    pub term_group: String,
    /// The TW-preferred form the linter recommends.
    pub preferred: String,
    /// All occurrences of the calque(s) in this group.
    pub occurrences: Vec<ConsistencyOccurrence>,
}

/// Top-level consistency report.  Empty `groups` means no mixed usage.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConsistencyReport {
    pub groups: Vec<ConsistencyGroup>,
}

impl ConsistencyReport {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// Build a consistency report from raw scan issues.
///
/// Algorithm:
///   1. Filter to CrossStrait / Confusable issues with non-empty
///      `english`.  Those are the cleanest equivalence-class anchors.
///   2. Skip issues whose severity is Info — TM-suppressed downgrades
///      land at Info; they are user-approved and should not count.
///   3. Group by `english`.  For each group, choose the TW-preferred
///      canonical form from `glossary.preferred` when that preferred
///      form is already present in the document; otherwise fall back to
///      the first suggestion.
///   4. Check whether that canonical form ALSO appears as a substring
///      anywhere in `text`.  If yes (and the calque is also present),
///      both regional variants coexist → emit a group.
pub fn compute_consistency_report(
    text: &str,
    issues: &[Issue],
    glossary: &ProjectGlossary,
) -> ConsistencyReport {
    let mut grouped: BTreeMap<String, Vec<&Issue>> = BTreeMap::new();

    for issue in issues {
        let eligible = matches!(
            issue.rule_type,
            IssueType::CrossStrait | IssueType::Confusable
        ) && issue.severity != Severity::Info;
        if !eligible {
            continue;
        }
        let Some(english) = issue.english.as_deref().filter(|e| !e.is_empty()) else {
            continue;
        };
        grouped.entry(english.to_string()).or_default().push(issue);
    }

    let mut report = ConsistencyReport::default();

    for (english, issues_in_group) in grouped {
        let canonical = preferred_canonical_for_group(text, &issues_in_group, glossary);
        let Some(canonical) = canonical else { continue };
        // Mixed usage: the canonical TW form must appear independently
        // somewhere in the document (i.e. NOT as a substring of an
        // already-flagged calque region).  Cheap proxy: the canonical
        // form is found at an offset that is not covered by any
        // `from`-span issue.  For the typical case where canonical and
        // calque differ in characters, plain `text.contains` is
        // sufficient because the calque span doesn't contain the
        // canonical form as a substring.
        if !text.contains(canonical.as_str()) {
            continue;
        }

        let occurrences: Vec<ConsistencyOccurrence> = issues_in_group
            .iter()
            .map(|i| ConsistencyOccurrence {
                offset: i.offset,
                line: i.line,
                col: i.col,
                found: i.found.clone(),
            })
            .collect();

        report.groups.push(ConsistencyGroup {
            term_group: english,
            preferred: canonical,
            occurrences,
        });
    }

    report
}

fn preferred_canonical_for_group(
    text: &str,
    issues_in_group: &[&Issue],
    glossary: &ProjectGlossary,
) -> Option<String> {
    // Prefer project glossary house terms when they also appear in the
    // document, but only when the rule already surfaced that term as a
    // canonical suggestion for this equivalence class. Short zh terms are
    // too collision-prone for edit-distance matching.
    if !glossary.preferred.is_empty() {
        for preferred in &glossary.preferred {
            if preferred.is_empty() {
                continue;
            }
            if !text.contains(preferred) {
                continue;
            }
            if glossary_preferred_matches_group(preferred, issues_in_group) {
                return Some(preferred.clone());
            }
        }
    }

    issues_in_group
        .iter()
        .find_map(|i| i.suggestions.first())
        .filter(|s| !s.is_empty())
        .cloned()
}

fn glossary_preferred_matches_group(preferred: &str, issues_in_group: &[&Issue]) -> bool {
    issues_in_group.iter().any(|issue| {
        issue
            .suggestions
            .iter()
            .any(|suggestion| suggestion == preferred)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cross_strait(offset: usize, found: &str, suggestion: &str, english: &str) -> Issue {
        let mut issue = Issue::new(
            offset,
            found.len(),
            found,
            vec![suggestion.into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = Some(Arc::from(english));
        issue
    }

    #[test]
    fn empty_when_no_mixed_usage() {
        let text = "我們只用線程實作。";
        let issues = vec![cross_strait(3, "線程", "執行緒", "thread")];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert!(report.is_empty(), "no canonical 執行緒 in text → no group");
    }

    #[test]
    fn fires_when_both_forms_present() {
        let text = "我們的線程很慢。執行緒設計需要重構。";
        let issues = vec![cross_strait(9, "線程", "執行緒", "thread")];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(report.groups.len(), 1);
        let group = &report.groups[0];
        assert_eq!(group.term_group, "thread");
        assert_eq!(group.preferred, "執行緒");
        assert_eq!(group.occurrences.len(), 1);
        assert_eq!(group.occurrences[0].found, "線程");
    }

    #[test]
    fn groups_multiple_calques_for_same_english() {
        // Both 線程 and an alternative mainland form 線程數 share english="thread".
        // (Simulated for the test — real ruleset may differ.)
        let text = "我們的線程很慢，線程數量太多。執行緒重構。";
        let issues = vec![
            cross_strait(9, "線程", "執行緒", "thread"),
            cross_strait(24, "線程", "執行緒", "thread"),
        ];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].occurrences.len(), 2);
    }

    #[test]
    fn ignores_info_severity_issues_tm_suppressed() {
        let text = "線程 ... 執行緒";
        let mut issue = cross_strait(0, "線程", "執行緒", "thread");
        issue.severity = Severity::Info;
        let report = compute_consistency_report(text, &[issue], &ProjectGlossary::default());
        assert!(report.is_empty(), "Info severity (TM-suppressed) skipped");
    }

    #[test]
    fn ignores_issues_without_english_anchor() {
        let text = "X ... Y";
        let mut issue = Issue::new(
            0,
            1,
            "X",
            vec!["Y".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = None;
        let report = compute_consistency_report(text, &[issue], &ProjectGlossary::default());
        assert!(report.is_empty());
    }

    #[test]
    fn separates_groups_by_english_anchor() {
        let text = "線程 執行緒 用戶 使用者";
        let issues = vec![
            cross_strait(0, "線程", "執行緒", "thread"),
            cross_strait(7, "用戶", "使用者", "user"),
        ];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(report.groups.len(), 2);
        let groups: Vec<&str> = report
            .groups
            .iter()
            .map(|g| g.term_group.as_str())
            .collect();
        assert!(groups.contains(&"thread"));
        assert!(groups.contains(&"user"));
    }

    #[test]
    fn prefers_glossary_preferred_form_over_default_suggestion() {
        // The rule lists two acceptable TW forms; the glossary picks
        // one as the project-canonical.  When both regional variants
        // appear in the document AND the glossary's choice is among
        // the rule's suggestions (matches_group), the consistency
        // report surfaces the glossary's choice instead of the rule's
        // first suggestion.
        let text = "我們的線程很慢。緒程設計需要重構。";
        let mut issue = Issue::new(
            9,
            6,
            "線程",
            vec!["執行緒".into(), "緒程".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = Some(Arc::from("thread"));
        let glossary = ProjectGlossary {
            preferred: vec!["緒程".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &[issue], &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].preferred, "緒程");
    }

    #[test]
    fn glossary_preferred_outside_suggestions_falls_back_to_rule_suggestion() {
        let text = "我們的線程很慢。緒程設計需要重構。執行緒也要重構。";
        let issues = vec![cross_strait(9, "線程", "執行緒", "thread")];
        let glossary = ProjectGlossary {
            preferred: vec!["緒程".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &issues, &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            report.groups[0].preferred, "執行緒",
            "preferred terms outside rule suggestions must not hijack the group"
        );
    }

    #[test]
    fn edit_distance_neighbor_does_not_hijack_group() {
        // Regression guard for short zh terms: sharing one edge
        // character with the calque is not enough to join the same
        // concept group.
        let text = "我們的線程很慢。執行緒設計需要重構。線性代數也出現。";
        let issues = vec![cross_strait(9, "線程", "執行緒", "thread")];
        let glossary = ProjectGlossary {
            preferred: vec!["線性".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &issues, &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            report.groups[0].preferred, "執行緒",
            "must fall back to rule suggestion, not pick unrelated 線性"
        );
    }

    #[test]
    fn glossary_preference_does_not_leak_across_groups() {
        let text = "線程與使用者都出現在文件裡。執行緒也出現。";
        let issues = vec![
            cross_strait(0, "線程", "執行緒", "thread"),
            cross_strait(3, "用戶", "使用者", "user"),
        ];
        let glossary = ProjectGlossary {
            preferred: vec!["使用者".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &issues, &glossary);
        let thread_group = report
            .groups
            .iter()
            .find(|group| group.term_group == "thread")
            .expect("thread group should exist");
        assert_eq!(thread_group.preferred, "執行緒");
    }
}
