//! Mastery verification for VC-201-013: autonomous patches fenced into
//! isolated workspaces that cannot write the installed release, user
//! files, or another experiment's workspace.

use crate::patch_cycle::{apply_patch_cycle, FilePatch, PatchRequest};
use crate::patch_fence::PatchFence;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_root(tag: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("susi-fence-mastery-{tag}-{nanos}"))
}

fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = tmp_root(tag);
    let ws = root.join("repo");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "old\n").unwrap();
    (root, ws)
}

fn request(patch_id: Option<&str>, test_cmd: &str) -> PatchRequest {
    PatchRequest {
        files: vec![FilePatch {
            path: "a.txt".into(),
            old: "old\n".into(),
            new: "new\n".into(),
        }],
        test_command: Some(test_cmd.into()),
        auto_apply: true,
        description: "mastery".into(),
        isolate: patch_id.map(str::to_string),
    }
}

/// Fixed: `patch_id` is validated as a single path segment before it is
/// ever joined onto the fence root, so `"../escape"` — which carries a
/// separator — is refused outright instead of building a workspace that
/// resolves outside `root/patches`.
#[test]
fn vc_201_013_mastery_patch_id_escapes_the_fence_root() {
    let root = tmp_root("esc");
    let err = PatchFence::isolate(&root, "../escape")
        .expect_err("a patch id containing a path separator must be refused");
    assert!(err.contains("path segment") || err.contains('.'), "{err}");
    assert!(
        !root.join("escape").exists(),
        "no workspace should have been created outside the fence root"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Fixed: `is_inside_fence` resolves `.`/`..` components lexically before
/// comparing, so `workspace/../outside.patch` — which resolves outside the
/// workspace — now correctly reports outside instead of matching on a raw
/// `starts_with` of the unresolved path.
#[test]
fn vc_201_013_mastery_dotdot_inside_reports_inside() {
    let root = tmp_root("dd");
    let fence = PatchFence::isolate(&root, "p1").unwrap();
    let sneaky = fence.workspace.join("..").join("outside.patch");
    assert!(
        !fence.is_inside_fence(&sneaky),
        "a path that resolves OUTSIDE the workspace must not report inside"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Fixed: `apply_in_isolation` now takes the target path and the content
/// being applied, validates the resolved target stays inside the fence,
/// and actually writes it — a target that climbs out with `..` is refused
/// rather than silently accepted with nothing checked.
#[test]
fn vc_201_013_mastery_apply_checks_nothing() {
    let root = tmp_root("ap");
    let mut fence = PatchFence::isolate(&root, "p1").unwrap();
    fence.ensure_isolated().unwrap();

    // A well-behaved target is actually written inside the fence.
    fence.apply_in_isolation("diff.patch", b"hello").unwrap();
    assert!(fence.applied);
    assert_eq!(
        std::fs::read(fence.workspace.join("diff.patch")).unwrap(),
        b"hello"
    );

    // A target that climbs out of the workspace is refused, and nothing
    // is written outside the fence.
    let mut escapee = PatchFence::isolate(&root, "p2").unwrap();
    escapee.ensure_isolated().unwrap();
    let outside = root.join("escaped.patch");
    assert!(escapee
        .apply_in_isolation("../escaped.patch", b"evil")
        .is_err());
    assert!(!outside.exists(), "escape target must never be written");

    let _ = std::fs::remove_dir_all(&root);
}

/// Fixed: two fences built from the same `patch_id` now get distinct
/// workspaces (a per-call nonce is mixed in), and a `patch_id` that tries
/// to walk onto a sibling experiment's directory (`"x/../victim"`) is
/// refused outright because it carries a path separator.
#[test]
fn vc_201_013_mastery_same_id_shares_workspace() {
    let root = tmp_root("sh");
    let a = PatchFence::isolate(&root, "shared").unwrap();
    let b = PatchFence::isolate(&root, "shared").unwrap();
    assert_ne!(
        a.workspace, b.workspace,
        "two fences for the same patch_id must not share one workspace"
    );

    let hostile = PatchFence::isolate(&root, "x/../victim");
    assert!(
        hostile.is_err(),
        "a patch id with a path separator must be refused, not resolved onto a sibling"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// What holds: a well-formed patch_id nests under root/patches, the
/// workspace is created, and a missing workspace refuses apply.
#[test]
fn vc_201_013_mastery_basics_hold() {
    let root = tmp_root("ok");
    let mut fence = PatchFence::isolate(&root, "p1").unwrap();
    assert!(fence.workspace.starts_with(root.join("patches")));
    assert!(fence.apply_in_isolation("diff.patch", b"x").is_err());
    fence.ensure_isolated().unwrap();
    fence.apply_in_isolation("diff.patch", b"x").unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// Production wiring: the real `apply_patch_cycle` runs the candidate
/// inside a fenced workspace under `<root>/patches/` — its name is
/// reported back — and a passing run promotes the patched contents into
/// the real tree. A byproduct the test command writes lands in the
/// fence only, proving the command's cwd was the candidate checkout and
/// not the live workspace.
#[test]
fn vc_201_013_mastery_patch_cycle_runs_fenced_and_promotes_on_pass() {
    let (root, ws) = fixture("prod");
    let outcome = apply_patch_cycle(
        &ws,
        &request(
            Some("exp1"),
            "test \"$(cat a.txt)\" = new && echo x > byproduct.txt",
        ),
        "autonomous",
    )
    .unwrap();

    assert!(outcome.test_passed, "fenced run must pass: {outcome:?}");
    let iso = outcome
        .isolated_workspace
        .as_deref()
        .expect("an isolated run must name its fenced workspace");
    assert!(
        std::path::Path::new(iso).starts_with(root.join("patches")),
        "fence must live under root/patches, got {iso}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).unwrap(),
        "new\n",
        "a passing fenced run promotes the patch into the real tree"
    );
    assert!(
        !ws.join("byproduct.txt").exists(),
        "a test-command byproduct must land in the fence, never the live tree"
    );
    assert!(
        std::path::Path::new(iso).join("byproduct.txt").is_file(),
        "the byproduct is evidence inside the fenced workspace"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Production wiring: a candidate whose verify step fails leaves the
/// live workspace byte-identical — the apply happened only inside the
/// fence — while the fenced workspace is retained as evidence.
#[test]
fn vc_201_013_mastery_failed_candidate_never_touches_the_live_tree() {
    let (root, ws) = fixture("fail");
    let outcome = apply_patch_cycle(&ws, &request(Some("exp2"), "false"), "autonomous").unwrap();

    assert!(!outcome.test_passed);
    assert!(outcome.reverted);
    assert!(
        outcome.isolated_workspace.is_some(),
        "the fenced workspace must be named even on failure"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("a.txt")).unwrap(),
        "old\n",
        "a rejected candidate must not mutate the live workspace"
    );
    assert!(
        ws.read_dir().unwrap().count() == 1,
        "no tx journals or byproducts may leak into the live tree"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Production wiring: two candidates for the same patch id still get
/// distinct fenced workspaces (the nonce holds through the whole
/// apply_patch_cycle path), so concurrent experiments can never collide
/// on — or write into — one another's checkout.
#[test]
fn vc_201_013_mastery_concurrent_candidates_get_disjoint_fences() {
    let (root, ws) = fixture("conc");
    let a = apply_patch_cycle(&ws, &request(Some("shared"), "true"), "autonomous").unwrap();
    let mut req2 = request(Some("shared"), "true");
    req2.files[0].old = "new\n".into();
    req2.files[0].new = "newer\n".into();
    let b = apply_patch_cycle(&ws, &req2, "autonomous").unwrap();
    let (a, b) = (a.isolated_workspace.unwrap(), b.isolated_workspace.unwrap());
    assert_ne!(
        a, b,
        "two candidates for the same patch id must not share one workspace"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Production wiring: a candidate patch whose declared target climbs out
/// of the workspace is refused on the isolated path exactly as on the
/// direct path — `..` never resolves onto the live tree's parent or a
/// sibling experiment's fence.
#[test]
fn vc_201_013_mastery_candidate_patch_paths_stay_confined() {
    let (root, ws) = fixture("conf");
    let mut req = request(Some("exp3"), "true");
    req.files[0].path = "../escaped.txt".into();
    assert!(
        apply_patch_cycle(&ws, &req, "autonomous").is_err(),
        "a patch path that climbs out must be refused"
    );
    assert!(!root.join("escaped.txt").exists());
    let _ = std::fs::remove_dir_all(&root);
}
