#![cfg(feature = "discord")]

use twlinter::core::CoreResult;
use twlinter::discord_policy::{
    automatic_replacement, protected_spans, rewrite_is_safe, rewrite_replacement, rewrite_reply,
};
use twlinter::llm::RewriteRequest;

#[test]
fn protected_discord_content_must_survive_rewrite() {
    let original = "請看 https://example.com <@123> \x60code\x60";
    let spans = protected_spans(original);
    assert_eq!(spans.len(), 3);

    let request = RewriteRequest {
        locale: "zh-TW".to_string(),
        original_text: original.to_string(),
        deterministic_draft: original.to_string(),
        issues: Vec::new(),
        protected_spans: spans,
    };
    assert!(rewrite_is_safe(&request, original));
    assert!(!rewrite_is_safe(&request, "請看 https://example.com"));
}

#[test]
fn rewrite_reply_is_bounded_for_discord() {
    let reply = rewrite_reply(&"字".repeat(2_000));

    assert_eq!(reply, "改寫內容超過 Discord 長度限制，未自動回覆。");
}

#[test]
fn rewrite_reply_uses_the_requested_prefix() {
    assert_eq!(
        rewrite_reply("這是一句話"),
        "You may want to say:\n這是一句話"
    );
}

#[test]
fn replacement_is_raw_and_bounded() {
    assert_eq!(
        rewrite_replacement("這是一句話").as_deref(),
        Some("這是一句話")
    );
    assert!(rewrite_replacement(&"字".repeat(2_000)).is_none());
}

#[test]
fn automatic_replacement_rejects_empty_output() {
    let result = CoreResult {
        text: String::new(),
        issues: Vec::new(),
        applied_fixes: Vec::new(),
        input_was_simplified: false,
        changed: true,
    };
    assert!(automatic_replacement(&result).is_none());
}
