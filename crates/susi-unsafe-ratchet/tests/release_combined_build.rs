#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

#[test]
fn combined_release_build_retries_engine_independently() {
    let workflow = include_str!("../../../.github/workflows/release.yml").replace("\r\n", "\n");
    let engine_step = workflow
        .split_once("      - name: Build Engine\n")
        .unwrap()
        .1
        .split_once("      - name: Rename Binary\n")
        .unwrap()
        .0;
    let command = engine_step
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("run: "))
        .unwrap();
    let script = format!(
        r#"cargo() {{
  printf 'CALL:%s\n' "$*"
  case "$*" in
    *susi-harness*) return "$FIRST_STATUS" ;;
    *) return "$SECOND_STATUS" ;;
  esac
}}
{command}
"#
    );
    for (first, second, succeeds, calls) in [(0, 1, true, 1), (1, 0, true, 2), (1, 1, false, 2)] {
        let out = Command::new(if cfg!(windows) { "sh" } else { "bash" })
            .args(["-c", &script])
            .env("FIRST_STATUS", first.to_string())
            .env("SECOND_STATUS", second.to_string())
            .output()
            .unwrap();
        assert_eq!(out.status.success(), succeeds);
        let stdout = String::from_utf8(out.stdout).unwrap();
        let invocations: Vec<_> = stdout.lines().collect();
        assert_eq!(invocations.len(), calls, "{stdout}");
        let args: Vec<_> = invocations[0].split_whitespace().collect();
        assert!(
            args.windows(2).any(|pair| pair == ["-p", "susi-harness"]),
            "{stdout}"
        );
        assert!(
            args.windows(2).any(|pair| pair == ["-p", "susi"]),
            "{stdout}"
        );
        if calls == 2 {
            assert_eq!(invocations[1], "CALL:build --profile dist");
        }
    }
}
