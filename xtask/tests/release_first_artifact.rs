#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)]

//! Release publication must not wait for the slower first-class platform.

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
fn either_verified_platform_can_publish_first_and_the_other_continues() {
    let text = include_str!("../../.github/workflows/release.yml");
    let jobs = jobs(text);
    let windows = &jobs["build"];
    let linux = &jobs["build-linux"];
    assert!(windows.contains("name: windows-x86_64"));
    assert!(!windows.contains("name: linux-x86_64"));
    assert!(linux.contains("name: linux-x86_64"));
    assert!(linux.contains("needs: [verify-tag]"));
    assert!(linux.contains("steps: *release-steps"));

    let first = &jobs["publish"];
    let other = &jobs["publish-linux"];
    assert!(first.contains("needs: [verify-tag, tests, build]"));
    assert!(other.contains("needs: [verify-tag, tests, build-linux]"));
    for (job, leg) in [(first, "build"), (other, "build-linux")] {
        assert!(job.contains("needs.tests.result == 'success'"));
        assert!(job.contains("needs.verify-tag.outputs.tag != ''"));
        assert!(job.contains(&format!("needs.{leg}.result == 'success'")));
        assert!(job.contains("group: release-first-asset-${{ github.ref }}"));
        assert!(job.contains("cancel-in-progress: false"));
        assert!(job.contains("actions/download-artifact@v4"));
        assert!(job.contains("softprops/action-gh-release@v3"));
    }
    assert!(!first.contains("needs.build-linux"));
    assert!(!other.contains("needs.publish"));
    assert!(jobs["publish-extra-fallback"].contains("needs.publish.result != 'success'"));
    assert!(jobs["publish-extra-fallback"].contains("needs.publish-linux.result == 'success'"));
    assert!(jobs["publish-extra-fallback"].contains("steps: *publish-extra-steps"));
    assert!(jobs["prune-old-releases"].contains("needs.publish-linux.result == 'success'"));
}
