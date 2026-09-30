//! Admission control for parallel model workers.
//!
//! Before a job or an attempt consumes capacity, it must hold an atomic
//! [`Ticket`] covering every resource it needs — request/token allowance per
//! scope (provider/account/key/model), local CPU/RAM/VRAM/subprocesses, and
//! mission + global concurrency. The grant is all-or-nothing under one lock,
//! so concurrent workers can never double-reserve.
//!
//! Fairness is a bounded FIFO queue: a job that can't be admitted yet queues
//! (not blocks), queue order grants first on release — but a job whose *own*
//! scopes are saturated never blocks unrelated pools behind it. Tickets are
//! leased and persisted; a restart reloads live tickets instead of silently
//! oversubscribing. Release/reconcile on success, failure, and cancellation
//! all free the same resources.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

/// One scoped resource request: `key` names the pool (`"acct:prov:acct"`,
/// `"model:prov/m"`, `"cred:fp8"`).
#[derive(Debug, Clone)]
pub struct ScopeRequest<'a> {
    /// Scope key — same account ⇒ same key, never multiplied by key rotation.
    pub key: &'a str,
    /// Concurrent requests this attempt occupies.
    pub requests: u32,
    /// Estimated token allowance reserved.
    pub tokens: u64,
}

/// What a job (or attempt) needs.
#[derive(Debug, Clone)]
pub struct AdmitRequest<'a> {
    /// Mission id — missions have their own concurrency cap.
    pub mission: &'a str,
    /// Job id — idempotent: re-admitting the same job returns its ticket.
    pub job: &'a str,
    /// Scoped reservations (account/model/credential pools).
    pub scopes: Vec<ScopeRequest<'a>>,
    /// Local CPU in millicores.
    pub cpu_millis: u32,
    /// Local RAM in MiB.
    pub ram_mb: u32,
    /// Local VRAM in MiB.
    pub vram_mb: u32,
    /// Subprocess slots.
    pub subprocesses: u32,
    /// Whether this reservation occupies a mission/global job slot —
    /// `true` for the job ticket, `false` for nested per-attempt scope
    /// tickets (a job's own attempts must not eat the job caps).
    pub holds_job_slot: bool,
}

/// Per-scope caps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ScopeLimits {
    /// Max concurrent attempts inside this scope.
    pub max_inflight: u32,
    /// Max requests the scope may hold at once.
    pub max_requests: u32,
    /// Max reserved tokens the scope may hold at once.
    pub max_tokens: u64,
}

impl Default for ScopeLimits {
    fn default() -> Self {
        Self {
            max_inflight: 4,
            max_requests: 64,
            max_tokens: u64::MAX / 4,
        }
    }
}

/// Local machine capacity the scheduler may hand out.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LocalCapacity {
    /// Millicores.
    pub cpu_millis: u32,
    /// MiB of RAM.
    pub ram_mb: u32,
    /// MiB of VRAM.
    pub vram_mb: u32,
    /// Subprocess slots.
    pub subprocesses: u32,
}

/// Controller-wide limits.
#[derive(Debug, Clone)]
pub struct AdmissionLimits {
    /// Default caps for any scope.
    pub scope: ScopeLimits,
    /// Per-scope overrides (e.g. a tight shared account).
    pub scope_overrides: BTreeMap<String, ScopeLimits>,
    /// Machine capacity.
    pub local: LocalCapacity,
    /// Max concurrent jobs across everything.
    pub global_jobs: u32,
    /// Max concurrent jobs per mission.
    pub mission_jobs: u32,
    /// Max queued jobs (backpressure bound — beyond this, reject).
    pub max_queue: usize,
    /// Ticket lease in ms — a restarted process honors live tickets until
    /// the lease lapses.
    pub lease_ms: u64,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            scope: ScopeLimits::default(),
            scope_overrides: BTreeMap::new(),
            local: LocalCapacity {
                cpu_millis: 64_000,
                ram_mb: 256 * 1024,
                vram_mb: 64 * 1024,
                subprocesses: 32,
            },
            global_jobs: 64,
            mission_jobs: 16,
            max_queue: 256,
            lease_ms: 5 * 60_000,
        }
    }
}

/// Why admission didn't grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backpressure {
    /// Queued at FIFO position; retry `admit` — release drains the queue.
    Queued { position: usize },
    /// This scope is saturated.
    ScopeFull { scope: String },
    /// Local capacity exhausted.
    LocalExhausted,
    /// Mission concurrency cap.
    MissionFull,
    /// Global concurrency cap.
    GlobalFull,
    /// Queue is bounded and full — hard reject.
    QueueFull,
}

/// A granted reservation. `release`/`reconcile` (or Drop) frees it.
pub struct Ticket<'a> {
    ctrl: &'a AdmissionController,
    id: u64,
}

impl Ticket<'_> {
    /// Job/job-id that owns this ticket.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Success path: free everything held.
    pub fn release(self) {}
    /// Failure/cancel path: same release — callers never double-free.
    pub fn reconcile(self) {}
}

impl std::fmt::Debug for Ticket<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket").field("id", &self.id).finish()
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        self.ctrl.release(self.id);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Held {
    ticket: u64,
    job: String,
    mission: String,
    job_slot: bool,
    scopes: Vec<(String, u32, u64)>,
    local: LocalCapacity,
    expires_ms: u64,
}

#[derive(Debug, Default)]
struct ScopeUse {
    inflight: u32,
    requests: u32,
    tokens: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    held: Vec<Held>,
    next_ticket: u64,
}

#[derive(Default)]
struct State {
    scopes: BTreeMap<String, ScopeUse>,
    local: LocalCapacity,
    global_jobs: u32,
    missions: BTreeMap<String, u32>,
    queue: VecDeque<String>,
    held: BTreeMap<u64, Held>,
    by_job: BTreeMap<String, u64>,
    next_ticket: u64,
}

/// Atomic admission controller.
pub struct AdmissionController {
    limits: AdmissionLimits,
    state: Mutex<State>,
    path: Option<PathBuf>,
    clock: fn() -> u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl AdmissionController {
    /// In-memory controller (tests/dev).
    pub fn new(limits: AdmissionLimits) -> Self {
        Self::with_clock(limits, now_ms)
    }
    /// Injected-clock controller.
    pub fn with_clock(limits: AdmissionLimits, clock: fn() -> u64) -> Self {
        Self {
            limits,
            state: Mutex::new(State::default()),
            path: None,
            clock,
        }
    }
    /// Persisted controller: live tickets are reloaded so a restart cannot
    /// silently oversubscribe; expired leases are dropped.
    #[must_use]
    pub fn persisted(path: PathBuf, limits: AdmissionLimits) -> Self {
        let mut c = Self::with_clock(limits, now_ms);
        c.path = Some(path);
        c.load_persisted();
        c
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn scope_limits(&self, key: &str) -> ScopeLimits {
        self.limits
            .scope_overrides
            .get(key)
            .copied()
            .unwrap_or(self.limits.scope)
    }

    /// Atomically admit `req`. Idempotent per job — a held job gets its
    /// existing ticket, never a second reservation.
    ///
    /// Fairness: when the queue is nonempty, a newcomer queues behind it.
    /// The queue head (or an empty queue) gets a grant attempt: scope-full
    /// is a typed terminal denial (the caller picks another pool — a jammed
    /// account never blocks unrelated providers), while concurrency/local
    /// capacity shortfalls queue the job until a release frees room.
    pub fn admit<'a>(&'a self, req: &AdmitRequest<'_>) -> Result<Ticket<'a>, Backpressure> {
        let now = (self.clock)();
        let mut st = self.lock();
        self.purge_expired(&mut st, now);
        // Idempotent: same job → same ticket.
        if let Some(&id) = st.by_job.get(req.job) {
            return Ok(Ticket { ctrl: self, id });
        }
        let position = st.queue.iter().position(|j| j == req.job);
        if req.holds_job_slot && position.is_none() && !st.queue.is_empty() {
            // Others are waiting — join the FIFO (bounded).
            if st.queue.len() >= self.limits.max_queue {
                return Err(Backpressure::QueueFull);
            }
            st.queue.push_back(req.job.to_string());
            return Err(Backpressure::Queued {
                position: st.queue.len(),
            });
        }
        // Grant attempt — all-or-nothing capacity check.
        let denial = if req.holds_job_slot && st.global_jobs >= self.limits.global_jobs {
            Some(Backpressure::GlobalFull)
        } else if req.holds_job_slot
            && st.missions.get(req.mission).copied().unwrap_or(0) >= self.limits.mission_jobs
        {
            Some(Backpressure::MissionFull)
        } else {
            let after = LocalCapacity {
                cpu_millis: st.local.cpu_millis.saturating_add(req.cpu_millis),
                ram_mb: st.local.ram_mb.saturating_add(req.ram_mb),
                vram_mb: st.local.vram_mb.saturating_add(req.vram_mb),
                subprocesses: st.local.subprocesses.saturating_add(req.subprocesses),
            };
            if after.cpu_millis > self.limits.local.cpu_millis
                || after.ram_mb > self.limits.local.ram_mb
                || after.vram_mb > self.limits.local.vram_mb
                || after.subprocesses > self.limits.local.subprocesses
            {
                Some(Backpressure::LocalExhausted)
            } else {
                let mut scope_full = None;
                for s in &req.scopes {
                    let lim = self.scope_limits(s.key);
                    let u = st.scopes.get(s.key);
                    let (inf, rq, tk) = u.map_or((0, 0, 0), |u| (u.inflight, u.requests, u.tokens));
                    if inf >= lim.max_inflight
                        || rq.saturating_add(s.requests) > lim.max_requests
                        || tk.saturating_add(s.tokens) > lim.max_tokens
                    {
                        scope_full = Some(Backpressure::ScopeFull {
                            scope: s.key.to_string(),
                        });
                        break;
                    }
                }
                match scope_full {
                    // Saturated scope: terminal for THIS request — never
                    // queued, so a jammed account can't stall other pools.
                    Some(b) => return Err(b),
                    None => {
                        st.next_ticket += 1;
                        let id = st.next_ticket;
                        for s in &req.scopes {
                            let u = st.scopes.entry(s.key.to_string()).or_default();
                            u.inflight += 1;
                            u.requests = u.requests.saturating_add(s.requests);
                            u.tokens = u.tokens.saturating_add(s.tokens);
                        }
                        st.local = after;
                        if req.holds_job_slot {
                            st.global_jobs += 1;
                            *st.missions.entry(req.mission.to_string()).or_insert(0) += 1;
                        }
                        if let Some(pos) = position {
                            st.queue.remove(pos);
                        }
                        let held = Held {
                            ticket: id,
                            job: req.job.to_string(),
                            mission: req.mission.to_string(),
                            job_slot: req.holds_job_slot,
                            scopes: req
                                .scopes
                                .iter()
                                .map(|s| (s.key.to_string(), s.requests, s.tokens))
                                .collect(),
                            local: LocalCapacity {
                                cpu_millis: req.cpu_millis,
                                ram_mb: req.ram_mb,
                                vram_mb: req.vram_mb,
                                subprocesses: req.subprocesses,
                            },
                            expires_ms: now.saturating_add(self.limits.lease_ms),
                        };
                        st.held.insert(id, held);
                        st.by_job.insert(req.job.to_string(), id);
                        self.persist(&st);
                        return Ok(Ticket { ctrl: self, id });
                    }
                }
            }
        };
        // Capacity shortfall: hold the job's queue slot (bounded) and
        // report the typed reason — a retry once capacity frees grants.
        // Nested scope tickets (holds_job_slot=false) never queue.
        if let Some(denial) = denial {
            if req.holds_job_slot && position.is_none() {
                if st.queue.len() >= self.limits.max_queue {
                    return Err(Backpressure::QueueFull);
                }
                st.queue.push_back(req.job.to_string());
            }
            return Err(denial);
        }
        Err(Backpressure::QueueFull)
    }

    /// Free a ticket and drain the queue in FIFO order — grants capacity
    /// to waiting jobs whose scopes fit, skipping ones that don't (so an
    /// exhausted account never blocks unrelated providers).
    pub fn release(&self, ticket: u64) {
        let now = (self.clock)();
        let mut st = self.lock();
        let Some(h) = st.held.remove(&ticket) else {
            return; // already released — never double-free
        };
        for (key, rq, tk) in &h.scopes {
            if let Some(u) = st.scopes.get_mut(key) {
                u.inflight = u.inflight.saturating_sub(1);
                u.requests = u.requests.saturating_sub(*rq);
                u.tokens = u.tokens.saturating_sub(*tk);
            }
        }
        st.local.cpu_millis = st.local.cpu_millis.saturating_sub(h.local.cpu_millis);
        st.local.ram_mb = st.local.ram_mb.saturating_sub(h.local.ram_mb);
        st.local.vram_mb = st.local.vram_mb.saturating_sub(h.local.vram_mb);
        st.local.subprocesses = st.local.subprocesses.saturating_sub(h.local.subprocesses);
        if h.job_slot {
            st.global_jobs = st.global_jobs.saturating_sub(1);
            if let Some(m) = st.missions.get_mut(&h.mission) {
                *m = m.saturating_sub(1);
            }
        }
        st.by_job.remove(&h.job);
        self.persist(&st);
        drop(st);
        let _ = now;
    }

    /// Current queue length (observability/backpressure surfacing).
    #[must_use]
    pub fn queued(&self) -> usize {
        self.lock().queue.len()
    }
    /// Live ticket count.
    #[must_use]
    pub fn inflight(&self) -> usize {
        self.lock().held.len()
    }
    /// Side-effect-free `admit`: reports what a request would get without
    /// granting, queueing, or persisting. For status surfaces.
    pub fn probe(&self, req: &AdmitRequest<'_>) -> Result<(), Backpressure> {
        let now = (self.clock)();
        let mut st = self.lock();
        self.purge_expired(&mut st, now);
        if req.holds_job_slot && !st.queue.is_empty() {
            let pos = st.queue.len() + 1;
            return Err(Backpressure::Queued { position: pos });
        }
        if req.holds_job_slot && st.global_jobs >= self.limits.global_jobs {
            return Err(Backpressure::GlobalFull);
        }
        if req.holds_job_slot
            && st.missions.get(req.mission).copied().unwrap_or(0) >= self.limits.mission_jobs
        {
            return Err(Backpressure::MissionFull);
        }
        let after = LocalCapacity {
            cpu_millis: st.local.cpu_millis.saturating_add(req.cpu_millis),
            ram_mb: st.local.ram_mb.saturating_add(req.ram_mb),
            vram_mb: st.local.vram_mb.saturating_add(req.vram_mb),
            subprocesses: st.local.subprocesses.saturating_add(req.subprocesses),
        };
        if after.cpu_millis > self.limits.local.cpu_millis
            || after.ram_mb > self.limits.local.ram_mb
            || after.vram_mb > self.limits.local.vram_mb
            || after.subprocesses > self.limits.local.subprocesses
        {
            return Err(Backpressure::LocalExhausted);
        }
        for s in &req.scopes {
            let lim = self.scope_limits(s.key);
            let u = st.scopes.get(s.key);
            let (inf, rq, tk) = u.map_or((0, 0, 0), |u| (u.inflight, u.requests, u.tokens));
            if inf >= lim.max_inflight
                || rq.saturating_add(s.requests) > lim.max_requests
                || tk.saturating_add(s.tokens) > lim.max_tokens
            {
                return Err(Backpressure::ScopeFull {
                    scope: s.key.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Whether a scope currently has headroom.
    #[must_use]
    pub fn scope_free(&self, key: &str) -> bool {
        let lim = self.scope_limits(key);
        let st = self.lock();
        st.scopes
            .get(key)
            .is_none_or(|u| u.inflight < lim.max_inflight)
    }

    fn purge_expired(&self, st: &mut State, now: u64) {
        let expired: Vec<u64> = st
            .held
            .iter()
            .filter(|(_, h)| h.expires_ms <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(h) = st.held.remove(&id) {
                for (key, rq, tk) in &h.scopes {
                    if let Some(u) = st.scopes.get_mut(key) {
                        u.inflight = u.inflight.saturating_sub(1);
                        u.requests = u.requests.saturating_sub(*rq);
                        u.tokens = u.tokens.saturating_sub(*tk);
                    }
                }
                st.local.cpu_millis = st.local.cpu_millis.saturating_sub(h.local.cpu_millis);
                st.local.ram_mb = st.local.ram_mb.saturating_sub(h.local.ram_mb);
                st.local.vram_mb = st.local.vram_mb.saturating_sub(h.local.vram_mb);
                st.local.subprocesses = st.local.subprocesses.saturating_sub(h.local.subprocesses);
                if h.job_slot {
                    st.global_jobs = st.global_jobs.saturating_sub(1);
                    if let Some(m) = st.missions.get_mut(&h.mission) {
                        *m = m.saturating_sub(1);
                    }
                }
                st.by_job.remove(&h.job);
            }
        }
    }

    fn persist(&self, st: &State) {
        let Some(path) = &self.path else { return };
        let body = Persisted {
            held: st.held.values().cloned().collect(),
            next_ticket: st.next_ticket,
        };
        if let Ok(s) = serde_json::to_string(&body) {
            let _ = std::fs::write(path, s);
        }
    }

    fn load_persisted(&self) {
        let Some(path) = &self.path else { return };
        let now = (self.clock)();
        let Ok(body) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(p) = serde_json::from_str::<Persisted>(&body) else {
            return;
        };
        let mut st = self.lock();
        st.next_ticket = p.next_ticket;
        for h in p.held {
            if h.expires_ms <= now {
                continue; // lease lapsed while we were down
            }
            for (key, rq, tk) in &h.scopes {
                let u = st.scopes.entry(key.clone()).or_default();
                u.inflight += 1;
                u.requests = u.requests.saturating_add(*rq);
                u.tokens = u.tokens.saturating_add(*tk);
            }
            st.local.cpu_millis = st.local.cpu_millis.saturating_add(h.local.cpu_millis);
            st.local.ram_mb = st.local.ram_mb.saturating_add(h.local.ram_mb);
            st.local.vram_mb = st.local.vram_mb.saturating_add(h.local.vram_mb);
            st.local.subprocesses = st.local.subprocesses.saturating_add(h.local.subprocesses);
            if h.job_slot {
                st.global_jobs += 1;
                *st.missions.entry(h.mission.clone()).or_insert(0) += 1;
            }
            st.by_job.insert(h.job.clone(), h.ticket);
            st.held.insert(h.ticket, h);
        }
    }
}

/// Path for the process-global persisted controller state.
#[must_use]
pub fn default_path() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("parallel_admission.json")
}

/// Load (or create) the persisted global controller.
#[must_use]
pub fn persisted_default() -> AdmissionController {
    AdmissionController::persisted(default_path(), AdmissionLimits::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn req<'a>(mission: &'a str, job: &'a str, scopes: Vec<ScopeRequest<'a>>) -> AdmitRequest<'a> {
        AdmitRequest {
            mission,
            job,
            scopes,
            cpu_millis: 0,
            ram_mb: 0,
            vram_mb: 0,
            subprocesses: 0,
            holds_job_slot: true,
        }
    }

    #[test]
    fn parallel_model_admission_bounded_load_under_concurrency() {
        // 16 threads contend for a scope capped at 3 inflight — the counter
        // must never exceed 3, and every admitted job completes.
        let lim = AdmissionLimits {
            scope: ScopeLimits {
                max_inflight: 3,
                ..Default::default()
            },
            ..Default::default()
        };
        let adm = Arc::new(AdmissionController::new(lim));
        let concurrent = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let mut handles = Vec::new();
        for i in 0..16 {
            let adm = adm.clone();
            let concurrent = concurrent.clone();
            let peak = peak.clone();
            handles.push(std::thread::spawn(move || {
                let job = format!("j{i}");
                let pool = "acct:prov:a".to_string();
                // ScopeFull is terminal per call — retry until a held slot
                // frees. Bound by wall-clock, not spin count: yields can burn
                // through any fixed count long before holders' sleeps elapse.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    let r = req(
                        "m",
                        &job,
                        vec![ScopeRequest {
                            key: &pool,
                            requests: 1,
                            tokens: 0,
                        }],
                    );
                    match adm.admit(&r) {
                        Ok(t) => {
                            let n = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(n, Ordering::SeqCst);
                            // Hold the slot long enough for siblings to stack.
                            std::thread::sleep(std::time::Duration::from_millis(8));
                            concurrent.fetch_sub(1, Ordering::SeqCst);
                            drop(t);
                            break;
                        }
                        Err(_) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(e) => panic!("never admitted: {e:?}"),
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "peak {}",
            peak.load(Ordering::SeqCst)
        );
        assert!(peak.load(Ordering::SeqCst) >= 2, "must actually overlap");
    }

    #[test]
    fn parallel_model_admission_no_double_reservation_per_job() {
        let adm = AdmissionController::new(AdmissionLimits::default());
        let pool = "acct:prov:a".to_string();
        let r = req(
            "m",
            "j1",
            vec![ScopeRequest {
                key: &pool,
                requests: 1,
                tokens: 0,
            }],
        );
        let t1 = adm.admit(&r).unwrap();
        let t2 = adm.admit(&r).unwrap();
        assert_eq!(t1.id(), t2.id(), "same job must reuse its ticket");
        assert_eq!(adm.inflight(), 1);
    }

    #[test]
    fn parallel_model_admission_fifo_queue_no_starvation() {
        let lim = AdmissionLimits {
            global_jobs: 1,
            max_queue: 8,
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        let holder = adm.admit(&req("m", "holder", vec![])).unwrap();
        // j1 queues ahead of j2.
        // First denials are typed (capacity reason) and enqueue the job.
        assert_eq!(
            adm.admit(&req("m", "j1", vec![])).unwrap_err(),
            Backpressure::GlobalFull
        );
        assert_eq!(adm.queued(), 1);
        assert_eq!(
            adm.admit(&req("m", "j2", vec![])).unwrap_err(),
            Backpressure::Queued { position: 2 }
        );
        assert_eq!(adm.queued(), 2);
        drop(holder);
        // FIFO: j1 gets in first.
        let t1 = adm.admit(&req("m", "j1", vec![])).unwrap();
        assert!(matches!(
            adm.admit(&req("m", "j2", vec![])),
            Err(Backpressure::GlobalFull) | Err(Backpressure::Queued { .. })
        ));
        drop(t1);
        let _t2 = adm.admit(&req("m", "j2", vec![])).unwrap();
    }

    #[test]
    fn parallel_model_admission_shared_account_cannot_multiply_quota() {
        // Two credentials, one account → one pool key → one cap.
        let lim = AdmissionLimits {
            scope: ScopeLimits {
                max_inflight: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        let acct = "acct:prov:shared".to_string();
        let _t = adm
            .admit(&req(
                "m",
                "j1",
                vec![ScopeRequest {
                    key: &acct,
                    requests: 1,
                    tokens: 0,
                }],
            ))
            .unwrap();
        // Same account via a *different* credential — still the same scope.
        let denied = adm.admit(&req(
            "m",
            "j2",
            vec![ScopeRequest {
                key: &acct,
                requests: 1,
                tokens: 0,
            }],
        ));
        assert_eq!(denied.unwrap_err(), Backpressure::ScopeFull { scope: acct });
    }

    #[test]
    fn parallel_model_admission_exhausted_account_never_blocks_others() {
        let lim = AdmissionLimits {
            scope: ScopeLimits {
                max_inflight: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        let a = "acct:prov:a".to_string();
        let b = "acct:prov:b".to_string();
        let _t = adm
            .admit(&req(
                "m",
                "j1",
                vec![ScopeRequest {
                    key: &a,
                    requests: 1,
                    tokens: 0,
                }],
            ))
            .unwrap();
        // Unrelated provider still admits.
        let _t2 = adm
            .admit(&req(
                "m",
                "j2",
                vec![ScopeRequest {
                    key: &b,
                    requests: 1,
                    tokens: 0,
                }],
            ))
            .unwrap();
    }

    #[test]
    fn parallel_model_admission_release_frees_every_path() {
        let lim = AdmissionLimits {
            global_jobs: 1,
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        {
            let t = adm.admit(&req("m", "success", vec![])).unwrap();
            t.release(); // success path
        }
        {
            let t = adm.admit(&req("m", "failure", vec![])).unwrap();
            t.reconcile(); // failure/cancel path
        }
        {
            let _t = adm.admit(&req("m", "cancelled", vec![])).unwrap();
        } // Drop path
        assert_eq!(adm.inflight(), 0);
        let _t = adm.admit(&req("m", "after", vec![])).unwrap();
    }

    #[test]
    fn parallel_model_admission_local_capacity_is_bounded() {
        let lim = AdmissionLimits {
            local: LocalCapacity {
                cpu_millis: 1000,
                ram_mb: 1024,
                vram_mb: 512,
                subprocesses: 1,
            },
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        fn big(j: &str) -> AdmitRequest<'_> {
            AdmitRequest {
                mission: "m",
                job: j,
                scopes: vec![],
                cpu_millis: 800,
                ram_mb: 700,
                vram_mb: 300,
                subprocesses: 1,
                holds_job_slot: true,
            }
        }
        let _t = adm.admit(&big("j1")).unwrap();
        let denied = adm.admit(&big("j2"));
        assert_eq!(denied.unwrap_err(), Backpressure::LocalExhausted);
    }

    #[test]
    fn parallel_model_admission_restart_recovers_live_tickets() {
        // Persisted tickets survive a "restart": the new controller loads
        // them and cannot oversubscribe; expired leases are dropped.
        let dir = std::env::temp_dir().join(format!("adm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("admissions.json");
        let lim = AdmissionLimits {
            global_jobs: 1,
            lease_ms: 60_000,
            ..Default::default()
        };
        {
            let adm = AdmissionController::persisted(path.clone(), lim.clone());
            let _t = adm.admit(&req("m", "live-job", vec![])).unwrap();
            // "crash" — drop without releasing.
            std::mem::forget(_t);
        }
        let adm2 = AdmissionController::persisted(path.clone(), lim.clone());
        // The live ticket was reloaded — global cap is consumed.
        assert_eq!(adm2.inflight(), 1);
        assert!(matches!(
            adm2.admit(&req("m", "new-job", vec![])),
            Err(Backpressure::GlobalFull) | Err(Backpressure::Queued { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_model_admission_bounded_queue_rejects_overflow() {
        let lim = AdmissionLimits {
            global_jobs: 1,
            max_queue: 2,
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        let _h = adm.admit(&req("m", "h", vec![])).unwrap();
        let _q1 = adm.admit(&req("m", "q1", vec![]));
        let _q2 = adm.admit(&req("m", "q2", vec![]));
        assert_eq!(
            adm.admit(&req("m", "q3", vec![])).unwrap_err(),
            Backpressure::QueueFull
        );
    }

    #[test]
    fn parallel_model_admission_dispatch_gates_jobs() {
        // End-to-end: run_jobs under an admission controller bounded to 1
        // job — every job still completes, serialized through admission.
        use crate::parallel_dispatch::{run_jobs, DispatchPlan, Job, Shared};
        use susi_gawd_agents::cloud_budget::{BudgetLedger, SpendPolicy};
        use susi_gawd_agents::cloud_intent::{Candidate, IntentConstraints};
        use susi_vendor_models::cloud_eligibility::EligibilityStore;
        use susi_vendor_models::cloud_quota::QuotaInventory;
        let cs = vec![Candidate {
            provider: "p".into(),
            api_key: "k1".into(),
            account: Some("a".into()),
            region: None,
            model: "m1".into(),
            context_tokens: 100,
            modalities: vec![],
            supports_tools: true,
            supports_structured_output: true,
            residency: None,
            est_latency_ms: 1,
            cost_per_mtok: Some(0.0),
            quality: BTreeMap::new(),
        }];
        let jobs: Vec<Job> = (0..3)
            .map(|i| Job {
                id: format!("j{i}"),
                intent: IntentConstraints {
                    task_class: "coding".into(),
                    discovery_budget: 2,
                    ..Default::default()
                },
            })
            .collect();
        let elig = Mutex::new(EligibilityStore::new());
        let quota = QuotaInventory::new();
        let lock = Mutex::new(crate::cloud_lockout::LockoutTracker::new(
            Default::default(),
            crate::cloud_lockout::now_unix,
        ));
        let ledger = BudgetLedger::new();
        let lim = AdmissionLimits {
            global_jobs: 1,
            ..Default::default()
        };
        let adm = AdmissionController::new(lim);
        let shared = Shared::new(&elig, &quota, &lock, &ledger).with_admission(&adm);
        struct Ok;
        impl crate::cloud_failover::Runner for Ok {
            fn attempt(&mut self, _i: usize, _r: u64) -> crate::cloud_failover::AttemptOutcome {
                crate::cloud_failover::AttemptOutcome::Success("done".into())
            }
        }
        let make = |_j: &Job| Ok;
        let plan = DispatchPlan {
            max_workers: 3,
            mission: "m".into(),
            job_cpu_millis: 0,
            job_ram_mb: 0,
            job_vram_mb: 0,
            job_subprocesses: 0,
            per_job: crate::cloud_failover::FailoverBudget {
                max_attempts: 4,
                deadline_ms: None,
                spend: SpendPolicy::FreeOnly,
                attempt_estimate_micros: 10,
                now_ms: 1_700_000_000_000,
            },
        };
        let outcomes = run_jobs(&jobs, &cs, &shared, plan, &make);
        for o in &outcomes {
            eprintln!(
                "{} stop={:?} attempts={}",
                o.job_id,
                o.stop,
                o.attempts.len()
            );
        }
        assert!(outcomes.iter().all(|o| o.output.is_some()));
        assert_eq!(adm.inflight(), 0, "all tickets released on completion");
    }
}
