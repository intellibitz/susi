//! CoreTools handlers and anti-entropy helpers for quorum commit logs, cluster rekeying, and membership.

use std::path::Path;

use super::CoreTools;
use crate::susi_core::plane_bus::{gawd, gawd_hooks};
use crate::susi_error::{EaiError, EaiResult};
use crate::tools::args;

impl CoreTools {
    crate::tool_def!(commit_record_tool_attr {
        name: "commit_record",
        description: "Accept a replicated swarm commit record. Args: the CommitRecord JSON object. Verifies the coordinator's cluster-key signature and internal consistency before appending to the local ledger — unsigned/forged records fail closed.",
        input_schema: args::typed::<args::CommitRecord>(),
    });
    pub fn commit_record(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("commit_record", &arg.to_string(), workspace)?;
        let record: crate::susi_core::commit_log::CommitRecord =
            serde_json::from_value(arg.clone())
                .map_err(|e| EaiError::protocol(format!("bad commit record: {e}")))?;
        // verify() runs again inside append(); call it here so the error
        // names the failure (forgery vs. ledger IO) rather than the generic
        // append refusal.
        if !record.verify() {
            return Err(EaiError::authorization(
                "commit record failed cluster-key signature or quorum consistency checks",
            ));
        }
        // Member records need coordinator authority, not just a valid
        // HMAC: an evicted node still holds cluster.key, so signature
        // alone cannot authorize roster changes (the rogue-eviction
        // hole). Refuse records sealed by non-members — they stay
        // missing and converge via pull once the coordinator is known.
        if !crate::susi_core::commit_log::member_coordinator_known(&record) {
            return Err(EaiError::authorization(format!(
                "member record coordinator {} is not an explicit member of this roster",
                record.coordinator
            )));
        }
        // Term gate BEFORE append (VC-200-001): a pushed record from a
        // stale term means the coordinator is working from superseded
        // leadership — reject it (Raft's AppendEntries term check).
        // History fills via repair_commit_gap bypass this gate by
        // calling append() directly — terms gate new writes, not the
        // log's past. Only member coordinators may drive term state:
        // an evicted node's forged high term must not adopt into
        // term.json (it would freeze every honest push as Stale).
        let term_note = if !crate::susi_core::commit_log::coordinator_known(&record) {
            String::new()
        } else {
            match crate::susi_core::commit_log::check_term(&record) {
                crate::susi_core::commit_log::TermVerdict::Stale => {
                    return Err(EaiError::protocol(format!(
                        "stale term {} (current {}): coordinator {} is working from superseded leadership",
                        record.term,
                        crate::susi_core::commit_log::load_term().term,
                        record.coordinator
                    )));
                }
                crate::susi_core::commit_log::TermVerdict::Adopted(s) => {
                    format!(" — adopted term {} (leader {})", s.term, s.leader)
                }
                crate::susi_core::commit_log::TermVerdict::LeaderConflict => {
                    format!(
                        " — WARNING: term {} leader conflict ({} vs persisted {})",
                        record.term,
                        record.leader,
                        crate::susi_core::commit_log::load_term().leader
                    )
                }
                crate::susi_core::commit_log::TermVerdict::Current => String::new(),
            }
        };
        // Replication-gap check BEFORE append: a seq that skips ahead of
        // what this node holds means commits from this coordinator were
        // lost in transit — repair by pulling the missing records from
        // the coordinator's own ledger (anti-entropy), then report.
        let held = crate::susi_core::commit_log::load();
        let floor = crate::susi_core::commit_log::snapshot_floor(&record.coordinator);
        let missing = crate::susi_core::commit_log::missing_seqs_floored(
            &held,
            &record.coordinator,
            record.seq,
            floor,
        );
        crate::susi_core::commit_log::append(&record)?;
        let mut out = format!(
            "commit {} accepted ({} / {} voters, seq {}, term {})",
            record.epoch.get(..12).unwrap_or(&record.epoch),
            record.tally,
            record.electorate.len(),
            record.seq,
            record.term
        );
        out.push_str(&term_note);
        if !missing.is_empty() {
            match repair_commit_gap(&record.coordinator, &missing) {
                Ok(repaired) if repaired == missing.len() => {
                    out.push_str(&format!(
                        " — repaired {} missing record(s) from {}",
                        repaired, record.coordinator
                    ));
                }
                Ok(repaired) => {
                    out.push_str(&format!(
                        " — WARNING: replication gap, missing seq {:?} from {} (repaired {})",
                        missing, record.coordinator, repaired
                    ));
                }
                Err(e) => {
                    out.push_str(&format!(
                        " — WARNING: replication gap, missing seq {:?} from {} (repair failed: {})",
                        missing, record.coordinator, e
                    ));
                }
            }
        }
        Ok(out)
    }

    crate::tool_def!(commit_records_tool_attr {
        name: "commit_records",
        description: "Batch intake of replicated commit records for anti-entropy pushes. Args: {records: [CommitRecord, ...]} (max 1000) — every record re-runs signature, coordinator-membership, and term gates; the accepted set appends in one ledger pass, and any remaining sequence gap is repaired from the coordinator once. Returns an applied/skipped/refused summary.",
        input_schema: args::typed::<args::CommitRecords>(),
    });
    pub fn commit_records(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("commit_records", &arg.to_string(), workspace)?;
        let records: Vec<crate::susi_core::commit_log::CommitRecord> = serde_json::from_value(
            arg.get("records")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| EaiError::protocol(format!("bad commit records array: {e}")))?;
        // Bound the batch — an unbounded array is an unbounded
        // signature-verification workload on one RPC.
        const MAX_BATCH: usize = 1000;
        if records.len() > MAX_BATCH {
            return Err(EaiError::protocol(format!(
                "batch of {} exceeds the {MAX_BATCH}-record cap",
                records.len()
            )));
        }
        // Same per-record gates as commit_record: signature, coordinator
        // membership (an evicted node still holds cluster.key), and the
        // AppendEntries term check. Adopted/conflict verdicts ride the
        // same check_term path — only Stale refuses.
        let mut accepted: Vec<crate::susi_core::commit_log::CommitRecord> = Vec::new();
        let mut refused = 0usize;
        for r in &records {
            if !r.verify()
                || !crate::susi_core::commit_log::member_coordinator_known(r)
                || (crate::susi_core::commit_log::coordinator_known(r)
                    && matches!(
                        crate::susi_core::commit_log::check_term(r),
                        crate::susi_core::commit_log::TermVerdict::Stale
                    ))
            {
                refused += 1;
                continue;
            }
            accepted.push(r.clone());
        }
        // One lock + one ledger load for the whole batch — the append
        // path's full validation still runs per record.
        let mut applied = 0usize;
        let mut skipped = 0usize;
        for outcome in crate::susi_core::commit_log::append_many(&accepted) {
            match outcome {
                crate::susi_core::commit_log::AppendOutcome::Applied => applied += 1,
                crate::susi_core::commit_log::AppendOutcome::Skipped => skipped += 1,
                crate::susi_core::commit_log::AppendOutcome::Refused => refused += 1,
            }
        }
        // Gap repair once per coordinator (the single-record path runs it
        // per record — a batch hitting seq jumps would recurse N times).
        // Compute each coordinator's max accepted seq, then pull whatever
        // between it and our high-water is still missing.
        let held = crate::susi_core::commit_log::load();
        let mut max_seq: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for r in &accepted {
            let e = max_seq.entry(r.coordinator.as_str()).or_insert(0);
            *e = (*e).max(r.seq);
        }
        let mut repaired = 0usize;
        for (coordinator, hi) in max_seq {
            let floor = crate::susi_core::commit_log::snapshot_floor(coordinator);
            let missing =
                crate::susi_core::commit_log::missing_seqs_floored(&held, coordinator, hi, floor);
            if !missing.is_empty() {
                repaired += repair_commit_gap(coordinator, &missing).unwrap_or(0);
            }
        }
        // Machine-readable summary — pushers get real counts instead of
        // scraping a human sentence.
        Ok(serde_json::json!({
            "applied": applied,
            "skipped": skipped,
            "refused": refused,
            "repaired": repaired,
        })
        .to_string())
    }

    crate::tool_def!(commit_log_fetch_tool_attr {
        name: "commit_log_fetch",
        description: "Return local commit-ledger records for anti-entropy pulls. Args: {coordinator?: string, from_seq?: N, limit?: N, offset?: N} — up to `limit` (default 500, max 1000) records with seq >= from_seq from that coordinator (or all coordinators), skipping `offset` matches for pagination.",
        input_schema: args::typed::<args::CommitLogFetch>(),
    });
    pub fn commit_log_fetch(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let coordinator = arg.get("coordinator").and_then(|v| v.as_str());
        let from_seq = arg.get("from_seq").and_then(|v| v.as_u64()).unwrap_or(1);
        let limit = arg
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .min(1000) as usize;
        let offset = arg.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        // Serve the compaction archive alongside the live ledger — a
        // peer repairing a gap that spans the snapshot boundary needs
        // records the live file no longer holds.
        let mut records: Vec<crate::susi_core::commit_log::CommitRecord> =
            crate::susi_core::commit_log::load();
        records.extend(crate::susi_core::commit_log::load_from(
            &crate::susi_core::commit_log::archive_path(),
        ));
        let records: Vec<_> = records
            .into_iter()
            .filter(|r| coordinator.is_none_or(|c| r.coordinator == c) && r.seq >= from_seq)
            .skip(offset)
            .take(limit)
            .collect();
        serde_json::to_string(&records)
            .map_err(|e| EaiError::internal(format!("serialize commit records: {e}")))
    }

    crate::tool_def!(cluster_rekey_stage_tool_attr {
        name: "cluster_rekey_stage",
        description: "Prepare phase of cluster-key rotation: stage a next-epoch cluster key delivered with its committed cluster_rekey record. Args: {record: CommitRecord, key_hex: string (64 hex)}. Verifies the record's signature under the current key, requires coordinator authority, checks the pushed key's SHA-256 matches the committed fingerprint, stages it (0600), then appends the record. Does NOT rotate — a separate cluster_rekey_commit call delivers the activate record whose apply activates the staged key.",
        input_schema: args::typed::<args::ClusterRekeyStage>(),
    });
    pub fn cluster_rekey_stage(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // Audit WITHOUT key_hex — the pushed secret must never reach
        // the audit log; the record alone carries its fingerprint.
        gawd_hooks::audit_action(
            "cluster_rekey_stage",
            &arg.get("record")
                .map_or_else(|| arg.to_string(), |r| r.to_string()),
            workspace,
        )?;
        let record: crate::susi_core::commit_log::CommitRecord = serde_json::from_value(
            arg.get("record")
                .cloned()
                .ok_or_else(|| EaiError::protocol("missing 'record' field"))?,
        )
        .map_err(|e| EaiError::protocol(format!("bad commit record: {e}")))?;
        let key_hex = arg
            .get("key_hex")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("missing 'key_hex' field"))?;
        let key_bytes = susi_paths::hex::decode(key_hex)
            .map_err(|e| EaiError::protocol(format!("bad key_hex: {e}")))?;
        if key_bytes.len() != 32 {
            return Err(EaiError::protocol("key_hex must decode to 32 bytes"));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);
        // Epoch check first: a duplicate push post-rotation is a no-op
        // when the record is already held; anything else signed only by
        // the retired key is revoked-key traffic.
        match record.verify_key_epoch() {
            Some(crate::susi_core::commit_log::KeyEpoch::Current) => {}
            Some(crate::susi_core::commit_log::KeyEpoch::Prev) => {
                let held = crate::susi_core::commit_log::load();
                if held.iter().any(|r| r == &record) {
                    return Ok("rekey already applied — nothing to do".to_string());
                }
                return Err(EaiError::authorization(
                    "rekey record verifies only under the retired key — it is not held and cannot be staged post-rotation",
                ));
            }
            None => {
                return Err(EaiError::authorization(
                    "rekey record failed cluster-key signature or consistency checks",
                ));
            }
        }
        if record.kind != crate::susi_core::commit_log::KIND_CLUSTER_REKEY {
            return Err(EaiError::protocol(
                "record is not a cluster_rekey (prepare-phase) record",
            ));
        }
        let Some(fingerprint) = record.rekey_fingerprint() else {
            return Err(EaiError::protocol(
                "record is not a well-formed cluster_rekey record",
            ));
        };
        // Coordinator authority — an evicted member still holds the key
        // and must not be able to rotate the cluster under itself.
        if !crate::susi_core::commit_log::member_coordinator_known(&record) {
            return Err(EaiError::authorization(format!(
                "rekey coordinator {} is not an explicit member of this roster",
                record.coordinator
            )));
        }
        // The pushed key must match the committed fingerprint — the
        // ledger, not the transport, decides which key activates.
        if crate::susi_config::cluster_key::key_fingerprint(&key) != fingerprint {
            return Err(EaiError::protocol(
                "pushed key does not match the committed rekey fingerprint",
            ));
        }
        // Same stale-term gate as commit_record — a coordinator working
        // from superseded leadership must not drive an epoch change.
        if crate::susi_core::commit_log::coordinator_known(&record) {
            if let crate::susi_core::commit_log::TermVerdict::Stale =
                crate::susi_core::commit_log::check_term(&record)
            {
                return Err(EaiError::protocol(format!(
                    "stale term {} (current {}): rekey coordinator is working from superseded leadership",
                    record.term,
                    crate::susi_core::commit_log::load_term().term
                )));
            }
        }
        // Stage, then append — the record commits WHICH key was agreed
        // but does not rotate; activation waits for the commit-phase
        // record. On append failure remove the staged file so a stray
        // next-epoch key cannot linger beside an uncommitted record.
        if !crate::susi_config::cluster_key::stage_key(&key) {
            return Err(EaiError::internal("failed to stage cluster.key.next"));
        }
        if let Err(e) = crate::susi_core::commit_log::append(&record) {
            let _ =
                std::fs::remove_file(susi_paths::SusiDirs::config_dir().join("cluster.key.next"));
            return Err(e);
        }
        // Self-heal: if the commit-phase record already landed (anti-
        // entropy can deliver activate before this stage push), apply
        // fired while nothing was staged and will not re-fire. Activate
        // now so a late stager is not silently stranded on the old key.
        let committed = crate::susi_core::commit_log::load().iter().any(|r| {
            r.kind == crate::susi_core::commit_log::KIND_CLUSTER_REKEY_ACTIVATE
                && r.rekey_fingerprint() == Some(fingerprint)
        });
        if committed && crate::susi_config::cluster_key::activate_staged_key(fingerprint) {
            return Ok(format!(
                "rekey staged and activated — epoch rotated to key {}",
                &fingerprint[..16]
            ));
        }
        Ok(format!(
            "rekey staged — awaiting activate record for epoch {}",
            &fingerprint[..16]
        ))
    }

    crate::tool_def!(cluster_rekey_commit_tool_attr {
        name: "cluster_rekey_commit",
        description: "Commit phase of cluster-key rotation: apply a committed cluster_rekey_activate record. Args: {record: CommitRecord}. Verifies the record's signature (current key — members are still pre-rotation when this arrives), requires coordinator authority, then appends it; the append's apply activates the staged cluster.key.next when its fingerprint matches the committed one. Members that never staged stay on the old epoch.",
        input_schema: args::typed::<args::RecordOnly>(),
    });
    pub fn cluster_rekey_commit(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("cluster_rekey_commit", &arg.to_string(), workspace)?;
        let record: crate::susi_core::commit_log::CommitRecord = serde_json::from_value(
            arg.get("record")
                .cloned()
                .ok_or_else(|| EaiError::protocol("missing 'record' field"))?,
        )
        .map_err(|e| EaiError::protocol(format!("bad commit record: {e}")))?;
        if record.kind != crate::susi_core::commit_log::KIND_CLUSTER_REKEY_ACTIVATE {
            return Err(EaiError::protocol(
                "record is not a cluster_rekey_activate record",
            ));
        }
        // Epoch check first: this record normally verifies under the
        // CURRENT key (we are still pre-rotation when it lands). A
        // Prev-verifying copy is only a no-op when already held —
        // anything else signed by the retired key is revoked traffic.
        match record.verify_key_epoch() {
            Some(crate::susi_core::commit_log::KeyEpoch::Current) => {}
            Some(crate::susi_core::commit_log::KeyEpoch::Prev) => {
                let held = crate::susi_core::commit_log::load();
                if held.iter().any(|r| r == &record) {
                    return Ok("rekey already activated — nothing to do".to_string());
                }
                return Err(EaiError::authorization(
                    "activate record verifies only under the retired key and is not held",
                ));
            }
            None => {
                return Err(EaiError::authorization(
                    "activate record failed cluster-key signature or consistency checks",
                ));
            }
        }
        let Some(fingerprint) = record.rekey_fingerprint() else {
            return Err(EaiError::protocol(
                "activate record carries a malformed fingerprint",
            ));
        };
        // Coordinator authority — an evicted member still holds the key
        // and must not drive an epoch change.
        if !crate::susi_core::commit_log::member_coordinator_known(&record) {
            return Err(EaiError::authorization(format!(
                "rekey coordinator {} is not an explicit member of this roster",
                record.coordinator
            )));
        }
        // Same stale-term gate as commit_record.
        if crate::susi_core::commit_log::coordinator_known(&record) {
            if let crate::susi_core::commit_log::TermVerdict::Stale =
                crate::susi_core::commit_log::check_term(&record)
            {
                return Err(EaiError::protocol(format!(
                    "stale term {} (current {}): rekey coordinator is working from superseded leadership",
                    record.term,
                    crate::susi_core::commit_log::load_term().term
                )));
            }
        }
        crate::susi_core::commit_log::append(&record)?;
        Ok(format!(
            "cluster epoch rotated to key {}",
            &fingerprint[..16]
        ))
    }

    crate::tool_def!(member_propose_tool_attr {
        name: "member_propose",
        description: "Leader-only membership proposal (Raft's leader-proposed configuration-entry rule): a member asks the elected leader to seal and replicate a roster delta. Args: {member: \"node_id@address\", kind?: \"member_add\"|\"member_remove\"|\"member_unban\" (default member_add)}. Refuses unless this node is the currently claimed leader. member_add refuses banned subjects or the leader itself; member_remove requires the subject in the leader's roster; member_unban requires the subject banned. On success the record is sealed, appended, and pushed to the roster including the subject.",
        input_schema: args::typed::<args::MemberPropose>(),
    });
    pub fn member_propose(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("member_propose", &arg.to_string(), workspace)?;
        let member = arg
            .get("member")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("missing 'member' field"))?;
        let kind = arg
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or(crate::susi_core::commit_log::KIND_MEMBER_ADD);
        // Optional on member_add: the subject's Ed25519 pubkey as the
        // proposer's handshake attested it — the committed record then
        // binds the member's signing key on every receiver. `subject_sig`
        // is the subject's own signature over
        // `susi-bind-v1:{id}:{pubkey}` from its v3 pong — intake refuses
        // a pubkey binding without it, so a proposer can never bind a
        // key the subject didn't claim.
        let member_pubkey = arg
            .get("member_pubkey")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject_sig = arg
            .get("subject_sig")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !matches!(
            kind,
            crate::susi_core::commit_log::KIND_MEMBER_ADD
                | crate::susi_core::commit_log::KIND_MEMBER_REMOVE
                | crate::susi_core::commit_log::KIND_MEMBER_UNBAN
        ) {
            return Err(EaiError::protocol(format!(
                "unknown member kind '{kind}' — expected member_add, member_remove, or member_unban"
            )));
        }
        let (id, addr) = member
            .split_once('@')
            .filter(|(i, a)| !i.is_empty() && !a.is_empty())
            .ok_or_else(|| EaiError::protocol("member must be 'node_id@address'"))?;
        // Only the claimed leader may seal configuration changes — the
        // member_coordinator_known gate on intake refuses anything else.
        let term = crate::susi_core::commit_log::load_term();
        let self_id = crate::susi_config::cluster_key::wire_node_id();
        if term.leader != self_id {
            return Err(EaiError::authorization(format!(
                "not the elected leader (leader: {}) — propose to the leader",
                if term.leader.is_empty() {
                    "none yet"
                } else {
                    &term.leader
                }
            )));
        }
        if kind == crate::susi_core::commit_log::KIND_MEMBER_ADD && id == self_id {
            return Err(EaiError::protocol(
                "leader cannot be proposed as a new member — self-edge",
            ));
        }
        // Fails closed on a damaged ban list: an add is refused rather
        // than proposed against a list that may hold this member.
        let is_banned = crate::susi_config::cluster_key::member_banned(|b| {
            crate::susi_config::cluster_key::ban_row_matches(b, id, addr)
        });
        // Electorate = this leader's explicit roster at seal time.
        let roster = gawd::cluster_roster().map_err(|e| {
            crate::susi_error::EaiError::internal(format!("cluster roster unavailable: {e}"))
        })?;
        // A delegated add must not resurrect a banned member — the ban
        // list is local state the proposer may not share.
        if kind == crate::susi_core::commit_log::KIND_MEMBER_ADD && is_banned {
            return Err(EaiError::authorization(format!(
                "{member} is banned — a delegated add cannot lift a committed eviction"
            )));
        }
        // Refuse phantom evictions — a remove for a subject the leader
        // doesn't roster is a wasted committed record.
        if kind == crate::susi_core::commit_log::KIND_MEMBER_REMOVE
            && !roster.iter().any(|(nid, _, _)| nid.as_str() == id)
        {
            return Err(EaiError::protocol(format!(
                "{id} is not in the leader's roster — nothing to remove"
            )));
        }
        if kind == crate::susi_core::commit_log::KIND_MEMBER_UNBAN && !is_banned {
            return Err(EaiError::protocol(format!(
                "{member} is not banned — nothing to unban"
            )));
        }
        // The leader votes in its own electorate — otherwise evicting a
        // dead member would require the dead member's endorsement. The
        // swarm roster lists this node itself (its `Local` entry), so it is
        // taken from there exactly once: a duplicate counts the leader as
        // two bound members, doubles the quorum it needs, and makes every
        // proposal to a standalone leader unsatisfiable.
        let mut electorate: Vec<String> = roster
            .iter()
            .map(|(nid, _, _)| nid.clone())
            .filter(|nid| nid != &self_id)
            .collect();
        electorate.sort();
        electorate.dedup();
        electorate.push(self_id.clone());
        let Some(mut record) = crate::susi_core::commit_log::CommitRecord::seal_member(
            &self_id,
            &self_id,
            kind,
            member,
            electorate,
            member_pubkey,
        ) else {
            return Err(EaiError::internal(
                "no cluster.key — cannot seal a member record",
            ));
        };
        record.subject_sig = subject_sig.to_string();
        // Joint-consensus: collect the bound electorate's endorsements
        // before committing — a bound-quorum requirement is evaluated
        // receiver-side, so a record sealed short of it would be refused
        // everywhere it lands. The leader's member_sig is its own vote;
        // ask the other members.
        let targets: Vec<(String, String)> = roster
            .iter()
            .filter(|(nid, a, _)| nid != &self_id && !a.starts_with("127."))
            .map(|(nid, a, _)| (nid.clone(), a.clone()))
            .collect();
        record.endorsements = crate::susi_core::commit_log::collect_endorsements(&record, &targets);
        if !crate::susi_core::commit_log::endorsements_satisfied(&record) {
            let signed: std::collections::HashSet<&str> = record
                .endorsements
                .iter()
                .map(|e| e.node.as_str())
                .collect();
            let missing: Vec<&str> = targets
                .iter()
                .map(|(nid, _)| nid.as_str())
                .filter(|nid| !signed.contains(nid))
                .collect();
            return Err(EaiError::authorization(format!(
                "insufficient endorsements for {kind} — a majority of the bound electorate must sign (got {} supporter(s)); non-signers: {}; dead members can only be dropped once the quorum question is resolvable",
                record.endorsements.len() + 1,
                if missing.is_empty() {
                    "none (signatures collected but quorum still short)".to_string()
                } else {
                    missing.join(", ")
                }
            )));
        }
        crate::susi_core::commit_log::append(&record)?;
        // Push to the roster plus the subject — the subject is not yet
        // in the leader's roster, and it must learn its own committed
        // membership promptly rather than waiting for anti-entropy.
        let bearer = crate::susi_config::cluster_key::peer_bearer();
        let args = serde_json::to_value(&record)
            .map_err(|e| EaiError::internal(format!("serialize record: {e}")))?;
        let mut pushed = 0usize;
        // Every bound member, not only those the swarm currently counts
        // live: a member reached only over the peer channel never answers a
        // UDP ping, and a committed record it never receives would leave it
        // without the roster change. A push to a dead member just fails.
        let bound_members: Vec<String> =
            crate::susi_config::cluster_key::config_json_rows("peers.json")
                .iter()
                .filter(|r| r.get("admission").and_then(|v| v.as_str()) == Some("explicit"))
                .filter(|r| {
                    r.get("pubkey")
                        .and_then(|v| v.as_str())
                        .is_some_and(|k| !k.is_empty())
                })
                .filter_map(|r| {
                    r.get("address")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect();
        let mut targets: Vec<String> = roster.iter().map(|(_, a, _)| a.clone()).collect();
        for a in bound_members
            .into_iter()
            .chain(std::iter::once(addr.to_string()))
        {
            if !targets.contains(&a) {
                targets.push(a);
            }
        }
        for target in targets {
            if target.starts_with("127.")
                || target.starts_with("::1")
                || target.starts_with("localhost")
            {
                continue;
            }
            if let Ok(result) = crate::susi_core::mcp_client::call_tool(
                &target,
                "commit_record",
                &args,
                bearer.as_deref(),
            ) {
                if result.get("isError").and_then(|v| v.as_bool()) != Some(true) {
                    pushed += 1;
                }
            }
        }
        Ok(format!(
            "{kind} {member} committed (seq {}, term {}) — pushed to {pushed} peer(s)",
            record.seq, record.term
        ))
    }

    crate::tool_def!(member_endorse_tool_attr {
        name: "member_endorse",
        description: "Endorse a sealed privileged record (member/rekey kind) — the joint-consensus vote a leader collects before committing a roster/config delta. Args: {record: <CommitRecord json>}. Verifies the record's HMAC signature under this member's cluster.key and refuses non-privileged kinds, then returns {node, sig} where sig is this node's Ed25519 signature over susi-endorse-v1:{record.signature}. An endorsement is only a vote on that exact record — all intake gates still apply when it lands.",
        input_schema: args::typed::<args::RecordOnly>(),
    });
    pub fn member_endorse(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("member_endorse", &arg.to_string(), workspace)?;
        let rec_val = arg
            .get("record")
            .ok_or_else(|| EaiError::protocol("missing 'record' field"))?;
        let record: crate::susi_core::commit_log::CommitRecord =
            serde_json::from_value(rec_val.clone())
                .map_err(|e| EaiError::protocol(format!("record does not parse: {e}")))?;
        if !crate::susi_core::commit_log::privileged_kind_name(&record.kind) {
            return Err(EaiError::protocol(
                "only privileged (member/rekey) records need endorsements",
            ));
        }
        if !record.verify() {
            return Err(EaiError::protocol(
                "refusing to endorse a record that fails signature or consistency verification",
            ));
        }
        let node = crate::susi_config::cluster_key::wire_node_id();
        let payload = crate::susi_core::commit_log::endorsement_payload(&record.signature);
        let sig = crate::susi_config::cluster_key::member_sign(&payload)
            .ok_or_else(|| EaiError::internal("no node.key — cannot sign an endorsement"))?;
        Ok(serde_json::json!({"node": node, "sig": sig}).to_string())
    }

    crate::tool_def!(cluster_status_tool_attr {
        name: "cluster_status",
        description: "Read-only consensus snapshot for cluster-wide status checks: {node, term, leader, records, heads, key_epoch, roster, banned, bound, pubkey}. `heads` maps each coordinator's node_id to its highest ledger seq + epoch prefix — comparing heads across members exposes ledger divergence directly. No args.",
        input_schema: args::no_args(),
    });
    pub fn cluster_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        use crate::susi_core::commit_log;
        let term = commit_log::load_term();
        let records = commit_log::load();
        let mut heads: std::collections::BTreeMap<String, serde_json::Value> =
            std::collections::BTreeMap::new();
        for r in &records {
            let entry = heads
                .entry(r.coordinator.clone())
                .or_insert_with(|| serde_json::json!({"seq": 0u64, "epoch": ""}));
            if r.seq > entry.get("seq").and_then(|v| v.as_u64()).unwrap_or(0) {
                entry["seq"] = serde_json::json!(r.seq);
                entry["epoch"] = serde_json::json!(r.epoch.get(..12).unwrap_or(&r.epoch));
            }
        }
        let roster = gawd::cluster_roster().unwrap_or_default();
        // Mtime-cached roster rows — this tool answers on every
        // `peers status` sweep.
        let banned = crate::susi_config::cluster_key::config_json_rows("peers_banned.json").len();
        let bound = crate::susi_config::cluster_key::config_json_rows("peers.json")
            .iter()
            .filter(|p| {
                !p.get("pubkey")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .is_empty()
            })
            .count();
        let key_epoch = crate::susi_config::cluster_key::cluster_key()
            .map(|k| crate::susi_config::cluster_key::key_fingerprint(&k))
            .unwrap_or_default();
        Ok(serde_json::json!({
            "node": crate::susi_config::cluster_key::wire_node_id(),
            "term": term.term,
            "leader": term.leader,
            "records": records.len(),
            "heads": heads,
            "key_epoch": key_epoch.get(..12).unwrap_or(&key_epoch),
            "roster": roster.len(),
            "banned": banned,
            "bound": bound,
            "pubkey": crate::susi_config::cluster_key::node_pubkey_hex()
                .map(|p| p.get(..12).unwrap_or(&p).to_string())
                .unwrap_or_default(),
        })
        .to_string())
    }

    crate::tool_def!(commit_log_tool_attr {
        name: "commit_log",
        description:
            "List quorum commit records in the local ledger, newest first. Args: {limit?: N}",
        input_schema: args::typed::<args::CommitLog>(),
    });
    pub fn commit_log(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let limit = arg
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(500) as usize;
        let records = crate::susi_core::commit_log::load();
        let mut lines = vec!["EPOCH\tCOORDINATOR\tTALLY\tQUORUM\tAT".to_string()];
        for r in records.iter().rev().take(limit) {
            lines.push(format!(
                "{}\t{}\t{}\t{}\t{}",
                r.epoch.get(..12).unwrap_or(&r.epoch),
                r.coordinator,
                r.tally,
                r.quorum_threshold,
                r.committed_at
            ));
        }
        Ok(lines.join("\n"))
    }
}

/// Anti-entropy repair for the commit ledger: resolve `coordinator`'s
/// address through the gawd cluster-roster topic, pull the missing seqs
/// via `commit_log_fetch`, verify each record's signature before
/// appending — a compromised peer can never inject unverifiable history,
/// it can only fail to answer. The coordinator is tried first, then
/// every other verified peer: records replicate to all voters, so any
/// member that held a copy can serve the gap. Returns the count of
/// records repaired.
fn repair_commit_gap(coordinator: &str, missing: &[u64]) -> Result<usize, String> {
    // Trust-sorted roster: coordinator first, then the most-trusted
    // fallbacks — every voter holds a replica of what it voted on.
    let peers = gawd::cluster_roster().map_err(|e| format!("cluster roster unavailable: {e}"))?;
    if peers.is_empty() {
        return Err("verified roster is empty".to_string());
    }
    let mut candidates: Vec<&str> = peers
        .iter()
        .filter(|(id, _, _)| id == coordinator)
        .map(|(_, a, _)| a.as_str())
        .collect();
    candidates.extend(
        peers
            .iter()
            .filter(|(id, _, _)| id != coordinator)
            .map(|(_, a, _)| a.as_str()),
    );
    if candidates.is_empty() {
        return Err(format!("coordinator {coordinator} not in verified roster"));
    }

    // Roster candidates are verified members — the member-to-member
    // credential is the cluster-derived peer bearer (the per-host
    // api_token cannot authenticate on a remote node).
    let bearer_owned = crate::susi_config::cluster_key::peer_bearer();
    let bearer = bearer_owned.as_deref();

    let mut last_err = String::new();
    let mut repaired_total = 0usize;
    let mut remaining: Vec<u64> = missing.to_vec();
    for addr in candidates {
        if remaining.is_empty() {
            break;
        }
        match fetch_commit_records(addr, coordinator, remaining[0], bearer) {
            Ok(mut fetched) => {
                // Descending seq order: a prior-epoch record (signed
                // under the retired cluster key) may only append when
                // its seq+1 successor is held and names its epoch — so
                // the successor must land first, or a pre-rotation tail
                // gap takes one sweep per record.
                fetched.sort_by_key(|r| std::cmp::Reverse(r.seq));
                // Member records need coordinator authority — an
                // evicted node still holds cluster.key, so a gap fill
                // must not smuggle a rogue roster delta through.
                let eligible: Vec<crate::susi_core::commit_log::CommitRecord> = fetched
                    .into_iter()
                    .filter(|r| {
                        r.coordinator == coordinator
                            && remaining.contains(&r.seq)
                            && r.verify()
                            && crate::susi_core::commit_log::member_coordinator_known(r)
                    })
                    .collect();
                // Batch intake: one lock + one ledger load for the whole
                // repair instead of a full-ledger parse per record.
                let outcomes = crate::susi_core::commit_log::append_many(&eligible);
                let mut got = 0usize;
                for (r, outcome) in eligible.iter().zip(outcomes) {
                    // Applied OR already-held (Skipped) — the seq is
                    // present either way, so the gap is filled.
                    if outcome != crate::susi_core::commit_log::AppendOutcome::Refused {
                        remaining.retain(|s| *s != r.seq);
                        got += 1;
                    }
                }
                repaired_total += got;
                if got == 0 {
                    last_err = format!("{addr} held none of the missing records");
                }
            }
            Err(e) => last_err = e,
        }
    }
    if repaired_total > 0 {
        return Ok(repaired_total);
    }
    Err(last_err)
}

/// Pull a coordinator's records (seq >= `from_seq`) from one peer's
/// `commit_log_fetch` tool over the MCP peer channel (`mcp_client` runs
/// the session handshake — a bare POST to `/` is rejected by the server).
fn fetch_commit_records(
    addr: &str,
    coordinator: &str,
    from_seq: u64,
    bearer: Option<&str>,
) -> Result<Vec<crate::susi_core::commit_log::CommitRecord>, String> {
    let result = crate::susi_core::mcp_client::call_tool(
        addr,
        "commit_log_fetch",
        &serde_json::json!({ "coordinator": coordinator, "from_seq": from_seq }),
        bearer,
    )
    .map_err(|e| format!("fetch from {addr}: {e}"))?;
    let text = result
        .pointer("/content/0/text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "no tool output in fetch response".to_string())?;
    serde_json::from_str(text).map_err(|e| format!("bad records: {e}"))
}

/// True when a remote tool's `Ok` payload is actually stringified error
/// text — MCP adapters can return `-32603`/protocol failures as content.
/// Display prefixes are checked only near the head so output that merely
/// mentions an error is not misclassified.
pub(crate) fn remote_result_is_error(text: &str) -> bool {
    let t = text.trim_start();
    if t.contains("[FAIL]") {
        return true;
    }
    let head: String = t.chars().take(64).collect();
    head.contains(" Error:")
        || head.contains(" Violation:")
        || head.contains("Mcp error")
        || head.contains("MCP Error")
}
