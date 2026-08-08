// 35.10 high-frequency calque coverage audit (real-world corpus).
//
// Anchored to ai-muninn.com calque blindspot sweep (2026-05).  Pins
// the linter behavior on the 14 mainland-Chinese terms reported as
// missed in published zh-TW articles, plus the boundary collocations
// that must NOT fire.

use std::path::{Path, PathBuf};

use zhtw_mcp::engine::scan::{ContentType, Scanner};
use zhtw_mcp::rules::loader::load_embedded_ruleset;
use zhtw_mcp::rules::ruleset::{Issue, IssueType, Profile};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("realworld_calques.md")
}

fn scan_text(text: &str) -> Vec<Issue> {
    let ruleset = load_embedded_ruleset().expect("embedded ruleset loads");
    let scanner = Scanner::new(ruleset.spelling_rules, ruleset.case_rules);
    scanner
        .scan_for_content_type(text, ContentType::Markdown, Profile::Base)
        .issues
}

fn issues_on_line(issues: &[Issue], line: usize) -> Vec<&Issue> {
    issues.iter().filter(|i| i.line == line).collect()
}

/// True when the issue is a CrossStrait hit whose english field equals or
/// contains the concept anchor.  The compound rule may use a richer
/// english (e.g. `數據庫` → "database", `用戶介面` → "user interface"),
/// so we accept substring containment for concept-level coverage.
fn english_matches(english: Option<&str>, concept: &str) -> bool {
    match english {
        Some(e) => e == concept || e.contains(concept),
        None => false,
    }
}

fn assert_term_fires(issues: &[Issue], line: usize, expected_english: &str) {
    let line_issues = issues_on_line(issues, line);
    let hit = line_issues.iter().any(|i| {
        matches!(i.rule_type, IssueType::CrossStrait)
            && english_matches(i.english.as_deref(), expected_english)
    });
    assert!(
        hit,
        "line {line}: expected a CrossStrait issue with english containing {expected_english:?}, got: {:?}",
        line_issues
            .iter()
            .map(|i| (i.found.as_str(), i.english.as_deref(), i.rule_type))
            .collect::<Vec<_>>()
    );
}

fn assert_term_silent(issues: &[Issue], line: usize, forbidden_from: &str) {
    // Match by containment, not equality: a regression where the
    // scanner emits a longer phrase that contains the forbidden bare
    // term (e.g. `好消息` slipping through as a phrase-level hit
    // covering the inner `消息` calque) would still violate the
    // collocation invariant the test guards.  Equality would let
    // those slip past.
    let line_issues = issues_on_line(issues, line);
    let hit = line_issues.iter().any(|i| i.found.contains(forbidden_from));
    assert!(
        !hit,
        "line {line}: expected NO issue containing {forbidden_from:?}, got: {:?}",
        line_issues
            .iter()
            .map(|i| (i.found.as_str(), i.rule_type))
            .collect::<Vec<_>>()
    );
}

/// Phase 1 — coverage audit.  Each of the 14 audited terms (plus a few
/// closely related compounds) must produce a non-zero hit when used in
/// realistic prose under the `base` profile.
#[test]
fn phase1_high_frequency_terms_fire() {
    let body = std::fs::read_to_string(fixture_path()).expect("fixture exists");
    let issues = scan_text(&body);

    // Each (line, expected term) anchors a section in the fixture.
    // Each anchor is (line, expected english field on a CrossStrait issue
    // covering some span of that line).  Concept-level coverage — the
    // longer compound rule is allowed to win over the bare term as long
    // as english anchors match.
    let anchors: &[(usize, &str)] = &[
        (10, "data"),
        (11, "data"),
        (12, "data"),
        (13, "data"),
        (14, "user"),
        (15, "user"),
        (16, "connect"),
        (17, "connect"),
        (18, "algorithm"),
        (19, "concurrency"),
        (20, "message"),
        (21, "memory"),
        (22, "thread"),
        (23, "programmer"),
        (24, "software"),
        (25, "hardware"),
        (26, "network"),
        (27, "video"),
    ];

    for (line, english) in anchors {
        assert_term_fires(&issues, *line, english);
    }
}

/// Phase 2 — `元數據` parent rule must keep firing exactly once and
/// surface "metadata" verbatim as the suggestion (no `元資料` /
/// `詮釋資料` / `後設資料` translation target).
#[test]
fn phase2_metadata_parent_rule_keeps_firing_with_english_anchor() {
    let body = std::fs::read_to_string(fixture_path()).expect("fixture exists");
    let issues = scan_text(&body);

    let line_31 = issues_on_line(&issues, 31);
    let metadata_hits: Vec<_> = line_31.iter().filter(|i| i.found == "元數據").collect();
    assert_eq!(
        metadata_hits.len(),
        1,
        "line 31: expected exactly one 元數據 hit (parent), got {}: {:?}",
        metadata_hits.len(),
        line_31.iter().map(|i| i.found.as_str()).collect::<Vec<_>>()
    );

    let parent = metadata_hits[0];
    let english = parent.english.as_deref().unwrap_or("");
    assert_eq!(
        english, "metadata",
        "元數據 rule must surface english anchor 'metadata' verbatim"
    );

    // The 元數據 rule uses `to: []`, so `effective_suggestions` falls
    // back to the english anchor.  The user-visible suggestion must be
    // exactly "metadata" — neither the rejected mainland form `元資料`
    // nor the acceptable-but-not-preferred coinages `詮釋資料` /
    // `後設資料`.  Asserting the suggestion list literally catches a
    // regression where someone adds `to: ["後設資料"]` thinking it's
    // a friendlier translation; the engine would surface that instead
    // of "metadata", silently violating the gate.
    assert_eq!(
        parent.suggestions.as_ref(),
        ["metadata".to_string()].as_slice(),
        "元數據 must surface exactly [\"metadata\"]; got {:?}",
        parent.suggestions,
    );

    // Phase 2 invariant: the inner 數據 hit must NOT double-fire on the
    // same span — overlap resolution + the parent rule should yield
    // exactly one issue covering the full 元數據 span.
    let inner_data: Vec<_> = line_31
        .iter()
        .filter(|i| {
            i.found == "數據"
                && (i.offset..i.offset + i.length) != (parent.offset..parent.offset + parent.length)
        })
        .collect();
    assert!(
        inner_data.is_empty(),
        "inner 數據 must not double-fire inside 元數據; got: {inner_data:?}"
    );
}

/// Three-tier metadata policy: writer prose containing `元資料` (the
/// rejected mainland-style Sinification) must trigger the symmetric
/// rule and surface "metadata" as the suggestion, mirroring the
/// `元數據` rule.  Without the sibling rule, `元資料` would slip
/// through unflagged.  The acceptable forms `詮釋資料` and `後設資料`
/// are validated separately by the boundary tests below.
#[test]
fn phase2_metadata_rejected_form_is_flagged_symmetrically() {
    let issues = scan_text("文件的元資料描述了結構與來源。");
    let yuanziliao_hits: Vec<_> = issues.iter().filter(|i| i.found == "元資料").collect();
    assert_eq!(
        yuanziliao_hits.len(),
        1,
        "writer prose containing 元資料 must trigger its own rule; got {yuanziliao_hits:?}"
    );
    let hit = yuanziliao_hits[0];
    assert_eq!(hit.english.as_deref(), Some("metadata"));
    assert_eq!(
        hit.suggestions.as_ref(),
        ["metadata".to_string()].as_slice()
    );
}

/// Three-tier metadata policy: the acceptable zh-TW alternatives
/// `詮釋資料` and `後設資料` (NAER terminology) must NOT be flagged
/// in writer prose — they are valid zh-TW forms even though
/// "metadata" is the preferred surface form.
#[test]
fn phase2_metadata_acceptable_forms_pass_through() {
    let issues = scan_text("詮釋資料與後設資料皆為合法的中文翻譯。");
    assert!(
        !issues
            .iter()
            .any(|i| i.found == "詮釋資料" || i.found == "後設資料"),
        "acceptable zh-TW alternatives must not fire; got {:?}",
        issues
            .iter()
            .map(|i| (i.found.as_str(), i.english.as_deref()))
            .collect::<Vec<_>>()
    );
}

/// Phase 2 — `算法` must not fire inside `演算法` (canonical zh-TW form).
#[test]
fn phase2_algorithm_silent_inside_canonical_form() {
    let body = std::fs::read_to_string(fixture_path()).expect("fixture exists");
    let issues = scan_text(&body);

    for line in [35, 36] {
        assert_term_silent(&issues, line, "算法");
    }
}

/// Phase 3 — `消息` rule must respect legitimate zh-TW collocations.
#[test]
fn phase3_message_silent_in_legitimate_collocations() {
    let body = std::fs::read_to_string(fixture_path()).expect("fixture exists");
    let issues = scan_text(&body);

    for line in [40, 41, 42] {
        assert_term_silent(&issues, line, "消息");
    }
}

/// Phase 3 — bare `文件` rule is intentionally disabled
/// (assets/ruleset.json `"disabled": true`).  The audit must NOT
/// accidentally re-enable it.  This test catches a regression where
/// someone toggles the flag without realizing the bare-word
/// ambiguity rationale.
#[test]
fn phase3_bare_file_rule_remains_disabled() {
    let ruleset = load_embedded_ruleset().expect("embedded ruleset loads");
    let bare_file_rule = ruleset
        .spelling_rules
        .iter()
        .find(|r| r.from == "文件")
        .expect("文件 rule exists");
    assert!(
        bare_file_rule.disabled,
        "文件 cross_strait rule must remain disabled — bare-word ambiguity"
    );
}

/// Boundary — pure zh-TW forms must produce zero hits on these terms.
#[test]
fn boundary_pure_tw_forms_silent() {
    let body = std::fs::read_to_string(fixture_path()).expect("fixture exists");
    let issues = scan_text(&body);

    let forms: &[(usize, &str)] = &[(46, "用戶"), (47, "連接"), (48, "消息")];

    for (line, term) in forms {
        assert_term_silent(&issues, *line, term);
    }
}
