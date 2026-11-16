//! Git object frames may contain newlines and invalid bytes. A bad board entry
//! must not move the following entry into its slot or hide chain damage.
use std::fs;
use std::path::Path;
use std::process::Stdio;
use susi_core::native_board::{load_git, SCHEMA};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = susi_paths::console::background_command("git")
        .current_dir(repo)
        .stdin(Stdio::null())
        .args(args)
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture Git output")
        .trim()
        .to_owned()
}

fn item(n: u64, prev: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "schema":SCHEMA,"n":n,"kind":"note","agent":"fixture","host":"test",
        "posted_unix":1,"title":"entry","body":"Unicode Ω and\nseveral lines","prev":prev,
    }))
    .expect("encode board item")
}

#[test]
fn mixed_git_objects_preserve_numbering_diagnostics_and_annotated_blob_targets() {
    let dir = susi_paths::tempfile::tempdir().expect("fixture directory");
    let repo = dir.path();
    git(repo, &["init", "--quiet"]);
    let blob = |bytes: &[u8]| {
        fs::write(repo.join("fixture"), bytes).expect("write fixture blob");
        git(repo, &["hash-object", "-w", "fixture"])
    };
    let post = |n, oid: &str| {
        git(repo, &["update-ref", &format!("refs/board/items/{n}"), oid]);
    };
    let first = blob(&item(1, ""));
    post(1, &first);
    post(2, &git(repo, &["mktree"]));
    post(3, &blob(b"bad\x00json\n1234\n{}\n"));
    let fourth = blob(&item(4, "broken-link"));
    post(4, &fourth);
    let seventh = blob(&item(71, &fourth));
    post(7, &seventh);
    let eighth = blob(&item(8, &seventh));
    git(
        repo,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "tag.gpgSign=false",
            "tag",
            "-a",
            "fixture-tag",
            "-m",
            "fixture",
            &eighth,
        ],
    );
    let tag = git(repo, &["rev-parse", "refs/tags/fixture-tag"]);
    post(8, &tag);

    let (items, findings) = load_git(repo).expect("read even the damaged sequence");
    assert_eq!(
        items.iter().map(|stored| stored.n).collect::<Vec<_>>(),
        [1, 4, 7, 8]
    );
    assert_eq!(items[3].sha, tag);
    assert_eq!(items[3].item.body, "Unicode Ω and\nseveral lines");
    assert_eq!(findings.len(), 5);
    for (finding, expected) in findings.iter().zip([
        "item 2 cannot be decoded:",
        "item 3 cannot be decoded:",
        "item 4 does not chain from item 3",
        "item 7 claims to be item 71",
        "items 5 to 6 are missing",
    ]) {
        assert!(finding.starts_with(expected), "{finding}");
    }
}
