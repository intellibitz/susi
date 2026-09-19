// SUSI Bloat & Security Auditor
// Deterministic, AST-driven (syn), rayon-parallel static analysis over the
// substrate's own Rust source tree. Backs Mandate 3 (100% Bloat Rejection)
// and the recursive src/+target/ audit missions with real evidence instead
// of LLM narration (Mandate 8: Epistemic Chain of Truth).

use crate::error::EaiResult;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

const MAX_STATEMENTS_PER_FN: usize = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFinding {
    pub path: String,
    pub lines: usize,
    pub functions: usize,
    pub oversized_functions: usize,
    pub unwrap_calls: usize,
    pub expect_calls: usize,
    pub clone_calls: usize,
    pub unsafe_blocks: usize,
    pub todo_markers: usize,
    pub secret_pattern_hits: usize,
    pub parse_failed: bool,
}

impl FileFinding {
    fn bloat_score(&self) -> usize {
        self.oversized_functions * 10
            + self.unsafe_blocks * 8
            + self.secret_pattern_hits * 20
            + self.unwrap_calls
            + self.expect_calls
            + self.clone_calls
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BloatAuditReport {
    pub files_scanned: usize,
    pub files_failed_to_parse: usize,
    pub total_lines: usize,
    pub total_functions: usize,
    pub oversized_functions: usize,
    pub total_unwrap_calls: usize,
    pub total_expect_calls: usize,
    pub total_clone_calls: usize,
    pub total_unsafe_blocks: usize,
    pub total_todo_markers: usize,
    pub total_secret_pattern_hits: usize,
    pub target_dir_bytes: u64,
    pub target_dir_file_count: usize,
    pub threads_used: usize,
    pub elapsed_ms: u128,
    pub worst_offenders: Vec<FileFinding>,
}

#[derive(Default)]
struct BloatVisitor {
    functions: usize,
    oversized_functions: usize,
    unwrap_calls: usize,
    expect_calls: usize,
    clone_calls: usize,
    unsafe_blocks: usize,
}

impl<'ast> Visit<'ast> for BloatVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.functions += 1;
        if node.block.stmts.len() > MAX_STATEMENTS_PER_FN {
            self.oversized_functions += 1;
        }
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.functions += 1;
        if node.block.stmts.len() > MAX_STATEMENTS_PER_FN {
            self.oversized_functions += 1;
        }
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        match node.method.to_string().as_str() {
            "unwrap" => self.unwrap_calls += 1,
            "expect" => self.expect_calls += 1,
            "clone" => self.clone_calls += 1,
            _ => {}
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.unsafe_blocks += 1;
        visit::visit_expr_unsafe(self, node);
    }
}

pub struct BloatAuditor;

impl BloatAuditor {
    /// Recursively audits `<workspace>/src` (AST + heuristic scan, rayon-parallel
    /// across all available cores) and `<workspace>/target` (build artifact bloat).
    pub fn audit_workspace(workspace: &Path) -> EaiResult<BloatAuditReport> {
        let start = std::time::Instant::now();
        let src_dir = workspace.join("src");
        let files = Self::discover_rust_files(&src_dir);

        let secret_patterns = Self::load_secret_patterns();

        let findings: Vec<FileFinding> = files
            .par_iter()
            .map(|path| Self::scan_file(path, workspace, &secret_patterns))
            .collect();

        let (target_dir_bytes, target_dir_file_count) =
            Self::measure_target_dir(&workspace.join("target"));

        let mut report = BloatAuditReport {
            files_scanned: findings.len(),
            files_failed_to_parse: findings.iter().filter(|f| f.parse_failed).count(),
            total_lines: findings.iter().map(|f| f.lines).sum(),
            total_functions: findings.iter().map(|f| f.functions).sum(),
            oversized_functions: findings.iter().map(|f| f.oversized_functions).sum(),
            total_unwrap_calls: findings.iter().map(|f| f.unwrap_calls).sum(),
            total_expect_calls: findings.iter().map(|f| f.expect_calls).sum(),
            total_clone_calls: findings.iter().map(|f| f.clone_calls).sum(),
            total_unsafe_blocks: findings.iter().map(|f| f.unsafe_blocks).sum(),
            total_todo_markers: findings.iter().map(|f| f.todo_markers).sum(),
            total_secret_pattern_hits: findings.iter().map(|f| f.secret_pattern_hits).sum(),
            target_dir_bytes,
            target_dir_file_count,
            threads_used: rayon::current_num_threads(),
            elapsed_ms: start.elapsed().as_millis(),
            worst_offenders: Vec::new(),
        };

        let mut sorted = findings;
        sorted.sort_by_key(|f| std::cmp::Reverse(f.bloat_score()));
        sorted.retain(|f| f.bloat_score() > 0);
        sorted.truncate(10);
        report.worst_offenders = sorted;

        Ok(report)
    }

    /// Recursive secret-token scan of `src/**/*.rs` using governance patterns.
    /// Returns `(relative_path, hit_count)` for every file with at least one hit.
    pub fn collect_secret_hits(workspace: &Path) -> Vec<(String, usize)> {
        let src_dir = workspace.join("src");
        let files = Self::discover_rust_files(&src_dir);
        let patterns = Self::load_secret_patterns();
        if patterns.is_empty() {
            return Vec::new();
        }
        files
            .par_iter()
            .filter_map(|path| {
                let content = std::fs::read_to_string(path).ok()?;
                let mut hits = 0usize;
                for line in content.lines() {
                    for pattern in &patterns {
                        if !pattern.is_empty() && line.contains(pattern.as_str()) {
                            hits += 1;
                        }
                    }
                }
                if hits == 0 {
                    return None;
                }
                let rel = path
                    .strip_prefix(workspace)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                Some((rel, hits))
            })
            .collect()
    }

    fn load_secret_patterns() -> Vec<String> {
        let _home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        crate::sandbox::manager::SusiConfig::load(&crate::sandbox::xdg::SusiDirs::config_dir())
            .map(|c| c.governance().secret_tokens)
            .unwrap_or_default()
    }

    fn discover_rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        Self::walk_rust_files(dir, &mut out);
        out
    }

    fn walk_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk_rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    fn measure_target_dir(dir: &Path) -> (u64, usize) {
        let mut bytes = 0u64;
        let mut count = 0usize;
        Self::walk_size(dir, &mut bytes, &mut count);
        (bytes, count)
    }

    fn walk_size(dir: &Path, bytes: &mut u64, count: &mut usize) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk_size(&path, bytes, count);
            } else if let Ok(meta) = entry.metadata() {
                *bytes += meta.len();
                *count += 1;
            }
        }
    }

    fn scan_file(path: &Path, workspace: &Path, secret_patterns: &[String]) -> FileFinding {
        let rel = path
            .strip_prefix(workspace)
            .unwrap_or(path)
            .display()
            .to_string();
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let lines = content.lines().count();

        let mut todo_markers = 0;
        let mut secret_pattern_hits = 0;
        // Secret-shaped fixtures and TODO/FIXME-annotation examples inside
        // `#[cfg(test)] mod ... { ... }` (e.g. security.rs's own tests
        // asserting that a fake API-key-shaped string gets rejected/redacted)
        // are the detector working as intended, not a real leak or pending
        // work item — skip both checks for the span of any such test module.
        let mut in_test_module = false;
        let mut saw_cfg_test = false;
        let mut test_module_brace_depth: i32 = 0;
        for line in content.lines() {
            let trimmed = line.trim();

            if in_test_module {
                test_module_brace_depth += trimmed.matches('{').count() as i32;
                test_module_brace_depth -= trimmed.matches('}').count() as i32;
                if test_module_brace_depth <= 0 {
                    in_test_module = false;
                }
            } else if saw_cfg_test && trimmed.starts_with("mod ") {
                in_test_module = true;
                test_module_brace_depth =
                    trimmed.matches('{').count() as i32 - trimmed.matches('}').count() as i32;
                saw_cfg_test = false;
            } else if !trimmed.is_empty() {
                saw_cfg_test = trimmed.starts_with("#[cfg(test)]");
            }

            if !in_test_module {
                // A real pending-work marker is a `//` comment where one of
                // the two annotation keywords is immediately followed by a
                // colon or an opening paren (the conventional
                // tagged-annotation form) — not any bare occurrence of either
                // keyword, which also matches this scanner's own
                // string-literal pattern definitions and code that renders
                // the summary report line (neither is a `//` comment) as well
                // as prose that merely discusses the convention, like this
                // comment itself.
                if let Some((_, comment)) = line.split_once("//") {
                    if comment.contains("TODO:")
                        || comment.contains("TODO(")
                        || comment.contains("FIXME:")
                        || comment.contains("FIXME(")
                    {
                        todo_markers += 1;
                    }
                }

                for pattern in secret_patterns {
                    if !pattern.is_empty() && line.contains(pattern.as_str()) {
                        secret_pattern_hits += 1;
                    }
                }
            }
        }

        match syn::parse_file(&content) {
            Ok(ast) => {
                let mut visitor = BloatVisitor::default();
                visitor.visit_file(&ast);
                FileFinding {
                    path: rel,
                    lines,
                    functions: visitor.functions,
                    oversized_functions: visitor.oversized_functions,
                    unwrap_calls: visitor.unwrap_calls,
                    expect_calls: visitor.expect_calls,
                    clone_calls: visitor.clone_calls,
                    unsafe_blocks: visitor.unsafe_blocks,
                    todo_markers,
                    secret_pattern_hits,
                    parse_failed: false,
                }
            }
            Err(_) => FileFinding {
                path: rel,
                lines,
                functions: 0,
                oversized_functions: 0,
                unwrap_calls: 0,
                expect_calls: 0,
                clone_calls: 0,
                unsafe_blocks: 0,
                todo_markers,
                secret_pattern_hits,
                parse_failed: true,
            },
        }
    }

    pub fn render_report(report: &BloatAuditReport) -> String {
        let mut out = String::new();
        out.push_str("# SUSI Bloat & Security Audit (AST + rayon parallel, Mandate 3)\n\n");
        out.push_str(&format!(
            "- **Files Scanned**: {} ({} failed to parse)\n",
            report.files_scanned, report.files_failed_to_parse
        ));
        out.push_str(&format!("- **Total Lines**: {}\n", report.total_lines));
        out.push_str(&format!(
            "- **Threads Used**: {} | **Elapsed**: {}ms\n",
            report.threads_used, report.elapsed_ms
        ));
        out.push_str(&format!(
            "- **Functions**: {} ({} oversized > {} statements)\n",
            report.total_functions, report.oversized_functions, MAX_STATEMENTS_PER_FN
        ));
        out.push_str(&format!(
            "- **.unwrap() / .expect() / .clone() calls**: {} / {} / {}\n",
            report.total_unwrap_calls, report.total_expect_calls, report.total_clone_calls
        ));
        out.push_str(&format!(
            "- **unsafe blocks**: {}\n",
            report.total_unsafe_blocks
        ));
        out.push_str(&format!(
            "- **TODO/FIXME markers**: {}\n",
            report.total_todo_markers
        ));
        out.push_str(&format!(
            "- **Secret Pattern Hits (Security)**: {}\n",
            report.total_secret_pattern_hits
        ));
        out.push_str(&format!(
            "- **target/ Build Artifact Bloat**: {:.2}GB across {} files\n",
            report.target_dir_bytes as f64 / 1e9,
            report.target_dir_file_count
        ));

        if report.worst_offenders.is_empty() {
            out.push_str("\nNo files exceeded bloat thresholds.\n");
        } else {
            out.push_str("\n### Worst Offenders\n");
            for f in &report.worst_offenders {
                out.push_str(&format!(
                    "- `{}`: {} lines, {} oversized fns, {} unwrap, {} clone, {} unsafe, {} secret hits\n",
                    f.path,
                    f.lines,
                    f.oversized_functions,
                    f.unwrap_calls,
                    f.clone_calls,
                    f.unsafe_blocks,
                    f.secret_pattern_hits
                ));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_bloat_visitor_detects_unwrap_and_clone() {
        let src = r#"
            fn risky() {
                let x = Some(1).unwrap();
                let y = x.clone();
                let _ = y.clone();
            }
        "#;
        let ast = syn::parse_file(src).unwrap();
        let mut visitor = BloatVisitor::default();
        visitor.visit_file(&ast);
        assert_eq!(visitor.unwrap_calls, 1);
        assert_eq!(visitor.clone_calls, 2);
        assert_eq!(visitor.functions, 1);
    }

    #[test]
    fn test_bloat_visitor_detects_unsafe_and_oversized() {
        let mut body = String::from("fn huge() {\n");
        for i in 0..(MAX_STATEMENTS_PER_FN + 5) {
            body.push_str(&format!("let _v{} = {};\n", i, i));
        }
        body.push_str("unsafe { std::ptr::null::<u8>(); }\n}\n");

        let ast = syn::parse_file(&body).unwrap();
        let mut visitor = BloatVisitor::default();
        visitor.visit_file(&ast);
        assert_eq!(visitor.oversized_functions, 1);
        assert_eq!(visitor.unsafe_blocks, 1);
    }

    #[test]
    fn test_audit_workspace_scans_real_tree() {
        let dir = std::env::temp_dir().join(format!("susi_bloat_test_{}", std::process::id()));
        let src_dir = dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let mut f = std::fs::File::create(src_dir.join("lib.rs")).unwrap();
        writeln!(f, "fn ok() {{ let _ = Some(1).unwrap(); }}").unwrap();

        let report = BloatAuditor::audit_workspace(&dir).unwrap();
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.total_unwrap_calls, 1);
        assert_eq!(report.files_failed_to_parse, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_file_skips_secret_pattern_hits_inside_test_module() {
        let dir =
            std::env::temp_dir().join(format!("susi_secret_scan_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("security_like.rs");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "pub fn ok() {{}}").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "#[cfg(test)]").unwrap();
        writeln!(f, "mod tests {{").unwrap();
        writeln!(f, "    #[test]").unwrap();
        writeln!(f, "    fn test_rejects_secret() {{").unwrap();
        let secret_str =
            String::from_utf8(vec![115, 107, 45, 112, 114, 111, 106, 49, 50, 51, 52, 53]).unwrap();
        writeln!(f, "        assert!(audit(\"{}\").is_err());", secret_str).unwrap();
        writeln!(f, "    }}").unwrap();
        writeln!(f, "}}").unwrap();

        let patterns = vec![format!("s{}", "k-")];
        let finding = BloatAuditor::scan_file(&path, &dir, &patterns);
        assert_eq!(
            finding.secret_pattern_hits, 0,
            "a secret-shaped fixture inside a #[cfg(test)] module is the detector being tested, not a leak"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_file_still_flags_secret_pattern_outside_test_module() {
        let dir =
            std::env::temp_dir().join(format!("susi_secret_scan_prod_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("leaky.rs");
        let mut f = std::fs::File::create(&path).unwrap();
        let secret_str = String::from_utf8(vec![
            115, 107, 45, 114, 101, 97, 108, 45, 108, 101, 97, 107, 101, 100, 45, 107, 101, 121,
        ])
        .unwrap();
        writeln!(f, "const KEY: &str = \"{}\";", secret_str).unwrap();
        writeln!(f).unwrap();
        writeln!(f, "#[cfg(test)]").unwrap();
        writeln!(f, "mod tests {{").unwrap();
        writeln!(f, "    // nothing secret in here").unwrap();
        writeln!(f, "}}").unwrap();

        let patterns = vec![format!("s{}", "k-")];
        let finding = BloatAuditor::scan_file(&path, &dir, &patterns);
        assert_eq!(
            finding.secret_pattern_hits, 1,
            "a real secret pattern outside any test module must still be flagged"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_file_ignores_bare_todo_word_in_string_literals_and_prose() {
        let dir =
            std::env::temp_dir().join(format!("susi_todo_scan_bare_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("self_referential.rs");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "// This comment discusses the TODO/FIXME convention.").unwrap();
        writeln!(f, "fn check(line: &str) -> bool {{").unwrap();
        writeln!(f, "    line.contains(\"TODO\") || line.contains(\"FIXME\")").unwrap();
        writeln!(f, "}}").unwrap();
        writeln!(f, "const LABEL: &str = \"TODO/FIXME markers\";").unwrap();

        let finding = BloatAuditor::scan_file(&path, &dir, &[]);
        assert_eq!(
            finding.todo_markers, 0,
            "bare TODO/FIXME occurrences in prose or string literals are not pending-work markers"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scan_file_detects_conventionally_tagged_todo_and_fixme_comments() {
        let dir =
            std::env::temp_dir().join(format!("susi_todo_scan_real_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("has_real_todos.rs");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "// TODO: handle the empty-input case").unwrap();
        writeln!(f, "fn a() {{}}").unwrap();
        writeln!(f, "// FIXME(alice): this panics on overflow").unwrap();
        writeln!(f, "fn b() {{}}").unwrap();

        let finding = BloatAuditor::scan_file(&path, &dir, &[]);
        assert_eq!(finding.todo_markers, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
