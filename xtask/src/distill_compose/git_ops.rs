use super::*;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

pub(in crate::distill_compose) struct Git<'a> {
    pub(in crate::distill_compose) root: &'a Path,
}

impl Git<'_> {
    pub(in crate::distill_compose) fn output(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<Output> {
        let mut command = Command::new("git");
        command
            .current_dir(self.root)
            .arg("--literal-pathspecs")
            .arg("--no-replace-objects")
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_GRAFT_FILE",
        ] {
            command.env_remove(key);
        }
        #[cfg(test)]
        {
            command.env_clear();
            for key in [
                "PATH",
                "SystemRoot",
                "WINDIR",
                "COMSPEC",
                "PATHEXT",
                "TEMP",
                "TMP",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            command
                .env("HOME", self.root)
                .env("USERPROFILE", self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    self.root.join(".absent-fixture-config"),
                )
                .env("GIT_AUTHOR_NAME", "Susi Distiller")
                .env("GIT_AUTHOR_EMAIL", "distiller@localhost")
                .env("GIT_COMMITTER_NAME", "Susi Distiller")
                .env("GIT_COMMITTER_EMAIL", "distiller@localhost")
                .env("GIT_TERMINAL_PROMPT", "0");
        }
        if args.first() == Some(&"push") {
            let executable = std::env::current_exe().map_err(|e| e.to_string())?;
            if let Some(path) = adjacent_helper_path(&executable, std::env::var_os("PATH"))? {
                command.env("PATH", path);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start Git: {e}"))?;
        if let Some(bytes) = input {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "Git stdin unavailable".to_owned())?;
            return std::thread::scope(|scope| {
                let writer = scope.spawn(move || stdin.write_all(bytes));
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("cannot wait for Git: {e}"))?;
                let write_res = writer
                    .join()
                    .map_err(|_| "Git stdin writer thread panicked".to_owned())?;
                if out.status.success() {
                    write_res.map_err(|e| format!("cannot write Git input: {e}"))?;
                }
                Ok(out)
            });
        }
        child
            .wait_with_output()
            .map_err(|e| format!("cannot wait for Git: {e}"))
    }

    pub(in crate::distill_compose) fn text(&self, args: &[&str]) -> Result<String> {
        let output = self.output(args, None)?;
        if !output.status.success() {
            return Err(format!("Git {} failed ({})", args[0], output.status));
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_owned())
            .map_err(|_| "Git returned non-UTF-8 output".to_owned())
    }

    pub(in crate::distill_compose) fn bytes(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let output = self.output(args, input)?;
        if !output.status.success() {
            return Err(format!("Git {} failed ({})", args[0], output.status));
        }
        Ok(output.stdout)
    }

    pub(in crate::distill_compose) fn require_original_history(&self) -> Result<()> {
        if std::env::var_os("GIT_GRAFT_FILE").is_some() {
            return Err("distillation refuses a GIT_GRAFT_FILE override".to_owned());
        }
        let path = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/grafts",
        ])?);
        if !path.is_absolute() || path.try_exists().map_err(|e| e.to_string())? {
            return Err(
                "legacy Git grafts can alter history; distillation requires the original commit graph"
                    .to_owned(),
            );
        }
        Ok(())
    }

    pub(in crate::distill_compose) fn resolve_commit(&self, rev: &str) -> Result<String> {
        if rev.is_empty() || rev.starts_with('-') || rev.chars().any(char::is_control) {
            return Err("invalid Git revision".to_owned());
        }
        let spec = format!("{rev}^{{commit}}");
        let oid = self.text(&["rev-parse", "--verify", &spec])?;
        valid_oid(&oid)?;
        Ok(oid)
    }

    pub(in crate::distill_compose) fn tree(&self, commit: &str) -> Result<String> {
        valid_oid(commit)?;
        let spec = format!("{commit}^{{tree}}");
        let oid = self.text(&["rev-parse", "--verify", &spec])?;
        valid_oid(&oid)?;
        Ok(oid)
    }

    pub(in crate::distill_compose) fn ancestor(
        &self,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool> {
        valid_oid(ancestor)?;
        valid_oid(descendant)?;
        let output = self.output(&["merge-base", "--is-ancestor", ancestor, descendant], None)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err("cannot verify Git ancestry".to_owned()),
        }
    }

    pub(in crate::distill_compose) fn state_path(
        &self,
        category: &str,
        name: &str,
    ) -> Result<PathBuf> {
        valid_name(name)?;
        let common = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?);
        if !common.is_absolute() {
            return Err("Git common directory is not absolute".to_owned());
        }
        Ok(common
            .join("susi-distill")
            .join(category)
            .join(format!("{name}.json")))
    }

    pub(in crate::distill_compose) fn zero_oid(&self, sample_oid: &str) -> &'static str {
        if sample_oid.len() == 64 {
            ZERO_SHA256
        } else {
            ZERO_SHA1
        }
    }

    pub(in crate::distill_compose) fn ensure_cas_ref(
        &self,
        reference: &str,
        commit: &str,
    ) -> Result<()> {
        valid_oid(commit)?;
        if let Ok(existing) = self.text(&["show-ref", "--verify", "--hash", reference]) {
            if existing == commit {
                return Ok(());
            }
            return Err(format!(
                "ref {reference} already exists at a different commit; refusing to overwrite"
            ));
        }
        let zero = self.zero_oid(commit);
        let output = self.output(&["update-ref", reference, commit, zero], None)?;
        if output.status.success() {
            return Ok(());
        }
        if self.text(&["show-ref", "--verify", "--hash", reference])? == commit {
            return Ok(());
        }
        Err(format!(
            "concurrent creation raced on {reference}; no ref was overwritten"
        ))
    }

    pub(in crate::distill_compose) fn endpoint_optional(&self) -> Result<Option<Endpoint>> {
        let fetch_out = self.output(&["remote", "get-url", "--all", "origin"], None)?;
        if !fetch_out.status.success() {
            return Ok(None);
        }
        let fetch = String::from_utf8(fetch_out.stdout)
            .map_err(|_| "non-UTF-8 origin URL".to_owned())?
            .trim()
            .to_owned();
        let push = self.text(&["remote", "get-url", "--push", "--all", "origin"])?;
        if fetch.is_empty() || fetch.lines().count() != 1 || fetch != push {
            return Err(
                "origin must have exactly one matching fetch and push destination".to_owned(),
            );
        }
        let address = normalize_endpoint(self.root, &fetch)?;
        let digest = sha256_hex(format!("susi-distill-endpoint-v1\n{address}\n").as_bytes());
        Ok(Some(Endpoint { address, digest }))
    }

    pub(in crate::distill_compose) fn local_ref_oid(
        &self,
        reference: &str,
    ) -> Result<Option<String>> {
        let output = self.output(&["show-ref", "--verify", "--hash", reference], None)?;
        if !output.status.success() {
            return Ok(None);
        }
        let oid = String::from_utf8(output.stdout)
            .map_err(|_| "Git returned non-UTF-8 ref output".to_owned())?
            .trim()
            .to_owned();
        if oid.is_empty() {
            return Ok(None);
        }
        valid_oid(&oid)?;
        Ok(Some(oid))
    }

    /// Fetches an explicitly named immutable remote ref when it is absent locally.
    ///
    /// Distillation accepts snapshot/release tags and agent handoff refs as remote
    /// inputs. It never fetches arbitrary revisions or remote branches here, and it
    /// never overwrites an existing local ref: a changed remote ref is an error.
    pub(in crate::distill_compose) fn fetch_explicit_remote_ref(
        &self,
        endpoint: &Endpoint,
        reference: &str,
    ) -> Result<()> {
        let fetchable =
            reference.starts_with("refs/tags/") || reference.starts_with("refs/agents/");
        if !fetchable {
            return Ok(());
        }
        if reference
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(format!("invalid explicit remote ref: {reference}"));
        }
        let format_check = self.output(&["check-ref-format", reference], None)?;
        if !format_check.status.success() {
            return Err(format!("invalid explicit remote ref: {reference}"));
        }

        let local = self.local_ref_oid(reference)?;
        let remote = match self.remote_ref(endpoint, reference) {
            Ok(remote) => remote,
            Err(_) if local.is_some() => return Ok(()),
            Err(error) => {
                return Err(format!("cannot inspect remote ref {reference}: {error}"));
            }
        };
        let Some(remote_oid) = remote else {
            return Ok(());
        };

        if let Some(local_oid) = local {
            if local_oid == remote_oid {
                return Ok(());
            }
            return Err(format!(
                "remote ref {reference} differs from the existing local ref; refusing to overwrite an immutable ref"
            ));
        }

        let refspec = format!("{reference}:{reference}");
        let fetched = self.output(
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--recurse-submodules=no",
                "--",
                &endpoint.address,
                &refspec,
            ],
            None,
        )?;
        if !fetched.status.success() {
            return Err(format!(
                "Git fetch of remote ref {reference} failed ({})",
                fetched.status
            ));
        }
        match self.local_ref_oid(reference)? {
            Some(local_oid) if local_oid == remote_oid => Ok(()),
            Some(_) => Err(format!(
                "Git fetch produced a different object for remote ref {reference}"
            )),
            None => Err(format!(
                "Git fetch did not create expected remote ref {reference}"
            )),
        }
    }

    pub(in crate::distill_compose) fn pull_explicit_remote_ref(
        &self,
        reference: &str,
    ) -> Result<()> {
        if !reference.starts_with("refs/tags/") && !reference.starts_with("refs/agents/") {
            return Ok(());
        }
        let Ok(Some(endpoint)) = self.endpoint_optional() else {
            return Ok(());
        };
        self.fetch_explicit_remote_ref(&endpoint, reference)
    }

    /// Offline-safe remote pull + 3-way object-store reconciliation against `origin/main`.
    /// When `origin` is unconfigured or temporarily unreachable, falls back cleanly to
    /// local `effective_from`. When `origin/main` has advanced past `effective_from`,
    /// fast-forwards cleanly; when diverged, composes a deterministic conflict-free
    /// 3-way merge commit in the Git object store (`origin/main` + `local_from`).
    pub(in crate::distill_compose) fn pull_and_reconcile_source(
        &self,
        effective_from: &str,
    ) -> Result<String> {
        let Ok(Some(endpoint)) = self.endpoint_optional() else {
            return self.resolve_commit(effective_from);
        };
        self.fetch_explicit_remote_ref(&endpoint, effective_from)?;
        let fetch_out = self.output(
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--recurse-submodules=no",
                "--",
                &endpoint.address,
                "+refs/heads/main:refs/remotes/origin/main",
            ],
            None,
        )?;
        if !fetch_out.status.success() {
            return self.resolve_commit(effective_from);
        }
        let remote_main = match self.resolve_commit("refs/remotes/origin/main") {
            Ok(oid) => oid,
            Err(_) => return self.resolve_commit(effective_from),
        };
        let local_from = match self.resolve_commit(effective_from) {
            Ok(oid) => oid,
            Err(_) if effective_from == "origin/main" => return Ok(remote_main),
            Err(err) => return Err(err),
        };
        if local_from == remote_main || self.ancestor(&remote_main, &local_from)? {
            return Ok(local_from);
        }
        if self.ancestor(&local_from, &remote_main)? {
            return Ok(remote_main);
        }
        let merge_base = self.text(&["merge-base", &remote_main, &local_from])?;
        valid_oid(&merge_base)?;
        let merged_tree = match self.merge_three_way(&merge_base, &remote_main, &local_from)? {
            ThreeWayMerge::Clean(tree) => tree,
            ThreeWayMerge::Conflict(conflict) => {
                return Err(format!(
                    "remote pull reconciliation conflict between origin/main ({remote_main}) and {effective_from} ({local_from}): {conflict}"
                ));
            }
        };
        self.commit_tree_parents(
            &merged_tree,
            &[remote_main.as_str(), local_from.as_str()],
            "Reconcile origin/main with local distillation stream\n",
        )
    }

    pub(in crate::distill_compose) fn remote_ref(
        &self,
        endpoint: &Endpoint,
        reference: &str,
    ) -> Result<Option<String>> {
        let output = self.text(&["ls-remote", "--refs", "--", &endpoint.address, reference])?;
        if output.is_empty() {
            return Ok(None);
        }
        let mut lines = output.lines();
        let line = lines
            .next()
            .ok_or_else(|| "missing remote ref".to_owned())?;
        let (oid, name) = line
            .split_once('\t')
            .ok_or_else(|| "malformed remote ref".to_owned())?;
        valid_oid(oid)?;
        if name != reference || lines.next().is_some() {
            return Err("ambiguous or unexpected remote ref".to_owned());
        }
        Ok(Some(oid.to_owned()))
    }

    pub(in crate::distill_compose) fn tree_blobs(
        &self,
        commit: &str,
        prefix: &str,
    ) -> Result<Vec<(String, String)>> {
        valid_oid(commit)?;
        let raw = self.bytes(&["ls-tree", "-r", "-z", commit, "--", prefix], None)?;
        let mut out = Vec::new();
        for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let text =
                std::str::from_utf8(record).map_err(|_| "non-UTF-8 path in Git tree".to_owned())?;
            let (meta, path) = text
                .split_once('\t')
                .ok_or_else(|| "malformed ls-tree entry".to_owned())?;
            let parts: Vec<&str> = meta.split_whitespace().collect();
            if parts.len() != 3 || parts[1] != "blob" {
                continue;
            }
            valid_oid(parts[2])?;
            out.push((path.to_owned(), parts[2].to_owned()));
        }
        Ok(out)
    }

    pub(in crate::distill_compose) fn read_blobs(&self, oids: &[String]) -> Result<Vec<Vec<u8>>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let mut stdin_buf = String::new();
        for oid in oids {
            valid_oid(oid)?;
            stdin_buf.push_str(oid);
            stdin_buf.push('\n');
        }
        let raw = self.bytes(&["cat-file", "--batch"], Some(stdin_buf.as_bytes()))?;
        let mut cursor = 0usize;
        let mut results = Vec::with_capacity(oids.len());
        for expected in oids {
            let nl = raw[cursor..]
                .iter()
                .position(|b| *b == b'\n')
                .ok_or_else(|| "truncated cat-file header".to_owned())?;
            let header = std::str::from_utf8(&raw[cursor..cursor + nl])
                .map_err(|_| "invalid cat-file header".to_owned())?;
            cursor += nl + 1;
            let parts: Vec<&str> = header.split_whitespace().collect();
            if parts.len() != 3 || parts[0] != expected || parts[1] != "blob" {
                return Err(format!("missing or non-blob Git object {expected}"));
            }
            let size: usize = parts[2]
                .parse()
                .map_err(|_| "invalid cat-file blob size".to_owned())?;
            let end = cursor
                .checked_add(size)
                .filter(|end| *end < raw.len() && raw[*end] == b'\n')
                .ok_or_else(|| "truncated cat-file payload".to_owned())?;
            results.push(raw[cursor..end].to_vec());
            cursor = end + 1;
        }
        Ok(results)
    }

    pub(in crate::distill_compose) fn diff_paths(
        &self,
        left: &str,
        right: &str,
    ) -> Result<Vec<String>> {
        valid_oid(left)?;
        valid_oid(right)?;
        let raw = self.bytes(
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                left,
                right,
            ],
            None,
        )?;
        let mut paths = BTreeSet::new();
        for entry in raw.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let path =
                std::str::from_utf8(entry).map_err(|_| "non-UTF-8 path in diff-tree".to_owned())?;
            paths.insert(path.replace('\\', "/"));
        }
        Ok(paths.into_iter().collect())
    }

    pub(in crate::distill_compose) fn inspect_commit(&self, oid: &str) -> Result<CommitInfo> {
        self.inspect_commit_inner(oid, false)
    }

    pub(in crate::distill_compose) fn inspect_commit_allow_root(
        &self,
        oid: &str,
    ) -> Result<CommitInfo> {
        self.inspect_commit_inner(oid, true)
    }

    pub(in crate::distill_compose) fn inspect_commit_inner(
        &self,
        oid: &str,
        allow_root: bool,
    ) -> Result<CommitInfo> {
        valid_oid(oid)?;
        let meta = self.text(&["log", "-n", "1", "--format=%P%x00%s%x00%B", oid])?;
        let mut fields = meta.splitn(3, '\0');
        let parents_str = fields.next().unwrap_or("").trim();
        let subject = fields.next().unwrap_or("").trim().to_owned();
        let body = fields.next().unwrap_or("");
        let mut task_trailers = Vec::new();
        let mut cherry_picked_from = Vec::new();
        for line in body.lines() {
            push_task_trailer_ids(line, &mut task_trailers);
            push_cherry_picked_oids(line, &mut cherry_picked_from);
        }
        let (parent, changed_paths) = match parents_str.split_whitespace().next() {
            Some(p) => {
                valid_oid(p)?;
                let paths = self.diff_paths(p, oid)?;
                (p.to_owned(), paths)
            }
            None if allow_root => {
                let paths: Vec<String> = self
                    .tree_blobs(oid, ".")?
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect();
                (self.zero_oid(oid).to_owned(), paths)
            }
            None => {
                return Err(format!("commit {oid} has no parent commit to diff against"));
            }
        };
        Ok(CommitInfo {
            oid: oid.to_owned(),
            parent,
            subject,
            task_trailers,
            cherry_picked_from,
            changed_paths,
        })
    }

    /// What `base` already carries from earlier distillations: the stream
    /// commits named in `Source-Commits:` and the ids in `Distilled-Task:`
    /// of every distilled commit in `from..base`, i.e. the history a release
    /// base has and the development stream lacks. A release distilled from
    /// main holds those commits as new composed commits, so without this the
    /// next distillation onto that release would apply them a second time.
    pub(in crate::distill_compose) fn distilled_into_base(
        &self,
        from: &str,
        base: &str,
    ) -> Result<DistilledInBase> {
        valid_oid(from)?;
        valid_oid(base)?;
        let range = format!("{from}..{base}");
        let raw = self.bytes(&["log", "--no-merges", "-z", "--format=%B", &range], None)?;
        let mut found = DistilledInBase::default();
        for body in raw.split(|byte| *byte == 0) {
            let body = std::str::from_utf8(body)
                .map_err(|_| "non-UTF-8 commit message in Git log".to_owned())?;
            let mut tasks = Vec::new();
            let mut sources = Vec::new();
            for line in body.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("Distilled-Task:") {
                    tasks.extend(
                        rest.split(|c: char| c == ',' || c.is_whitespace())
                            .filter(|id| !id.is_empty())
                            .map(str::to_ascii_uppercase),
                    );
                } else if let Some(rest) = line.strip_prefix("Source-Commits:") {
                    for oid in rest.split(|c: char| c == ',' || c.is_whitespace()) {
                        if oid.is_empty() {
                            continue;
                        }
                        valid_oid(oid).map_err(|_| {
                            format!("distilled commit in {range} names an invalid source commit '{oid}'")
                        })?;
                        sources.push(oid.to_owned());
                    }
                }
            }
            if !tasks.is_empty() {
                found.tasks.extend(tasks);
                found.source_commits.extend(sources);
            }
        }
        Ok(found)
    }

    pub(in crate::distill_compose) fn non_merge_commits(
        &self,
        base: &str,
        from: &str,
    ) -> Result<Vec<CommitInfo>> {
        valid_oid(base)?;
        valid_oid(from)?;
        let range = format!("{base}..{from}");
        let raw = self.bytes(
            &[
                "log",
                "--topo-order",
                "--reverse",
                "--no-merges",
                "-z",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                "--format=%H%x00%P%x00%s%x00%B",
                &range,
            ],
            None,
        )?;
        parse_batched_commit_stream(&raw)
    }

    pub(in crate::distill_compose) fn merge_three_way(
        &self,
        merge_base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<ThreeWayMerge> {
        use std::sync::atomic::{AtomicBool, Ordering};
        static MERGE_BASE_FLAG_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

        valid_oid(merge_base)?;
        valid_oid(ours)?;
        valid_oid(theirs)?;

        if !MERGE_BASE_FLAG_UNSUPPORTED.load(Ordering::Relaxed) {
            let merge_base_arg = format!("--merge-base={merge_base}");
            let output = self.output(
                &["merge-tree", "--write-tree", &merge_base_arg, ours, theirs],
                None,
            )?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.code() == Some(129) && stderr.contains("merge-base") {
                MERGE_BASE_FLAG_UNSUPPORTED.store(true, Ordering::Relaxed);
            } else {
                return parse_merge_tree_output(output, merge_base, theirs);
            }
        }
        self.merge_three_way_synthetic_base(merge_base, ours, theirs)
    }

    /// Git 2.38–2.39 (`merge-tree --write-tree` without `--merge-base=`): bind the
    /// exact `merge_base`, `ours` and `theirs` trees into a deterministic 3-commit
    /// object-store DAG (`ours <- base -> theirs`) so `merge-ort` resolves against
    /// the identical explicit base tree without touching any index or worktree.
    pub(in crate::distill_compose) fn merge_three_way_synthetic_base(
        &self,
        merge_base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<ThreeWayMerge> {
        let base_tree = self.tree(merge_base)?;
        let ours_tree = self.tree(ours)?;
        let theirs_tree = self.tree(theirs)?;
        let synth_base = self.commit_tree_raw(&base_tree, &[], "distill-merge-base\n")?;
        let synth_ours =
            self.commit_tree_raw(&ours_tree, &[synth_base.as_str()], "distill-ours\n")?;
        let synth_theirs =
            self.commit_tree_raw(&theirs_tree, &[synth_base.as_str()], "distill-theirs\n")?;
        let output = self.output(
            &["merge-tree", "--write-tree", &synth_ours, &synth_theirs],
            None,
        )?;
        parse_merge_tree_output(output, merge_base, theirs)
    }

    pub(in crate::distill_compose) fn commit_tree(
        &self,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String> {
        self.commit_tree_parents(tree, &[parent], message)
    }

    pub(in crate::distill_compose) fn commit_tree_parents(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String> {
        if parents.is_empty() {
            return Err("commit-tree requires at least one parent".to_owned());
        }
        self.commit_tree_raw(tree, parents, message)
    }

    /// A merge published to shared development main records when it was
    /// made: the fixed composition date would misdate main's history (the
    /// host showed a reconcile merge from 11 Nov as made "yesterday").
    pub(in crate::distill_compose) fn commit_tree_dated_now(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String> {
        if parents.is_empty() {
            return Err("commit-tree requires at least one parent".to_owned());
        }
        self.commit_tree_with(tree, parents, message, false)
    }

    /// Composition commits use a fixed date so the same inputs give the same
    /// candidate commit and receipt on every host.
    pub(in crate::distill_compose) fn commit_tree_raw(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String> {
        self.commit_tree_with(tree, parents, message, true)
    }

    pub(in crate::distill_compose) fn commit_tree_with(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
        fixed_date: bool,
    ) -> Result<String> {
        valid_oid(tree)?;
        let mut command = Command::new("git");
        command
            .current_dir(self.root)
            .arg("--no-replace-objects")
            .arg("commit-tree")
            .arg(tree);
        for parent in parents {
            valid_oid(parent)?;
            command.args(["-p", parent]);
        }
        command
            .args(["-F", "-"])
            .env("GIT_AUTHOR_NAME", "Susi Distiller")
            .env("GIT_AUTHOR_EMAIL", "distiller@localhost")
            .env("GIT_COMMITTER_NAME", "Susi Distiller")
            .env("GIT_COMMITTER_EMAIL", "distiller@localhost")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_GRAFT_FILE",
        ] {
            command.env_remove(key);
        }
        if fixed_date {
            command
                .env("GIT_AUTHOR_DATE", "2026-11-10T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-11-10T00:00:00Z");
        } else {
            command
                .env_remove("GIT_AUTHOR_DATE")
                .env_remove("GIT_COMMITTER_DATE");
        }
        #[cfg(test)]
        {
            command
                .env("HOME", self.root)
                .env("USERPROFILE", self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    self.root.join(".absent-fixture-config"),
                );
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start git commit-tree: {e}"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "commit-tree stdin unavailable".to_owned())?
            .write_all(message.as_bytes())
            .map_err(|e| format!("cannot write commit-tree message: {e}"))?;
        let output = child
            .wait_with_output()
            .map_err(|e| format!("cannot wait for commit-tree: {e}"))?;
        if !output.status.success() {
            return Err("cannot create distilled commit object".to_owned());
        }
        let commit = String::from_utf8(output.stdout)
            .map_err(|_| "invalid commit-tree OID".to_owned())?
            .trim()
            .to_owned();
        valid_oid(&commit)?;
        Ok(commit)
    }

    pub(in crate::distill_compose) fn commit_single_blob_envelope(
        &self,
        parent_commit: &str,
        rel_path: &str,
        content: &[u8],
        message: &str,
    ) -> Result<String> {
        valid_oid(parent_commit)?;
        let blob_raw = self.bytes(&["hash-object", "-w", "--stdin"], Some(content))?;
        let blob_oid = String::from_utf8(blob_raw)
            .map_err(|_| "invalid blob OID".to_owned())?
            .trim()
            .to_owned();
        valid_oid(&blob_oid)?;

        let common = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?);
        let tmp_dir = common.join("susi-distill").join("tmp");
        fs::create_dir_all(&tmp_dir).map_err(|e| format!("cannot create temp index dir: {e}"))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_index = tmp_dir.join(format!("envelope-{}-{nanos}.idx", std::process::id()));
        struct TempIndexGuard(PathBuf);
        impl Drop for TempIndexGuard {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _guard = TempIndexGuard(tmp_index.clone());

        let run_with_index = |args: &[&str]| -> Result<String> {
            let mut command = Command::new("git");
            command
                .current_dir(self.root)
                .arg("--literal-pathspecs")
                .arg("--no-replace-objects")
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null());
            for key in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_COMMON_DIR",
                "GIT_OBJECT_DIRECTORY",
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                "GIT_GRAFT_FILE",
            ] {
                command.env_remove(key);
            }
            command.env("GIT_INDEX_FILE", &tmp_index);
            let out = command
                .output()
                .map_err(|e| format!("cannot run git index helper: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "git {} failed ({}): {}",
                    args[0],
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            String::from_utf8(out.stdout)
                .map(|s| s.trim().to_owned())
                .map_err(|_| "non-UTF-8 git index output".to_owned())
        };

        run_with_index(&["read-tree", parent_commit])?;
        let cacheinfo = format!("100644,{blob_oid},{rel_path}");
        run_with_index(&["update-index", "--add", "--cacheinfo", &cacheinfo])?;
        let tree_oid = run_with_index(&["write-tree"])?;
        valid_oid(&tree_oid)?;
        self.commit_tree_parents(&tree_oid, &[parent_commit], message)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::distill_compose) enum ThreeWayMerge {
    Clean(String),
    Conflict(String),
}

pub(in crate::distill_compose) fn parse_merge_tree_output(
    output: Output,
    merge_base: &str,
    theirs: &str,
) -> Result<ThreeWayMerge> {
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8(output.stdout)
                .map_err(|_| "non-UTF-8 merge-tree output".to_owned())?;
            let tree = stdout
                .lines()
                .next()
                .ok_or_else(|| "empty merge-tree output".to_owned())?
                .trim()
                .to_owned();
            valid_oid(&tree)?;
            Ok(ThreeWayMerge::Clean(tree))
        }
        Some(1) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let first_line = stdout.lines().nth(1).unwrap_or("conflicting paths");
            Ok(ThreeWayMerge::Conflict(format!(
                "3-way merge-tree conflict applying {theirs} (base {merge_base}): {first_line}"
            )))
        }
        Some(code) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .next()
                .unwrap_or("unexpected merge-tree failure");
            Err(format!("git merge-tree failed (exit {code}): {detail}"))
        }
        None => Err("git merge-tree terminated by signal".to_owned()),
    }
}

pub(in crate::distill_compose) fn parse_batched_commit_stream(
    raw: &[u8],
) -> Result<Vec<CommitInfo>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let tokens: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let mut commits = Vec::new();
    let mut cursor = 0usize;

    while cursor < tokens.len() {
        let first = std::str::from_utf8(tokens[cursor])
            .map_err(|_| "non-UTF-8 commit header in Git log".to_owned())?
            .trim_start_matches(['\n', '\r'])
            .trim();
        if first.is_empty() {
            cursor += 1;
            continue;
        }
        valid_oid(first)?;
        if cursor + 3 >= tokens.len() {
            return Err(format!("truncated batched Git log header for {first}"));
        }
        let parents_str = std::str::from_utf8(tokens[cursor + 1])
            .map_err(|_| "non-UTF-8 parent list in Git log".to_owned())?
            .trim();
        let subject = std::str::from_utf8(tokens[cursor + 2])
            .map_err(|_| "non-UTF-8 subject in Git log".to_owned())?
            .trim()
            .to_owned();
        let body = std::str::from_utf8(tokens[cursor + 3])
            .map_err(|_| "non-UTF-8 body in Git log".to_owned())?;
        let parent = parents_str
            .split_whitespace()
            .next()
            .ok_or_else(|| format!("commit {first} has no parent commit to diff against"))?
            .to_owned();
        valid_oid(&parent)?;

        let mut task_trailers = Vec::new();
        let mut cherry_picked_from = Vec::new();
        for line in body.lines() {
            push_task_trailer_ids(line, &mut task_trailers);
            push_cherry_picked_oids(line, &mut cherry_picked_from);
        }
        cursor += 4;

        let mut path_set = BTreeSet::new();
        while cursor < tokens.len() {
            let peek = std::str::from_utf8(tokens[cursor])
                .map_err(|_| "non-UTF-8 diff header in Git log".to_owned())?
                .trim_start_matches(['\n', '\r']);
            if !peek.starts_with(':') {
                break;
            }
            if cursor + 1 >= tokens.len() {
                return Err(format!("truncated raw diff path entry in commit {first}"));
            }
            let path = std::str::from_utf8(tokens[cursor + 1])
                .map_err(|_| "non-UTF-8 file path in Git log".to_owned())?
                .replace('\\', "/");
            if !path.is_empty() {
                path_set.insert(path);
            }
            cursor += 2;
        }

        commits.push(CommitInfo {
            oid: first.to_owned(),
            parent,
            subject,
            task_trailers,
            cherry_picked_from,
            changed_paths: path_set.into_iter().collect(),
        });
    }

    Ok(commits)
}

pub(in crate::distill_compose) fn sha256_hex(bytes: &[u8]) -> String {
    crate::common::sha256_hex(bytes)
}

pub(in crate::distill_compose) fn valid_oid(oid: &str) -> Result<()> {
    crate::common::require_valid_oid(oid)
}

/// Adds the task ids named by one `Task:` trailer line. A trailer may name
/// several ids separated by commas and/or whitespace (labelling rule 1 in
/// docs/distillation.md, as `distill lint` reads it); ids are uppercased and
/// kept once each.
pub(in crate::distill_compose) fn push_task_trailer_ids(line: &str, out: &mut Vec<String>) {
    let trimmed = line.trim();
    let Some(prefix) = trimmed.get(..5) else {
        return;
    };
    if !prefix.eq_ignore_ascii_case("task:") {
        return;
    }
    for id in trimmed[5..].split(|c: char| c == ',' || c.is_whitespace()) {
        if id.is_empty() {
            continue;
        }
        let id = id.to_ascii_uppercase();
        if !out.contains(&id) {
            out.push(id);
        }
    }
}

/// Extracts full lowercase Git object IDs from `(cherry picked from commit <oid>)`
/// provenance lines added by `git cherry-pick -x`.
pub(in crate::distill_compose) fn push_cherry_picked_oids(line: &str, out: &mut Vec<String>) {
    let trimmed = line.trim();
    let Some(rest) = trimmed
        .strip_prefix("(cherry picked from commit ")
        .and_then(|s| s.strip_suffix(')'))
    else {
        return;
    };
    let candidate = rest.trim().to_ascii_lowercase();
    if valid_oid(&candidate).is_ok() && !out.contains(&candidate) {
        out.push(candidate);
    }
}

pub(in crate::distill_compose) fn valid_name(name: &str) -> Result<()> {
    crate::common::validate_kebab_name(name, true, "distillation")
}
