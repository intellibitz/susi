//! WASM Gas Metering (Swarm OS Bullet 88)
//!
//! Bounds how much work a sandboxed WASM reflex may perform before it's
//! killed. `consume` uses a compare-and-swap loop so a rejected charge
//! never mutates `consumed` — a tripped meter reports exactly what it
//! actually ran, not the over-budget amount it refused.

use std::sync::atomic::{AtomicU64, Ordering};

pub struct GasMeter {
    limit: u64,
    consumed: AtomicU64,
}

impl GasMeter {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            consumed: AtomicU64::new(0),
        }
    }

    /// Charges `amount` units of gas. Fails without mutating state if the
    /// charge would exceed the limit.
    pub fn consume(&self, amount: u64) -> Result<(), String> {
        let mut current = self.consumed.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(amount).ok_or("gas overflow")?;
            if next > self.limit {
                return Err("Out of Gas".to_string());
            }
            match self.consumed.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::Acquire)
    }

    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.consumed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumes_up_to_the_limit() {
        let meter = GasMeter::new(100);
        assert!(meter.consume(60).is_ok());
        assert!(meter.consume(40).is_ok());
        assert_eq!(meter.remaining(), 0);
    }

    #[test]
    fn rejected_charge_does_not_mutate_consumed() {
        let meter = GasMeter::new(100);
        meter.consume(90).unwrap();
        assert!(meter.consume(20).is_err());
        assert_eq!(meter.consumed(), 90);
        // A smaller charge that now fits still succeeds afterward.
        assert!(meter.consume(10).is_ok());
        assert_eq!(meter.remaining(), 0);
    }
}
