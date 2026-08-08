#![cfg(feature = "discord")]

use twlinter::discord_policy::{protected_spans, rewrite_is_safe, rewrite_reply};
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
