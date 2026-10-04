//! Production-entry regressions for live swarm admission (VC-201-024).

use crate::capacity_admission::{CapacityProbe, LiveAdmission};
use crate::dag::MissionDag;
use crate::resource_schedule::{DagNode, Resources};
use std::sync::{Arc, Mutex};

fn inventory(
    cpu: f64,
    gpu_mem_gb: f64,
    mem_gb: f64,
    subprocesses: u32,
    model_ready: bool,
) -> Resources {
    Resources {
        cpu,
        gpu_mem_gb,
        mem_gb,
        subprocesses,
        model_ready,
        tool_grants: vec!["exec_command".into()],
    }
}

fn node(id: &str, cpu: f64, gpu_mem_gb: f64, mem_gb: f64, subprocesses: u32) -> DagNode {
    DagNode {
        id: id.into(),
        cpu,
        gpu_mem_gb,
        mem_gb,
        subprocesses,
        needs_model: true,
        needs_tools: vec!["exec_command".into()],
    }
}

struct MutableProbe(Arc<Mutex<Resources>>);

impl CapacityProbe for MutableProbe {
    fn measure(&self) -> Resources {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[test]
fn swarm_gap_live_admission_enforces_measured_resource_caps_and_releases() {
    let admission = LiveAdmission::fixed(inventory(2.0, 4.0, 4.0, 2, true));
    let first_node = node("n0", 1.0, 3.0, 2.0, 1);
    let second_node = node("n1", 1.0, 2.0, 2.0, 1);

    let first = admission.admit_batch("mission-a", &[(0, first_node), (1, second_node.clone())]);
    assert_eq!(first.admitted_indices(), vec![0]);
    assert_eq!(first.deferred, vec![1]);
    assert_eq!(admission.active(), 1);
    assert_eq!(admission.queued(), 1);

    drop(first);
    let drained = admission.admit_batch("mission-a", &[(1, second_node)]);
    assert_eq!(drained.admitted_indices(), vec![1]);
    assert!(drained.blocked.is_none());
    drop(drained);
    assert_eq!(admission.active(), 0);
    assert_eq!(admission.queued(), 0);
}

#[test]
fn swarm_gap_live_admission_unavailable_model_and_gpu_stay_queued() {
    let state = Arc::new(Mutex::new(inventory(2.0, 0.0, 4.0, 1, false)));
    let admission = LiveAdmission::new(Arc::new(MutableProbe(Arc::clone(&state))));
    let request = node("gpu-specialist", 1.0, 1.0, 1.0, 1);

    let blocked_model = admission.admit_batch("mission-model", &[(0, request.clone())]);
    assert_eq!(blocked_model.admitted_indices(), Vec::<usize>::new());
    assert_eq!(
        blocked_model.blocked.as_ref().map(|b| b.reason),
        Some("model unavailable")
    );
    assert_eq!(admission.queued(), 1);

    state.lock().unwrap_or_else(|e| e.into_inner()).model_ready = true;
    let blocked_gpu = admission.admit_batch("mission-model", &[(0, request.clone())]);
    assert_eq!(
        blocked_gpu.blocked.as_ref().map(|b| b.reason),
        Some("GPU memory unavailable")
    );
    assert_eq!(admission.queued(), 1);

    {
        let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
        current.gpu_mem_gb = 1.0;
    }
    let admitted = admission.admit_batch("mission-model", &[(0, request)]);
    assert_eq!(admitted.admitted_indices(), vec![0]);
    drop(admitted);
    assert_eq!(admission.active(), 0);
    assert_eq!(admission.queued(), 0);
}

#[test]
fn swarm_gap_live_admission_competing_missions_drain_fairly() {
    let admission = Arc::new(LiveAdmission::fixed(inventory(1.0, 0.0, 2.0, 1, true)));
    let holder = admission.admit_batch("mission-a", &[(0, node("a", 1.0, 0.0, 0.0, 1))]);
    assert_eq!(holder.admitted_indices(), vec![0]);

    let waiting_admission = Arc::clone(&admission);
    let waiter = std::thread::spawn(move || {
        waiting_admission.admit_batch("mission-b", &[(0, node("b", 1.0, 0.0, 0.0, 1))])
    });
    for _ in 0..100 {
        if admission.queued() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        std::thread::yield_now();
    }
    assert_eq!(admission.queued(), 1);
    drop(holder);

    let drained = waiter
        .join()
        .expect("waiting mission must drain after release");
    assert_eq!(drained.admitted_indices(), vec![0]);
    drop(drained);
    assert_eq!(admission.active(), 0);
}

#[test]
fn swarm_gap_live_admission_required_specialists_drain_from_dag_entry() {
    let admission = Arc::new(LiveAdmission::fixed(inventory(1.0, 0.0, 8.0, 1, true)));
    let mut dag = MissionDag::new("live admission").with_live_admission(admission);
    let specialist_one = dag.push_node("Specialist-1", "required specialist one", vec![]);
    let specialist_two = dag.push_node("Specialist-2", "required specialist two", vec![]);
    dag.nodes[specialist_one].assigned_agent = Some("RequiredSpecialist-1".into());
    dag.nodes[specialist_two].assigned_agent = Some("RequiredSpecialist-2".into());
    dag.set_resource_request(specialist_one, node("specialist-one", 1.0, 0.0, 0.0, 1));
    dag.set_resource_request(specialist_two, node("specialist-two", 1.0, 0.0, 0.0, 1));

    let first = dag.admit_ready_nodes("mission-specialists", &[0, specialist_one, specialist_two]);
    assert_eq!(first.admitted_indices(), vec![0]);
    assert_eq!(first.deferred, vec![specialist_one, specialist_two]);
    assert!(!dag.nodes[specialist_one].completed);
    assert!(!dag.nodes[specialist_two].completed);
    drop(first);

    let second = dag.admit_ready_nodes("mission-specialists", &[specialist_one, specialist_two]);
    assert_eq!(second.admitted_indices(), vec![specialist_one]);
    assert_eq!(second.deferred, vec![specialist_two]);
    drop(second);

    let third = dag.admit_ready_nodes("mission-specialists", &[specialist_two]);
    assert_eq!(third.admitted_indices(), vec![specialist_two]);
    assert!(third.deferred.is_empty());
    drop(third);
}
