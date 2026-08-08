//! Shared human/CLI issue reporting helpers copied from the upstream output path.

use crate::engine::scan::is_spaced_acronym_issue;
use crate::rules::ruleset::{Issue, IssueType};

fn build_explanation(issue: &Issue) -> Option<String> {
    let mut parts = Vec::new();

    match issue.rule_type {
        IssueType::CrossStrait => {
            if let Some(eng) = &issue.english {
                parts.push(format!(
                    "'{}' is a mainland Chinese term for '{}'; Taiwan uses '{}'.",
                    issue.found,
                    eng,
                    issue.suggestions.join(" / "),
                ));
            } else if !issue.suggestions.is_empty() {
                parts.push(format!(
                    "'{}' is a mainland Chinese expression; Taiwan standard: {}.",
                    issue.found,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::Confusable => {
            if let Some(eng) = &issue.english {
                parts.push(format!(
                    "'{}' is ambiguous across the strait. English anchor: '{}'. Taiwan form: {}.",
                    issue.found,
                    eng,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::PoliticalColoring => parts.push(format!(
            "'{}' carries mainland political connotations; prefer {}.",
            issue.found,
            issue.suggestions.join(" / "),
        )),
        IssueType::Variant => parts.push(format!(
            "'{}' is a non-standard character variant; MoE standard form: {}.",
            issue.found,
            issue.suggestions.join(" / "),
        )),
        IssueType::Typo => parts.push(format!(
            "'{}' appears to be a typo; suggested: {}.",
            issue.found,
            issue.suggestions.join(" / "),
        )),
        IssueType::Case => parts.push(format!(
            "'{}' has incorrect casing; standard form: {}.",
            issue.found,
            issue.suggestions.join(" / "),
        )),
        IssueType::Punctuation => parts.push(format!(
            "'{}' should use the full-width equivalent {} in CJK prose per MoE standards.",
            issue.found,
            issue.suggestions.join(" / "),
        )),
        IssueType::Grammar => {
            if let Some(ctx) = &issue.context {
                parts.push(format!(
                    "'{}' — {}. Suggested: {}.",
                    issue.found,
                    ctx,
                    issue.suggestions.join(" / "),
                ));
            } else {
                parts.push(format!(
                    "'{}' is a grammatical issue; suggested: {}.",
                    issue.found,
                    issue.suggestions.join(" / "),
                ));
            }
        }
        IssueType::AiStyle => {
            if let Some(ctx) = &issue.context {
                parts.push(format!("'{}' — {}.", issue.found, ctx));
            }
            if !issue.suggestions.is_empty() {
                parts.push(format!("Suggested: {}.", issue.suggestions.join(" / ")));
            } else {
                parts.push("Consider removing or rephrasing.".to_string());
            }
        }
        IssueType::Translationese => {
            if let Some(ctx) = &issue.context {
                parts.push(format!("'{}' — {}.", issue.found, ctx));
            }
            if !issue.suggestions.is_empty() {
                parts.push(format!(
                    "Suggested rewrite: {}.",
                    issue.suggestions.join(" / ")
                ));
            } else {
                parts.push(
                    "Translationese / 歐化 pattern; consider an idiomatic zh-TW rewrite."
                        .to_string(),
                );
            }
        }
        IssueType::Repetition => {
            if is_spaced_acronym_issue(issue) {
                parts.push(format!(
                    "'{}' should be written as '{}'; the spacing looks like a transcription artifact.",
                    issue.found,
                    issue.suggestions[0],
                ));
            } else {
                parts.push(format!(
                    "'{}' is a consecutive duplicate; remove the repetition.",
                    issue.found,
                ));
            }
        }
    }

    if !matches!(
        issue.rule_type,
        IssueType::Grammar | IssueType::AiStyle | IssueType::Translationese
    ) {
        if let Some(ctx) = &issue.context {
            parts.push(format!("Context: {ctx}"));
        }
    }

    (!parts.is_empty()).then(|| parts.join(" "))
}

pub struct IssueGroup {
    pub suggestions: Vec<String>,
    pub count: usize,
    pub locs: Vec<(usize, usize)>,
    pub explanation: Option<String>,
}

pub type IssueGroupKey<'a> = (&'a str, &'a str, String, &'a str);

pub fn group_issues<'a>(
    issues: &'a [Issue],
    explain: bool,
) -> std::collections::BTreeMap<IssueGroupKey<'a>, IssueGroup> {
    let mut groups = std::collections::BTreeMap::new();
    for issue in issues {
        let key = (
            issue.found.as_str(),
            issue.rule_type.name(),
            issue.suggestions.join("|"),
            issue.severity.name(),
        );
        let entry = groups.entry(key).or_insert_with(|| IssueGroup {
            suggestions: issue.suggestions.to_vec(),
            count: 0,
            locs: Vec::new(),
            explanation: explain.then(|| build_explanation(issue)).flatten(),
        });
        entry.count += 1;
        entry.locs.push((issue.line, issue.col));
    }
    groups
}

pub fn escape_tsv_field(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes()
        .any(|b| b == b'\\' || b == b'\t' || b == b'\n' || b == b'\r')
    {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

pub fn shorten_severity(sev: &str) -> &str {
    match sev {
        "error" => "E",
        "warning" => "W",
        "info" => "I",
        _ => sev,
    }
}

pub fn shorten_type(rt: &str) -> &str {
    match rt {
        "political_coloring" => "pol",
        "cross_strait" => "cs",
        "typo" => "typo",
        "confusable" => "cf",
        "case" => "case",
        "punctuation" => "punc",
        "variant" => "v",
        "grammar" => "gram",
        _ => rt,
    }
}

pub fn compress_locations(locs: &[(usize, usize)]) -> String {
    use std::fmt::Write;
    if locs.is_empty() {
        return String::new();
    }
    if locs.len() == 1 {
        return format!("{}:{}", locs[0].0, locs[0].1);
    }
    let first_col = locs[0].1;
    if locs.iter().all(|(_, c)| *c == first_col) {
        let mut result = String::new();
        for (i, (line, _)) in locs.iter().enumerate() {
            if i > 0 {
                result.push(',');
            }
            let _ = write!(result, "{line}");
        }
        let _ = write!(result, ":{first_col}");
        result
    } else {
        locs.iter()
            .map(|(line, col)| format!("{line}:{col}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}
