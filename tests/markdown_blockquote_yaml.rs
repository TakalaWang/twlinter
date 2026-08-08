// 35.7 — Markdown blockquote exemption (opt-in) and YAML scalar
// quote preservation (always-on).
//
// Both behaviors are anchored to the ai-muninn.com calque blindspot
// sweep (2026-05): citation contexts produce ~50 false positives, and
// auto-converting `"` to `「`/`」` inside YAML frontmatter scalar values
// breaks downstream parsers.

use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::rules::loader::load_embedded_ruleset;
use zhtw_mcp::rules::ruleset::{IssueType, Profile};

fn scan_with(text: &str, exempt_blockquotes: bool) -> Vec<zhtw_mcp::rules::ruleset::Issue> {
    let ruleset = load_embedded_ruleset().expect("embedded ruleset loads");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    let cfg = Profile::Base
        .config()
        .with_exempt_blockquotes(exempt_blockquotes);
    scanner
        .scan_for_content_type_with_config(text, ContentType::Markdown, cfg)
        .issues
}

/// Blockquote exemption disabled (default): mainland-Chinese calques
/// inside `>`-prefixed citations are still flagged.
#[test]
fn blockquote_default_scans_citations() {
    let md = "正文裡的中文是 zh-TW。\n\n> 用戶輸入需要驗證。\n";
    let issues = scan_with(md, false);
    assert!(
        issues
            .iter()
            .any(|i| matches!(i.rule_type, IssueType::CrossStrait)
                && i.english.as_deref() == Some("user")),
        "default mode must scan blockquote citations; got {:?}",
        issues.iter().map(|i| i.found.as_str()).collect::<Vec<_>>()
    );
}

/// Blockquote exemption enabled: citations are silenced.
#[test]
fn blockquote_exempt_silences_citations() {
    let md = "正文裡的中文是 zh-TW。\n\n> 用戶輸入需要驗證。\n";
    let issues = scan_with(md, true);
    assert!(
        !issues
            .iter()
            .any(|i| matches!(i.rule_type, IssueType::CrossStrait)
                && i.english.as_deref() == Some("user")),
        "exempt mode must skip blockquote prose; got {:?}",
        issues.iter().map(|i| i.found.as_str()).collect::<Vec<_>>()
    );
}

/// Blockquote exemption only affects blockquotes — body prose still scans.
#[test]
fn blockquote_exempt_keeps_body_scanning() {
    let md = "用戶介面在正文裡也應該被覆蓋：用戶帳號要用使用者帳號。\n\n> 用戶輸入需要驗證。\n";
    let issues = scan_with(md, true);
    assert!(
        issues
            .iter()
            .any(|i| matches!(i.rule_type, IssueType::CrossStrait)),
        "body cross_strait hits must remain; got {:?}",
        issues.iter().map(|i| i.found.as_str()).collect::<Vec<_>>()
    );
}

/// Nested blockquotes (`> >`) and blockquotes inside list items must
/// also be exempt under the option.  Cmark events provide the correct
/// span tracking; a regex on `>` line prefixes would mishandle these.
#[test]
fn blockquote_exempt_handles_nested_and_listitem_quotes() {
    let md = "\
- 正文 list item.
  > 用戶帳號的處理流程。
  > > 巢狀引用的數據庫設計。
";
    let issues = scan_with(md, true);
    assert!(
        !issues
            .iter()
            .any(|i| matches!(i.rule_type, IssueType::CrossStrait)),
        "nested + list-item blockquotes must be exempt; got {:?}",
        issues.iter().map(|i| i.found.as_str()).collect::<Vec<_>>()
    );
}

/// YAML frontmatter ASCII `"` and `'` quote bytes are preserved (never
/// auto-converted to `「`/`」`).  Otherwise `"...".  to `「...」` would
/// break downstream YAML parsers.  This is always-on; no option needed.
#[test]
fn yaml_frontmatter_preserves_ascii_quotes() {
    let md = "\
---
title: \"用戶手冊\"
description: '使用者體驗指南'
---

正文。
";
    let issues = scan_with(md, false);

    // No punctuation issue should fire on the ASCII `\"` bytes inside
    // the frontmatter scalar values.
    let punct_quote_hits: Vec<_> = issues
        .iter()
        .filter(|i| {
            matches!(i.rule_type, IssueType::Punctuation) && (i.found == "\"" || i.found == "'")
        })
        .collect();

    assert!(
        punct_quote_hits.is_empty(),
        "YAML scalar quote bytes must not produce punctuation issues; got {:?}",
        punct_quote_hits
            .iter()
            .map(|i| (i.offset, i.found.as_str()))
            .collect::<Vec<_>>()
    );
}

/// Body ASCII `"` adjacent to CJK still converts to `「`/`」` (the
/// frontmatter exemption must not bleed into the body).
#[test]
fn body_ascii_quotes_still_convert_to_brackets() {
    let md = "\
---
title: \"用戶手冊\"
---

他說\"你好\"再見。
";
    let issues = scan_with(md, false);

    let body_quote_hits: Vec<_> = issues
        .iter()
        .filter(|i| matches!(i.rule_type, IssueType::Punctuation) && i.found == "\"")
        .collect();

    // The body has two `\"` bytes (open + close) adjacent to CJK; both
    // should fire the punctuation conversion suggestion.
    assert_eq!(
        body_quote_hits.len(),
        2,
        "body ASCII quotes adjacent to CJK must convert; got {:?}",
        body_quote_hits
            .iter()
            .map(|i| (i.offset, i.found.as_str()))
            .collect::<Vec<_>>()
    );
}
