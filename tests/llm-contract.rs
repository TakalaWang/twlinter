use zhtw_core::core::{CoreEngine, CoreOptions};
use zhtw_core::llm::{validate_context_response, ContextDecision, ContextRequest, ContextResponse};

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
                selected: issue.suggestions[0].clone(),
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
                selected: "模型自行發明的詞".to_string(),
            }],
        },
    );
    assert!(invalid.is_err());
}
