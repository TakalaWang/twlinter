//! Discord-specific policy and output formatting.

use regex::Regex;

use crate::core::CoreResult;
use crate::llm::ProtectedSpan;

pub const REWRITE_COMMAND: &str = "/tw-rewrite ";

pub fn automatic_reply(result: &CoreResult) -> Option<String> {
    if !result.changed || result.text.is_empty() {
        return None;
    }
    let text = truncate_for_discord(&result.text);
    Some(format!("建議改成：\n{text}"))
}

pub fn rewrite_request(
    original: &str,
    draft: &str,
    issues: &crate::core::CoreAnalysis,
) -> crate::llm::RewriteRequest {
    crate::llm::RewriteRequest {
        locale: "zh-TW".to_string(),
        original_text: original.to_string(),
        deterministic_draft: draft.to_string(),
        issues: issues
            .issues
            .iter()
            .map(|issue| crate::llm::ContextIssue {
                offset: issue.offset,
                found: issue.found.clone(),
                suggestions: issue.suggestions.to_vec(),
                context: issue.context.as_deref().map(str::to_string),
                english: issue.english.as_deref().map(str::to_string),
                context_clues: issue.context_clues.as_deref().unwrap_or_default().to_vec(),
            })
            .collect(),
        protected_spans: protected_spans(original),
    }
}

pub fn protected_spans(text: &str) -> Vec<ProtectedSpan> {
    let patterns = [
        ("url", r"https?://[^\s>]+"),
        ("mention", r"<[@#!&][0-9]+>"),
        (
            "code",
            r"\x60\x60\x60[\s\S]*?\x60\x60\x60|\x60[^\x60\n]+\x60",
        ),
    ];
    patterns
        .into_iter()
        .flat_map(|(kind, pattern)| {
            let regex = Regex::new(pattern).expect("protected span regex is valid");
            regex
                .find_iter(text)
                .map(move |m| ProtectedSpan {
                    kind: kind.to_string(),
                    text: m.as_str().to_string(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn rewrite_is_safe(request: &crate::llm::RewriteRequest, rewritten: &str) -> bool {
    request
        .protected_spans
        .iter()
        .all(|span| rewritten.contains(&span.text))
}

fn truncate_for_discord(text: &str) -> String {
    const LIMIT: usize = 1900;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let truncated: String = text.chars().take(LIMIT - 1).collect();
    format!("{truncated}…")
}
