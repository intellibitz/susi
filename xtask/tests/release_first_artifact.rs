#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)]

//! Every verified release platform can publish without waiting for siblings.

use std::collections::BTreeMap;

fn jobs(text: &str) -> BTreeMap<String, String> {
    let mut jobs = BTreeMap::<String, String>::new();
    let mut current = None::<String>;
    let mut in_jobs = false;
    for line in text.lines() {
        if line == "jobs:" {
            in_jobs = true;
            continue;
        }
        if !in_jobs || line.trim_start().starts_with('#') {
            continue;
        }
        let id = line
            .strip_prefix("  ")
            .and_then(|rest| rest.strip_suffix(':'))
            .filter(|rest| !rest.is_empty() && !rest.starts_with(' '));
        if let Some(id) = id {
            current = Some(id.to_string());
            jobs.insert(id.to_string(), String::new());
        } else if let Some(id) = &current {
            jobs.get_mut(id).unwrap().push_str(&format!("{line}\n"));
        }
    }
    jobs
}

#[test]
fn each_verified_platform_can_publish_first_without_waiting_for_siblings() {
    let text = include_str!("../../.github/workflows/release.yml");
    let jobs = jobs(text);
    let platforms = [
        ("build", "publish", "windows-x86_64"),
        ("build-linux", "publish-linux", "linux-x86_64"),
        ("build-arm", "publish-arm", "linux-aarch64"),
        ("build-macos", "publish-macos", "macos-aarch64"),
        ("build-cuda", "publish-cuda", "cuda-linux-x86_64"),
    ];
    for (builder, publisher, artifact) in platforms {
        let build = &jobs[builder];
        let publish = &jobs[publisher];
        assert!(build.contains("needs: [verify-tag]"), "{builder}");
        assert!(build.contains("engine_ready:"), "{builder}");
        assert!(
            publish.contains(&format!("needs: [verify-tag, tests, {builder}]")),
            "{publisher}"
        );
        assert!(
            publish.contains("needs.tests.result == 'success'"),
            "{publisher}"
        );
        assert!(
            publish.contains("needs.verify-tag.outputs.tag != ''"),
            "{publisher}"
        );
        assert!(
            publish.contains(&format!("needs.{builder}.result == 'success'")),
            "{publisher}"
        );
        assert!(
            publish.contains(&format!(
                "needs.{builder}.outputs.engine_ready == 'success'"
            )),
            "{publisher}"
        );
        assert!(
            publish.contains(&format!("RELEASE_ENGINE: release-{artifact}")),
            "{publisher}"
        );
        if publisher != "publish" {
            assert!(
                publish.contains("concurrency: *publisher-concurrency"),
                "{publisher}"
            );
            assert!(publish.contains("steps: *publisher-steps"), "{publisher}");
        }
    }
    let first = &jobs["publish"];
    assert!(first.contains("group: release-first-asset-${{ github.ref }}"));
    assert!(first.contains("queue: max") && first.contains("cancel-in-progress: false"));
    assert!(first.contains("actions/download-artifact@v4"));
    assert!(first.contains("softprops/action-gh-release@v3"));
    assert!(first.contains("if: env.RELEASE_HARNESS_READY == 'success'"));
    assert!(first.contains("fail_on_unmatched_files: true"));
    assert!(jobs["build"].contains("id: engine_upload"));
    assert!(jobs["build-cuda"].contains("id: cuda_upload"));
    assert!(jobs["prune-old-releases"].contains("needs.publish-linux.result == 'success'"));
}
