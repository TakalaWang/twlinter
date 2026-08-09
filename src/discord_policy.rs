//! Discord-specific policy and output formatting.

use regex::Regex;

use crate::core::CoreResult;
use crate::llm::ProtectedSpan;

const DISCORD_BODY_LIMIT: usize = 1_900;
const OVERSIZE_REPLY: &str = "改寫內容超過 Discord 長度限制，未自動回覆。";
const REPLY_PREFIX: &str = "You may want to say:";

pub fn automatic_reply(result: &CoreResult) -> Option<String> {
    if !result.changed || result.text.is_empty() {
        return None;
    }
    if result.text.chars().count() > DISCORD_BODY_LIMIT {
        return Some(OVERSIZE_REPLY.to_string());
    }
    Some(format!("{REPLY_PREFIX}\n{}", result.text))
}

pub fn automatic_replacement(result: &CoreResult) -> Option<String> {
    bounded_replacement(result.changed.then_some(result.text.as_str())?)
}

pub fn rewrite_reply(text: &str) -> String {
    if text.chars().count() > DISCORD_BODY_LIMIT {
        return OVERSIZE_REPLY.to_string();
    }
    format!("{REPLY_PREFIX}\n{text}")
}

pub fn rewrite_replacement(text: &str) -> Option<String> {
    bounded_replacement((!text.is_empty()).then_some(text)?)
}

fn bounded_replacement(text: &str) -> Option<String> {
    (text.chars().count() <= DISCORD_BODY_LIMIT).then(|| text.to_string())
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
                negative_context_clues: issue
                    .negative_context_clues
                    .as_deref()
                    .unwrap_or_default()
                    .to_vec(),
                positional_clues: issue
                    .positional_clues
                    .as_deref()
                    .unwrap_or_default()
                    .to_vec(),
                exceptions: issue.exceptions.as_deref().unwrap_or_default().to_vec(),
                tags: issue.tags.as_deref().unwrap_or_default().to_vec(),
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
