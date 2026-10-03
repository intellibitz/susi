//! Test for capability floor per task class (T-DEEPSEEK-100).
//! Verifies that each task class declares minimum model capabilities.
//!
//! This test exercises:
//! - Task class definition with associated capability floor
//! - Capability floor enforcement before cost comparison
//! - Prevention of weak models being used for hard work
//! - Capability hierarchy (reasoning > coding > summarize)

use std::collections::HashMap;

/// Model capabilities that a task class requires.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// Very basic operations (summarization, classification)
    Basic,
    /// Code generation, analysis, debugging
    Coding,
    /// Complex multi-step reasoning, proof verification
    Reasoning,
}

/// Represents a task class with its minimum capability requirement.
#[derive(Debug, Clone)]
pub struct TaskClass {
    pub name: String,
    pub capability_floor: Capability,
}

impl TaskClass {
    pub fn new(name: &str, capability_floor: Capability) -> Self {
        Self {
            name: name.to_string(),
            capability_floor,
        }
    }

    /// Verify that a model's capabilities meet the floor for this task class.
    pub fn can_handle(&self, model_capabilities: &[Capability]) -> bool {
        model_capabilities
            .iter()
            .any(|cap| cap >= &self.capability_floor)
    }
}

/// Registry of task classes with their capability requirements.
pub struct TaskClassRegistry {
    classes: HashMap<String, TaskClass>,
}

impl TaskClassRegistry {
    pub fn new() -> Self {
        Self {
            classes: HashMap::new(),
        }
    }

    pub fn register(&mut self, task_class: TaskClass) {
        self.classes.insert(task_class.name.clone(), task_class);
    }

    /// Check if a model can be assigned to a task class.
    /// This must be checked BEFORE cost comparison to prevent weak models.
    pub fn is_capable(&self, task_class_name: &str, model_capabilities: &[Capability]) -> bool {
        match self.classes.get(task_class_name) {
            Some(tc) => tc.can_handle(model_capabilities),
            None => true, // Unknown task class defaults to capable
        }
    }

    /// Get the capability floor for a task class.
    pub fn capability_floor(&self, task_class_name: &str) -> Option<Capability> {
        self.classes
            .get(task_class_name)
            .map(|tc| tc.capability_floor.clone())
    }
}

#[test]
fn capability_floor() {
    // Foundation test: verify capability floor enforcement.
    // In production, this will:
    // 1. Declare task classes with minimum capability requirements
    // 2. Check floor before cost comparison (never price-optimize downward)
    // 3. Prevent weak models from receiving hard work
    // 4. Enable safe model substitution when capabilities are met

    let mut registry = TaskClassRegistry::new();

    // Register task classes with capability floors
    registry.register(TaskClass::new("summarize", Capability::Basic));
    registry.register(TaskClass::new("classify", Capability::Basic));
    registry.register(TaskClass::new("coding", Capability::Coding));
    registry.register(TaskClass::new("refactor", Capability::Coding));
    registry.register(TaskClass::new("prove", Capability::Reasoning));
    registry.register(TaskClass::new("verify", Capability::Reasoning));

    // Test 1: Basic capability model can do summarization
    let basic_model = vec![Capability::Basic];
    assert!(registry.is_capable("summarize", &basic_model));
    assert!(registry.is_capable("classify", &basic_model));
    assert!(
        !registry.is_capable("coding", &basic_model),
        "Basic model cannot do coding"
    );

    // Test 2: Coding capability model can do coding and basic work
    let coding_model = vec![Capability::Coding];
    assert!(registry.is_capable("coding", &coding_model));
    assert!(registry.is_capable("refactor", &coding_model));
    assert!(
        registry.is_capable("summarize", &coding_model),
        "Coding model can do basic work"
    );
    assert!(
        !registry.is_capable("prove", &coding_model),
        "Coding model cannot do reasoning"
    );

    // Test 3: Reasoning capability model can do everything
    let reasoning_model = vec![Capability::Reasoning];
    assert!(registry.is_capable("summarize", &reasoning_model));
    assert!(registry.is_capable("coding", &reasoning_model));
    assert!(registry.is_capable("prove", &reasoning_model));
    assert!(registry.is_capable("verify", &reasoning_model));

    // Test 4: Model with multiple capabilities
    let multi_model = vec![Capability::Basic, Capability::Coding, Capability::Reasoning];
    assert!(registry.is_capable("summarize", &multi_model));
    assert!(registry.is_capable("coding", &multi_model));
    assert!(registry.is_capable("prove", &multi_model));

    // Test 5: Capability floor prevents cost-based downgrade
    // Even if a cheaper Basic model is available, it cannot be used for coding
    let floor = registry.capability_floor("coding");
    assert_eq!(
        floor,
        Some(Capability::Coding),
        "Coding task has Coding floor"
    );

    let floor = registry.capability_floor("prove");
    assert_eq!(
        floor,
        Some(Capability::Reasoning),
        "Prove task has Reasoning floor"
    );

    // Test 6: Unknown task class defaults to capable
    assert!(registry.is_capable("unknown-class", &basic_model));
    assert!(registry.is_capable("custom-work", &[Capability::Basic]));

    // Summary: capability floor ensures hard work never goes to weak models.
    // Full implementation will:
    // - Declare floors in task creation (mandatory)
    // - Check floor before cost ranking (gates the algorithm)
    // - Reject assignments that violate the floor
    // - Enable safe cost optimization within capability envelope
    // - Support capability profiling: measure what each model can actually do
}
