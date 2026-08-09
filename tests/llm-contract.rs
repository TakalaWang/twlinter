use twlinter::core::{CoreEngine, CoreOptions};
use twlinter::llm::{validate_context_response, ContextDecision, ContextRequest, ContextResponse};

#[test]
fn context_response_is_limited_to_ruleset_candidates() {
    let engine = CoreEngine::from_embedded(CoreOptions::default()).unwrap();
    let analysis = engine.analyze("位操作使用 mask 和 shift。");
    let request = ContextRequest::from_analysis("位操作使用 mask 和 shift。", &analysis);
    let issue = request.issues.first().unwrap();

    let valid = validate_context_response(
        &request,
        ContextResponse {
            decisions: vec![ContextDecision {
                offset: issue.offset,
                found: issue.found.clone(),
                selected: Some(issue.suggestions[0].clone()),
            }],
        },
    )
    .unwrap();
    assert_eq!(valid.len(), 1);

    let invalid = validate_context_response(
        &request,
        ContextResponse {
            decisions: vec![ContextDecision {
                offset: issue.offset,
                found: issue.found.clone(),
                selected: Some("模型自行發明的詞".to_string()),
            }],
        },
    );
    assert!(invalid.is_err());
}

#[test]
fn context_request_carries_ruleset_conditions_and_allows_keep() {
    let engine = CoreEngine::from_embedded(CoreOptions::default()).unwrap();
    let text = "這段程式碼會呼叫函數。";
    let analysis = engine.analyze(text);
    let request = ContextRequest::from_analysis(text, &analysis);
    let issue = request
        .issues
        .iter()
        .find(|issue| issue.found == "函數")
        .unwrap();

    assert!(!issue.context_clues.is_empty());
    assert!(!issue.negative_context_clues.is_empty());
    assert!(!issue.exceptions.is_empty());
    let decisions = validate_context_response(
        &request,
        ContextResponse {
            decisions: vec![ContextDecision {
                offset: issue.offset,
                found: issue.found.clone(),
                selected: None,
            }],
        },
    )
    .unwrap();
    assert_eq!(decisions[0].selected, None);
}
