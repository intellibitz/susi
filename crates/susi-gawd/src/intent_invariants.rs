//! Property / metamorphic intent checks for pure reflexes (VC-201-008).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentInvariant {
    pub name: String,
    pub holds: bool,
    pub counterexample: Option<String>,
}

/// Metamorphic: same action for whitespace-normalized inputs.
#[must_use]
pub fn metamorphic_whitespace(
    action_for: impl Fn(&str) -> String,
    a: &str,
    b: &str,
) -> IntentInvariant {
    let left = action_for(a.trim());
    let right = action_for(b.trim());
    if left == right {
        IntentInvariant {
            name: "whitespace_normalize".into(),
            holds: true,
            counterexample: None,
        }
    } else {
        IntentInvariant {
            name: "whitespace_normalize".into(),
            holds: false,
            counterexample: Some(format!("{a:?} → {left}; {b:?} → {right}")),
        }
    }
}

/// Pure reflex must not invent actions outside an allowlist.
#[must_use]
pub fn action_in_allowlist(action: &str, allow: &[&str]) -> IntentInvariant {
    let holds = allow.contains(&action);
    IntentInvariant {
        name: "allowlist".into(),
        holds,
        counterexample: if holds { None } else { Some(action.into()) },
    }
}
