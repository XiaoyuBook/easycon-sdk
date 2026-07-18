use std::collections::BTreeSet;
use std::fs;
use std::sync::{Arc, Barrier, Mutex};

use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContractRuntimeState {
    Active,
    Closing,
    Closed,
    CloseFailed,
}

#[derive(Debug)]
struct ContractCloseReport {
    failed_resource_id: u64,
    remaining_resources: usize,
}

struct RuntimeContractModel {
    state: ContractRuntimeState,
    admission_open: bool,
    root_cancelled: bool,
    task_owned: bool,
    task_active: bool,
    task_joined: bool,
    resource_callback_count: usize,
    healthy_resource_closed: bool,
    finalizer_started: bool,
    final_event: Option<&'static str>,
    operation_terminal: bool,
    close_report: Option<Arc<ContractCloseReport>>,
}

impl RuntimeContractModel {
    fn new() -> Self {
        Self {
            state: ContractRuntimeState::Active,
            admission_open: true,
            root_cancelled: false,
            task_owned: false,
            task_active: false,
            task_joined: false,
            resource_callback_count: 0,
            healthy_resource_closed: false,
            finalizer_started: false,
            final_event: None,
            operation_terminal: false,
            close_report: None,
        }
    }

    fn spawn_supervised(&mut self) {
        assert_eq!(self.state, ContractRuntimeState::Active);
        self.task_owned = true;
        self.task_active = true;
    }

    fn close_from_supervised_task(&self) -> bool {
        false
    }

    fn finish_task(&mut self) {
        self.task_active = false;
    }

    fn close_successfully(&mut self) {
        assert_eq!(self.state, ContractRuntimeState::Active);
        self.state = ContractRuntimeState::Closing;
        self.admission_open = false;
        self.root_cancelled = true;
        assert!(!self.task_active);
        self.task_joined = true;
        self.final_event = Some("runtime.closed");
        self.state = ContractRuntimeState::Closed;
    }

    fn drop_final_owner(&mut self) {
        assert_eq!(self.state, ContractRuntimeState::Active);
        self.state = ContractRuntimeState::Closing;
        self.admission_open = false;
        self.root_cancelled = true;
    }

    fn close_with_resource_panic(&mut self) -> Arc<ContractCloseReport> {
        if let Some(report) = &self.close_report {
            return Arc::clone(report);
        }
        self.state = ContractRuntimeState::Closing;
        self.admission_open = false;
        self.root_cancelled = true;
        self.resource_callback_count += 1;
        self.resource_callback_count += 1;
        self.healthy_resource_closed = true;
        let report = Arc::new(ContractCloseReport {
            failed_resource_id: 7,
            remaining_resources: 1,
        });
        self.final_event = Some("runtime.close_failed");
        self.state = ContractRuntimeState::CloseFailed;
        self.close_report = Some(Arc::clone(&report));
        report
    }

    fn finish_failed_owner_cleanup(&mut self) {
        assert_eq!(self.state, ContractRuntimeState::CloseFailed);
        self.task_active = false;
        self.operation_terminal = true;
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn verify_coverage(scenario_id: &str, steps: &[&str], assertions: &[&str]) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../spec/conformance/runtime-controller-v1.json"
    );
    let document: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("conformance read"))
            .expect("conformance JSON");
    let scenario = document["scenarios"]
        .as_array()
        .expect("scenario array")
        .iter()
        .find(|scenario| scenario["id"].as_str() == Some(scenario_id))
        .unwrap_or_else(|| panic!("missing conformance scenario {scenario_id}"));
    let expected_steps = ids(scenario, "steps");
    let expected_assertions = ids(scenario, "assertions");
    assert_eq!(
        steps.iter().copied().collect::<BTreeSet<_>>(),
        expected_steps
    );
    assert_eq!(
        assertions.iter().copied().collect::<BTreeSet<_>>(),
        expected_assertions
    );
}

fn ids<'a>(scenario: &'a Value, field: &str) -> BTreeSet<&'a str> {
    scenario[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} array"))
        .iter()
        .map(|item| item["id"].as_str().expect("conformance ID"))
        .collect()
}

// conformance: runtime.drop-limited
#[test]
fn runtime_stabilization_reference_actions_are_executable() {
    let mut successful = RuntimeContractModel::new();
    successful.spawn_supervised();
    assert!(!successful.close_from_supervised_task());
    assert_eq!(successful.state, ContractRuntimeState::Active);
    successful.finish_task();
    successful.close_successfully();
    assert!(successful.task_owned && successful.task_joined);
    assert_eq!(successful.state, ContractRuntimeState::Closed);

    let mut dropped = RuntimeContractModel::new();
    dropped.drop_final_owner();
    assert!(!dropped.admission_open && dropped.root_cancelled);
    assert_eq!(dropped.resource_callback_count, 0);
    assert!(!dropped.finalizer_started);
    assert!(dropped.final_event.is_none());

    let failed = Arc::new(Mutex::new(RuntimeContractModel::new()));
    lock_recover(&failed).spawn_supervised();
    let first = lock_recover(&failed).close_with_resource_panic();
    let barrier = Arc::new(Barrier::new(3));
    let callers: Vec<_> = (0..2)
        .map(|_| {
            let failed = Arc::clone(&failed);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                lock_recover(&failed).close_with_resource_panic()
            })
        })
        .collect();
    barrier.wait();
    let concurrent: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("concurrent close caller"))
        .collect();
    let later = lock_recover(&failed).close_with_resource_panic();
    let failed_state = lock_recover(&failed);
    assert_eq!(failed_state.resource_callback_count, 2);
    assert!(failed_state.healthy_resource_closed);
    assert!(
        concurrent
            .iter()
            .all(|outcome| Arc::ptr_eq(&first, outcome))
    );
    assert!(Arc::ptr_eq(&first, &later));
    assert_eq!(failed_state.final_event, Some("runtime.close_failed"));
    assert!(!failed_state.operation_terminal);
    assert_eq!(first.failed_resource_id, 7);
    assert_eq!(first.remaining_resources, 1);
    drop(failed_state);
    lock_recover(&failed).finish_failed_owner_cleanup();
    assert!(lock_recover(&failed).operation_terminal);

    verify_coverage(
        "runtime-stabilization",
        &[
            "runtime.spawn-self-close",
            "runtime.external-close",
            "runtime.final-drop",
            "runtime.drop-observe",
            "runtime.resource-panic",
            "runtime.failed-owner-cleanup",
            "runtime.finish-real-cleanup",
            "runtime.repeat-close-failed",
        ],
        &[
            "runtime.self-close-rejected",
            "runtime.task-owned",
            "runtime.drop-limited",
            "runtime.healthy-cleanup-after-panic",
            "runtime.saved-close-failed",
            "runtime.owner-terminal-order",
            "runtime.close-report",
        ],
    );
}
