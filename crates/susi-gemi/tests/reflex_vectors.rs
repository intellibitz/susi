use susi_gemi::engines::alpha::SusiAlphaModel;
use susi_gemi::susi_core::AgentProfile;

#[test]
fn exact_spellings_win_and_shared_stems_retain_their_order() {
    let file = SusiAlphaModel::reflex_features("file");
    let files = SusiAlphaModel::reflex_features("files");
    assert_ne!(file, files);
    assert_eq!(SusiAlphaModel::reflex_features("filing"), files);
    assert_eq!(
        SusiAlphaModel::reflex_features("displayed"),
        SusiAlphaModel::reflex_features("read")
    );
}

#[test]
fn first_live_agent_owns_a_shared_canonical_anchor() -> susi_gemi::susi_error::EaiResult<()> {
    let agents = ["λ", "second"].map(|name| AgentProfile {
        name: name.to_owned(),
        description: String::new(),
        categories: Vec::new(),
        semantic_anchors: vec!["status".to_owned()],
        base_rank: 0.0,
        is_core: false,
    });
    let actual = SusiAlphaModel::semantic_centroid_projection("STATUS", Some(&agents))?;
    let mut expected = vec![0.0; 128];
    expected[82] = 1.0;
    assert_eq!(actual, expected);
    Ok(())
}
