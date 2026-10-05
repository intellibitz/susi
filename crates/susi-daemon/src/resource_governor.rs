//! Resource governance for background provisioning and mission admission.
//!
//! The percentage-based [`govern`] helper is retained for the daemon's
//! lightweight background tick. [`MissionAdmission`] is the stronger
//! admission boundary: every running mission reserves one vector covering
//! CPU, GPU, memory, and disk, so a leaf worker cannot oversubscribe the host
//! by racing another mission. Queued work ages, which prevents a stream of
//! higher-priority submissions from starving an older request.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernorDecision {
    pub allow: bool,
    pub reason: String,
}

/// Gate a background provision when CPU/mem pressure is high.
#[must_use]
pub fn govern(cpu_pct: f64, mem_pct: f64, is_foreground: bool) -> GovernorDecision {
    if is_foreground {
        return GovernorDecision {
            allow: true,
            reason: "foreground always allowed".into(),
        };
    }
    if cpu_pct > 85.0 || mem_pct > 90.0 {
        return GovernorDecision {
            allow: false,
            reason: "defer under resource pressure".into(),
        };
    }
    GovernorDecision {
        allow: true,
        reason: "within budget".into(),
    }
}

/// Resources reserved by one mission and all of its leaf workers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceDemand {
    /// CPU in millicores. A host with two cores has 2,000 millicores.
    pub cpu_millis: u32,
    /// Whole GPU units, allowing a caller to reserve one or more devices.
    pub gpu_units: u32,
    /// Resident memory in bytes.
    pub memory_bytes: u64,
    /// Scratch/storage reservation in bytes.
    pub disk_bytes: u64,
}

impl ResourceDemand {
    fn fits_in(self, available: Self) -> bool {
        self.cpu_millis <= available.cpu_millis
            && self.gpu_units <= available.gpu_units
            && self.memory_bytes <= available.memory_bytes
            && self.disk_bytes <= available.disk_bytes
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            cpu_millis: self.cpu_millis.checked_add(other.cpu_millis)?,
            gpu_units: self.gpu_units.checked_add(other.gpu_units)?,
            memory_bytes: self.memory_bytes.checked_add(other.memory_bytes)?,
            disk_bytes: self.disk_bytes.checked_add(other.disk_bytes)?,
        })
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            cpu_millis: self.cpu_millis.checked_sub(other.cpu_millis)?,
            gpu_units: self.gpu_units.checked_sub(other.gpu_units)?,
            memory_bytes: self.memory_bytes.checked_sub(other.memory_bytes)?,
            disk_bytes: self.disk_bytes.checked_sub(other.disk_bytes)?,
        })
    }
}

/// Total host capacity and the currently reserved amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceQuota {
    pub capacity: ResourceDemand,
    pub used: ResourceDemand,
}

impl ResourceQuota {
    #[must_use]
    pub fn new(capacity: ResourceDemand) -> Self {
        Self {
            capacity,
            used: ResourceDemand::default(),
        }
    }

    #[must_use]
    pub fn available(self) -> ResourceDemand {
        ResourceDemand {
            cpu_millis: self
                .capacity
                .cpu_millis
                .saturating_sub(self.used.cpu_millis),
            gpu_units: self.capacity.gpu_units.saturating_sub(self.used.gpu_units),
            memory_bytes: self
                .capacity
                .memory_bytes
                .saturating_sub(self.used.memory_bytes),
            disk_bytes: self
                .capacity
                .disk_bytes
                .saturating_sub(self.used.disk_bytes),
        }
    }

    fn reserve(&mut self, demand: ResourceDemand) -> Result<(), AdmissionError> {
        if !demand.fits_in(self.available()) {
            return Err(AdmissionError::InternalAccounting {
                detail: "reservation exceeds available quota".into(),
            });
        }
        self.used =
            self.used
                .checked_add(demand)
                .ok_or_else(|| AdmissionError::InternalAccounting {
                    detail: "resource usage overflow".into(),
                })?;
        Ok(())
    }

    fn release(&mut self, demand: ResourceDemand) -> Result<(), AdmissionError> {
        self.used =
            self.used
                .checked_sub(demand)
                .ok_or_else(|| AdmissionError::InternalAccounting {
                    detail: "resource release exceeds reserved usage".into(),
                })?;
        Ok(())
    }
}

/// A mission's admission request. The demand includes every leaf worker the
/// mission will start, so workers cannot bypass the parent's reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionRequest {
    pub id: String,
    pub priority: u32,
    pub demand: ResourceDemand,
    /// A preemptible mission can be paused and returned to the queue for a
    /// higher-priority request. Non-preemptible work is never evicted.
    pub preemptible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmissionStatus {
    Running,
    Queued,
    Completed,
    Cancelled,
}

/// Result returned from submit/release scheduling, including externally
/// observable queue and preemption information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionOutcome {
    pub id: String,
    pub status: AdmissionStatus,
    pub queue_position: Option<usize>,
    pub started: Vec<String>,
    pub preempted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningMission {
    pub id: String,
    pub priority: u32,
    pub demand: ResourceDemand,
    pub preemptible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMission {
    pub id: String,
    pub priority: u32,
    pub demand: ResourceDemand,
    pub preemptible: bool,
    pub queue_position: usize,
    pub wait_ticks: u64,
    pub preemptions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionSnapshot {
    pub quota: ResourceQuota,
    pub running: Vec<RunningMission>,
    pub queued: Vec<QueuedMission>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    EmptyMissionId,
    DuplicateMission(String),
    DemandExceedsCapacity { id: String, demand: ResourceDemand },
    UnknownMission(String),
    InternalAccounting { detail: String },
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMissionId => write!(f, "mission id must not be empty"),
            Self::DuplicateMission(id) => write!(f, "mission {id:?} is already admitted"),
            Self::DemandExceedsCapacity { id, .. } => {
                write!(f, "mission {id:?} demand exceeds host capacity")
            }
            Self::UnknownMission(id) => write!(f, "mission {id:?} is not running"),
            Self::InternalAccounting { detail } => {
                write!(f, "admission accounting error: {detail}")
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

#[derive(Debug, Clone)]
struct PendingMission {
    request: MissionRequest,
    sequence: u64,
    wait_ticks: u64,
    preemptions: u32,
}

impl PendingMission {
    fn effective_priority(&self) -> u64 {
        u64::from(self.request.priority).saturating_add(self.wait_ticks)
    }
}

#[derive(Debug)]
struct AdmissionState {
    quota: ResourceQuota,
    running: BTreeMap<String, RunningMission>,
    pending: Vec<PendingMission>,
    next_sequence: u64,
}

/// Thread-safe admission controller for missions and their leaf workers.
///
/// All state transitions reserve/release the complete resource vector under
/// one lock. Scheduling scans every pending request that fits instead of
/// blocking a small request behind an oversized head item; wait ticks add to
/// priority so repeatedly arriving high-priority work cannot starve an older
/// mission forever.
pub struct MissionAdmission {
    state: Mutex<AdmissionState>,
}

impl MissionAdmission {
    #[must_use]
    pub fn new(capacity: ResourceDemand) -> Self {
        Self {
            state: Mutex::new(AdmissionState {
                quota: ResourceQuota::new(capacity),
                running: BTreeMap::new(),
                pending: Vec::new(),
                next_sequence: 0,
            }),
        }
    }

    /// Submit work. It starts immediately when possible, otherwise it is
    /// queued. A higher-priority request may preempt lower-priority,
    /// preemptible work; the evicted request remains queued for resumption.
    pub fn submit(&self, request: MissionRequest) -> Result<AdmissionOutcome, AdmissionError> {
        if request.id.trim().is_empty() {
            return Err(AdmissionError::EmptyMissionId);
        }
        let mut state = lock_state(&self.state);
        if state.running.contains_key(&request.id)
            || state
                .pending
                .iter()
                .any(|item| item.request.id == request.id)
        {
            return Err(AdmissionError::DuplicateMission(request.id));
        }
        if !request.demand.fits_in(state.quota.capacity) {
            return Err(AdmissionError::DemandExceedsCapacity {
                id: request.id,
                demand: request.demand,
            });
        }

        let id = request.id.clone();
        let mut preempted = Vec::new();
        if !request.demand.fits_in(state.quota.available()) {
            preempted = preempt_for(&mut state, &request)?;
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.pending.push(PendingMission {
            request,
            sequence,
            wait_ticks: 0,
            preemptions: 0,
        });
        let started = dispatch(&mut state)?;
        Ok(outcome_for(&state, &id, started, preempted))
    }

    /// Release a completed mission and immediately dispatch what now fits.
    pub fn complete(&self, id: &str) -> Result<AdmissionOutcome, AdmissionError> {
        let mut state = lock_state(&self.state);
        let mission = state
            .running
            .remove(id)
            .ok_or_else(|| AdmissionError::UnknownMission(id.to_string()))?;
        state.quota.release(mission.demand)?;
        let started = dispatch(&mut state)?;
        Ok(AdmissionOutcome {
            id: id.to_string(),
            status: AdmissionStatus::Completed,
            queue_position: None,
            started,
            preempted: Vec::new(),
        })
    }

    /// Cancel either running or queued work. Cancellation never preempts a
    /// different mission and returns the queue changes caused by freeing it.
    pub fn cancel(&self, id: &str) -> Result<AdmissionOutcome, AdmissionError> {
        let mut state = lock_state(&self.state);
        if let Some(mission) = state.running.remove(id) {
            state.quota.release(mission.demand)?;
            let started = dispatch(&mut state)?;
            return Ok(AdmissionOutcome {
                id: id.to_string(),
                status: AdmissionStatus::Cancelled,
                queue_position: None,
                started,
                preempted: Vec::new(),
            });
        }
        if let Some(index) = state.pending.iter().position(|item| item.request.id == id) {
            state.pending.remove(index);
            return Ok(AdmissionOutcome {
                id: id.to_string(),
                status: AdmissionStatus::Cancelled,
                queue_position: None,
                started: Vec::new(),
                preempted: Vec::new(),
            });
        }
        Err(AdmissionError::UnknownMission(id.to_string()))
    }

    #[must_use]
    pub fn snapshot(&self) -> AdmissionSnapshot {
        let state = lock_state(&self.state);
        snapshot(&state)
    }
}

fn lock_state(mutex: &Mutex<AdmissionState>) -> MutexGuard<'_, AdmissionState> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn preempt_for(
    state: &mut AdmissionState,
    request: &MissionRequest,
) -> Result<Vec<String>, AdmissionError> {
    let mut candidates: Vec<_> = state
        .running
        .values()
        .filter(|mission| mission.preemptible && mission.priority < request.priority)
        .cloned()
        .collect();
    candidates.sort_by_key(|mission| (mission.priority, mission.id.clone()));
    let mut preempted = Vec::new();
    for mission in candidates {
        if request.demand.fits_in(state.quota.available()) {
            break;
        }
        state.running.remove(&mission.id);
        state.quota.release(mission.demand)?;
        state.pending.push(PendingMission {
            request: MissionRequest {
                id: mission.id.clone(),
                priority: mission.priority,
                demand: mission.demand,
                preemptible: mission.preemptible,
            },
            sequence: state.next_sequence,
            wait_ticks: 0,
            preemptions: 1,
        });
        state.next_sequence = state.next_sequence.saturating_add(1);
        preempted.push(mission.id);
    }
    Ok(preempted)
}

fn dispatch(state: &mut AdmissionState) -> Result<Vec<String>, AdmissionError> {
    for item in &mut state.pending {
        item.wait_ticks = item.wait_ticks.saturating_add(1);
    }
    let mut started = Vec::new();
    while let Some(index) = state
        .pending
        .iter()
        .enumerate()
        .filter(|(_, item)| item.request.demand.fits_in(state.quota.available()))
        .max_by_key(|(_, item)| (item.effective_priority(), std::cmp::Reverse(item.sequence)))
        .map(|(index, _)| index)
    {
        let item = state.pending.remove(index);
        state.quota.reserve(item.request.demand)?;
        let id = item.request.id.clone();
        state.running.insert(
            id.clone(),
            RunningMission {
                id: id.clone(),
                priority: item.request.priority,
                demand: item.request.demand,
                preemptible: item.request.preemptible,
            },
        );
        started.push(id);
    }
    Ok(started)
}

fn ordered_pending(state: &AdmissionState) -> Vec<&PendingMission> {
    let mut pending: Vec<_> = state.pending.iter().collect();
    pending.sort_by_key(|item| (item.effective_priority(), std::cmp::Reverse(item.sequence)));
    pending
}

fn queue_position(state: &AdmissionState, id: &str) -> Option<usize> {
    ordered_pending(state)
        .iter()
        .position(|item| item.request.id == id)
        .map(|position| position + 1)
}

fn outcome_for(
    state: &AdmissionState,
    id: &str,
    started: Vec<String>,
    preempted: Vec<String>,
) -> AdmissionOutcome {
    let status = if state.running.contains_key(id) {
        AdmissionStatus::Running
    } else {
        AdmissionStatus::Queued
    };
    AdmissionOutcome {
        id: id.to_string(),
        status,
        queue_position: queue_position(state, id),
        started,
        preempted,
    }
}

fn snapshot(state: &AdmissionState) -> AdmissionSnapshot {
    let running = state.running.values().cloned().collect();
    let queued = ordered_pending(state)
        .into_iter()
        .enumerate()
        .map(|(position, item)| QueuedMission {
            id: item.request.id.clone(),
            priority: item.request.priority,
            demand: item.request.demand,
            preemptible: item.request.preemptible,
            queue_position: position + 1,
            wait_ticks: item.wait_ticks,
            preemptions: item.preemptions,
        })
        .collect();
    AdmissionSnapshot {
        quota: state.quota,
        running,
        queued,
    }
}

#[cfg(test)]
mod resource_governor_tests {
    use super::*;

    #[test]
    fn resource_governor_defers_under_pressure() {
        assert!(govern(10.0, 10.0, false).allow);
        assert!(!govern(90.0, 50.0, false).allow);
        assert!(govern(99.0, 99.0, true).allow);
    }

    fn demand(
        cpu_millis: u32,
        gpu_units: u32,
        memory_bytes: u64,
        disk_bytes: u64,
    ) -> ResourceDemand {
        ResourceDemand {
            cpu_millis,
            gpu_units,
            memory_bytes,
            disk_bytes,
        }
    }

    fn request(
        id: &str,
        priority: u32,
        resources: ResourceDemand,
        preemptible: bool,
    ) -> MissionRequest {
        MissionRequest {
            id: id.into(),
            priority,
            demand: resources,
            preemptible,
        }
    }

    #[test]
    fn mission_admission_quota_never_oversubscribes_and_reports_preemption() {
        let admission = MissionAdmission::new(demand(2_000, 1, 8, 10));
        let first = admission
            .submit(request("low", 1, demand(1_500, 1, 6, 2), true))
            .expect("first mission fits");
        assert_eq!(first.status, AdmissionStatus::Running);

        let queued = admission
            .submit(request("waiting", 0, demand(1_000, 0, 1, 1), true))
            .expect("waiting mission queues");
        assert_eq!(queued.status, AdmissionStatus::Queued);
        assert_eq!(queued.queue_position, Some(1));

        let urgent = admission
            .submit(request("urgent", 10, demand(1_000, 1, 1, 1), false))
            .expect("urgent mission preempts lower priority work");
        assert_eq!(urgent.status, AdmissionStatus::Running);
        assert_eq!(urgent.preempted, vec!["low"]);
        assert_eq!(urgent.started, vec!["urgent", "waiting"]);

        let snapshot = admission.snapshot();
        assert_eq!(snapshot.quota.used, demand(2_000, 1, 2, 2));
        assert!(snapshot.running.iter().any(|item| item.id == "urgent"));
        assert!(snapshot.running.iter().any(|item| item.id == "waiting"));
        assert_eq!(snapshot.queued.len(), 1);
        assert!(
            snapshot
                .queued
                .iter()
                .any(|item| item.id == "low" && item.preemptions == 1)
        );
    }

    #[test]
    fn mission_admission_quota_ages_low_priority_work_without_starvation() {
        let admission = MissionAdmission::new(demand(1, 0, 1, 1));
        admission
            .submit(request("first", 0, demand(1, 0, 1, 1), false))
            .expect("first mission fits");
        admission
            .submit(request("old", 0, demand(1, 0, 1, 1), true))
            .expect("old mission queues");

        for index in 0..6 {
            let id = format!("new-{index}");
            let current = admission
                .snapshot()
                .running
                .first()
                .map(|mission| mission.id.clone())
                .expect("one mission keeps the slot occupied");
            admission
                .submit(request(&id, 1, demand(1, 0, 1, 1), true))
                .expect("new mission queues");
            admission
                .complete(&current)
                .expect("current mission completes");
            if admission
                .snapshot()
                .running
                .iter()
                .any(|mission| mission.id == "old")
            {
                break;
            }
        }

        let snapshot = admission.snapshot();
        assert!(snapshot.running.iter().any(|item| item.id == "old"));
        assert!(snapshot.queued.iter().all(|item| item.id != "old"));
    }

    #[test]
    fn mission_admission_quota_rejects_a_single_request_that_cannot_fit() {
        let admission = MissionAdmission::new(demand(1, 1, 1, 1));
        let error = admission
            .submit(request("too-large", 1, demand(2, 0, 1, 1), true))
            .expect_err("oversized mission must be rejected");
        assert!(matches!(
            error,
            AdmissionError::DemandExceedsCapacity { .. }
        ));
    }
}
