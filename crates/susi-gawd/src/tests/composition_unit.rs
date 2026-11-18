use super::*;

#[test]
fn swarm_pool_threads_reports_positive_worker_count() {
    assert!(swarm_pool_threads() >= 1);
}

#[test]
fn init_hooks_is_idempotent_across_repeated_invocations() {
    init_hooks();
    init_hooks();
    assert!(swarm_pool_threads() >= 1);
}

#[test]
fn ama_facade_sanitize_input_accepts_benign_prompt_and_wires_hooks() {
    let cleaned = ama::SusiMasterAgent::sanitize_input("check cluster health")
        .expect("benign input should pass");
    assert!(cleaned.contains("check cluster health"));
}
