use std::path::PathBuf;

use traceable_search::{
    ClarificationStatus, ModelAccessConfig, ResearchInfrastructureConfig, TracePolicy,
    TraceableResearchRuntime,
};

#[tokio::test]
#[ignore = "requires live Brave Search API, public Web access, and model services"]
async fn live_research_pipeline() {
    let research_data_dir = PathBuf::from(
        std::env::var_os("TRACEABLE_SEARCH_DATA_DIR")
            .expect("TRACEABLE_SEARCH_DATA_DIR must isolate live E2E output"),
    );
    assert!(
        research_data_dir.is_absolute(),
        "E2E data directory must be absolute"
    );

    let runtime = TraceableResearchRuntime::new(
        ResearchInfrastructureConfig::from_env().expect("valid live E2E config"),
    );
    let model_access = ModelAccessConfig::from_env().expect("valid live model config");
    let mut clarification = runtime
        .start_single_turn_conversation(
            "Rust 1.85.0 的发布日期和主要稳定特性是什么？请引用官方来源。",
            &model_access,
        )
        .await
        .expect("start intake");

    while clarification.status == ClarificationStatus::AwaitingUserMessage {
        clarification = runtime
            .submit_dialogue_message(
                &clarification.clarification_id,
                clarification.revision,
                "聚焦 Rust 1.85.0 正式版，以 Rust 官方发布说明为准；简洁概括主要稳定特性。",
                &model_access,
            )
            .await
            .expect("reply to intake");
    }

    assert_eq!(
        clarification.status,
        ClarificationStatus::ResearchReady,
        "clarification failed: {:?}",
        clarification.failure
    );
    let prepared = runtime
        .prepare_research_run(
            &clarification.clarification_id,
            TracePolicy {
                rounds: 3,
                input_budget: 200_000,
                max_snapshots: 30,
            },
        )
        .await
        .expect("prepare run");
    let answer = runtime
        .execute_prepared_research(prepared, &model_access)
        .await
        .expect("run research");

    assert!(!answer.answer.trim().is_empty());
    assert!(!answer.claims.is_empty());
    assert!(answer.claims.iter().all(|claim| !claim.sources.is_empty()));
}
