## Summary

Briefly describe what this Pull Request changes and why.

Closes #

## Verification Checklist

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` passes
- [ ] Unit / integration tests added or updated (`cargo test --locked`)
- [ ] `cargo test --test core_docs_promise_no_ui_or_plugins --locked` passes (if Markdown docs were modified)
- [ ] No `unsafe` blocks or unjustified `unwrap()` / `expect()` / `panic!()` in production code
- [ ] Respects the open-core boundary (no UI assets or bundled third-party product catalogs in core)
