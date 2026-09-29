//! Verify synthesized reflex intent correctness (VC-201-007).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentFixture {
    pub input: String,
    pub expect_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflexVerdict {
    Accept,
    RejectMalformed,
    RejectSignatureEcho,
    RejectWrongOutput,
}

/// Independent compile-and-probe: reject signature-echo and malformed inputs.
pub fn probe_reflex(code: &str, fixture: &IntentFixture) -> ReflexVerdict {
    if fixture.input.contains('\0') || fixture.input.trim().is_empty() {
        return ReflexVerdict::RejectMalformed;
    }
    // Signature-echo: executable that just returns the goal verbatim.
    if code.contains("echo_goal") || code.contains(&fixture.input) && code.contains("return input")
    {
        return ReflexVerdict::RejectSignatureEcho;
    }
    // Toy probe: require the expected action token appears as a constant, not as echo.
    if code.contains(&format!("ACTION: {}", fixture.expect_action)) {
        ReflexVerdict::Accept
    } else {
        ReflexVerdict::RejectWrongOutput
    }
}
