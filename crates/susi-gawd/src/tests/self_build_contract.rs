//! Core's self-build brief keeps its verified local production promotion rules
//! (VC-201-018). The promotion engine itself moved to Susi Commercial Apps with the
//! RSI eval cluster; the contract text it is held to stays in core.

#[test]
fn vc_201_018_self_build_requires_verified_local_production_promotion() {
    let brief = crate::susi_core::self_build::BRIEF;
    for requirement in [
        "Debug and optimized candidates stay separate from the installed production",
        "Never copy an unqualified candidate over production",
        "Production promotion requires exact",
        "source/lock/toolchain/target evidence",
        "executable smoke tests",
        "artifact digest",
        "atomic replacement",
        "health checks",
        "tested rollback to the previous verified",
    ] {
        assert!(
            brief.contains(requirement),
            "delegated production contract lost: {requirement}"
        );
    }
}
