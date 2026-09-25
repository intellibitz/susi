//! Semantic Boundary Contracts (Swarm OS Bullet 23)
//!
//! Allows cells to define explicit input/output semantic contracts
//! (e.g., "Input: JSON, Output: Markdown"). Validated at runtime by the OS.

use std::collections::HashMap;
use std::sync::RwLock;

/// Represents a payload's semantic type (MIME-like).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MimeType {
    Json,
    Markdown,
    PlainText,
    Binary,
    Custom(String),
}

/// A semantic contract defining what a cell accepts and produces.
#[derive(Debug, Clone)]
pub struct SemanticContract {
    pub cell_id: String,
    pub accepted_inputs: Vec<MimeType>,
    pub guaranteed_outputs: Vec<MimeType>,
}

/// Manages and validates semantic contracts across the swarm.
pub struct ContractManager {
    contracts: RwLock<HashMap<String, SemanticContract>>,
}

impl Default for ContractManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractManager {
    pub fn new() -> Self {
        Self {
            contracts: RwLock::new(HashMap::new()),
        }
    }

    /// Registers a contract for a specific cell.
    pub fn register(&self, contract: SemanticContract) {
        let mut map = self.contracts.write().unwrap_or_else(|e| e.into_inner());
        map.insert(contract.cell_id.clone(), contract);
    }

    /// Validates if a cell is allowed to receive a specific payload type.
    pub fn validate_input(
        &self,
        target_cell_id: &str,
        input_type: &MimeType,
    ) -> Result<(), String> {
        let map = self.contracts.read().unwrap_or_else(|e| e.into_inner());

        if let Some(contract) = map.get(target_cell_id) {
            if contract.accepted_inputs.contains(input_type) {
                Ok(())
            } else {
                Err(format!(
                    "Semantic violation: Cell '{}' does not accept input type {:?}",
                    target_cell_id, input_type
                ))
            }
        } else {
            // If no contract is registered, we deny by default for strict boundary safety
            Err(format!(
                "Semantic violation: No contract registered for cell '{}'",
                target_cell_id
            ))
        }
    }

    /// Validates if a cell actually produced the output it promised.
    pub fn validate_output(
        &self,
        source_cell_id: &str,
        output_type: &MimeType,
    ) -> Result<(), String> {
        let map = self.contracts.read().unwrap_or_else(|e| e.into_inner());

        if let Some(contract) = map.get(source_cell_id) {
            if contract.guaranteed_outputs.contains(output_type) {
                Ok(())
            } else {
                Err(format!(
                    "Semantic violation: Cell '{}' produced output {:?}, which is not in its guaranteed outputs",
                    source_cell_id, output_type
                ))
            }
        } else {
            Err(format!(
                "Semantic violation: No contract registered for cell '{}'",
                source_cell_id
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_contract_validation() {
        let manager = ContractManager::new();

        // Register cell
        manager.register(SemanticContract {
            cell_id: "agent-1".to_string(),
            accepted_inputs: vec![MimeType::Json],
            guaranteed_outputs: vec![MimeType::Markdown],
        });

        // Valid input
        assert!(manager.validate_input("agent-1", &MimeType::Json).is_ok());

        // Invalid input
        assert!(
            manager
                .validate_input("agent-1", &MimeType::PlainText)
                .is_err()
        );

        // Valid output
        assert!(
            manager
                .validate_output("agent-1", &MimeType::Markdown)
                .is_ok()
        );

        // Invalid output
        assert!(manager.validate_output("agent-1", &MimeType::Json).is_err());

        // Unknown cell
        assert!(
            manager
                .validate_input("agent-unknown", &MimeType::Json)
                .is_err()
        );
    }
}
