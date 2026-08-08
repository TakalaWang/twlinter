use zhtw_core::core::{CoreEngine, CoreOptions, IssueDecision};

#[test]
fn core_reuses_upstream_rules_and_applies_deterministic_fixes() {
    let engine = CoreEngine::from_embedded(CoreOptions::default()).unwrap();
    let analysis = engine.analyze("這個軟件會把數據存到內存。");
    let result = engine.apply(&analysis, &[]).unwrap();

    assert_eq!(result.text, "這個軟體會把資料存到記憶體。");
    assert!(result.changed);
    assert!(result.issues.iter().all(|issue| issue.found != "軟件"));
}

#[test]
fn core_accepts_only_existing_candidates_from_external_decisions() {
    let engine = CoreEngine::from_embedded(CoreOptions::default()).unwrap();
    let analysis = engine.analyze("這個程序會編譯原始碼。");
    let issue = analysis
        .issues
        .iter()
        .find(|issue| issue.found == "程序")
        .unwrap();
    let selected = issue
        .suggestions
        .iter()
        .find(|suggestion| suggestion.as_str() == "程式")
        .cloned()
        .unwrap();

    let result = engine
        .apply(
            &analysis,
            &[IssueDecision {
                offset: issue.offset,
                found: issue.found.clone(),
                selected,
            }],
        )
        .unwrap();

    assert!(result.text.contains("程式"));
}

#[test]
fn core_rejects_an_external_candidate_not_present_in_ruleset() {
    let engine = CoreEngine::from_embedded(CoreOptions::default()).unwrap();
    let analysis = engine.analyze("這個程序會編譯原始碼。");
    let issue = analysis
        .issues
        .iter()
        .find(|issue| issue.found == "程序")
        .unwrap();

    let error = engine
        .apply(
            &analysis,
            &[IssueDecision {
                offset: issue.offset,
                found: issue.found.clone(),
                selected: "不在規則中的詞".to_string(),
            }],
        )
        .unwrap_err();

    assert!(error.to_string().contains("not an allowed suggestion"));
}
