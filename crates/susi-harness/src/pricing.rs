//! Token accounting and deterministic cost arithmetic.
//!
//! Prices are quoted per million tokens, the way providers publish them, and
//! are stored in micro-USD so the arithmetic stays integral. Cache hits are a
//! separate price: on DeepSeek a hit costs roughly one fiftieth of a miss,
//! which is the single largest lever the harness has, so it must be modelled
//! rather than approximated.

use crate::money::MicroUsd;
use serde::{Deserialize, Serialize};

/// Tokens in one price unit (the "per million tokens" denominator).
pub const TOKENS_PER_PRICE_UNIT: u64 = 1_000_000;

/// Tokens consumed by one model call.
///
/// `cache_hit` is the subset of `input` served from the provider's prefix
/// cache. The constructor clamps it to `input`, but the fields stay public for
/// deserialization, so consumers use [`TokenUsage::cache_miss`], which is
/// saturating, rather than subtracting by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Total input tokens, cached or not.
    pub input: u64,
    /// Input tokens served from cache (a subset of `input`).
    pub cache_hit: u64,
    /// Output tokens.
    pub output: u64,
}

impl TokenUsage {
    /// Build usage, clamping the cache hit to the input it belongs to.
    #[must_use]
    pub const fn new(input: u64, cache_hit: u64, output: u64) -> Self {
        let hit = if cache_hit > input { input } else { cache_hit };
        Self {
            input,
            cache_hit: hit,
            output,
        }
    }

    /// Input tokens billed at the miss rate.
    #[must_use]
    pub const fn cache_miss(self) -> u64 {
        self.input.saturating_sub(self.cache_hit)
    }

    /// Input plus output tokens.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }

    /// Verify fail-closed invariants of a [`TokenUsage`] record.
    ///
    /// # Errors
    ///
    /// Returns an error if `cache_hit` exceeds `input` or if `input + output`
    /// overflows `u64`.
    pub fn verify_usage_invariants(&self) -> Result<(), String> {
        if self.cache_hit > self.input {
            return Err(format!(
                "token usage cache_hit ({}) exceeds input ({})",
                self.cache_hit, self.input
            ));
        }
        if self.input.checked_add(self.output).is_none() {
            return Err(format!(
                "token usage input ({}) + output ({}) overflows u64",
                self.input, self.output
            ));
        }
        Ok(())
    }
}

/// A model's published price, per million tokens, in micro-USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelPrice {
    /// Price per million input tokens that missed the cache.
    pub input_miss: MicroUsd,
    /// Price per million input tokens served from cache.
    pub input_hit: MicroUsd,
    /// Price per million output tokens.
    pub output: MicroUsd,
}

impl ModelPrice {
    /// Cost of `usage` at this price, rounded **up** to the micro-dollar.
    ///
    /// Rounding up is deliberate: a ledger that rounds down undercounts spend,
    /// and the error is silent. Saturating at `u64::MAX` keeps an absurd price
    /// from wrapping into a free call.
    #[must_use]
    pub fn cost(self, usage: TokenUsage) -> MicroUsd {
        let miss = u128::from(usage.cache_miss());
        let hit = u128::from(usage.cache_hit.min(usage.input));
        let output = u128::from(usage.output);
        let priced = miss * u128::from(self.input_miss.micros())
            + hit * u128::from(self.input_hit.micros())
            + output * u128::from(self.output.micros());
        let rounded = priced.div_ceil(u128::from(TOKENS_PER_PRICE_UNIT));
        MicroUsd::from_micros(u64::try_from(rounded).unwrap_or(u64::MAX))
    }

    /// Verify fail-closed invariants of a published [`ModelPrice`].
    ///
    /// # Errors
    ///
    /// Returns an error if all three rates are zero or if `input_hit` exceeds
    /// `input_miss`.
    pub fn verify_price_invariants(&self) -> Result<(), String> {
        if self.input_miss == MicroUsd::ZERO
            && self.input_hit == MicroUsd::ZERO
            && self.output == MicroUsd::ZERO
        {
            return Err("model price rates must not all be zero".to_string());
        }
        if self.input_hit > self.input_miss {
            return Err(format!(
                "model price input_hit ({}) exceeds input_miss ({})",
                self.input_hit, self.input_miss
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DeepSeek flash off-peak: $0.15 / $0.003 / $0.60 per million tokens.
    fn flash() -> ModelPrice {
        ModelPrice {
            input_miss: MicroUsd::from_micros(150_000),
            input_hit: MicroUsd::from_micros(3_000),
            output: MicroUsd::from_micros(600_000),
        }
    }

    #[test]
    fn a_cache_hit_costs_a_fiftieth_of_a_miss() {
        let price = flash();
        let hit = price.cost(TokenUsage::new(1_000_000, 1_000_000, 0));
        let miss = price.cost(TokenUsage::new(1_000_000, 0, 0));
        assert_eq!(hit, MicroUsd::from_micros(3_000));
        assert_eq!(miss, MicroUsd::from_micros(150_000));
    }

    #[test]
    fn mixed_usage_is_priced_per_bucket() {
        // 3M input (2.4M cached) + 200k output, the worked example from the plan.
        let usage = TokenUsage::new(3_000_000, 2_400_000, 200_000);
        assert_eq!(flash().cost(usage), MicroUsd::from_micros(217_200));
        assert_eq!(usage.cache_miss(), 600_000);
        assert_eq!(usage.total(), 3_200_000);
    }

    #[test]
    fn cost_rounds_up_never_down() {
        let price = ModelPrice {
            input_miss: MicroUsd::from_micros(1),
            input_hit: MicroUsd::ZERO,
            output: MicroUsd::ZERO,
        };
        assert_eq!(
            price.cost(TokenUsage::new(1, 0, 0)),
            MicroUsd::from_micros(1)
        );
        assert_eq!(price.cost(TokenUsage::new(0, 0, 0)), MicroUsd::ZERO);
    }

    #[test]
    fn cache_hit_is_clamped_to_input() {
        let usage = TokenUsage::new(10, 99, 1);
        assert_eq!(usage.cache_hit, 10);
        assert_eq!(usage.cache_miss(), 0);
    }

    #[test]
    fn token_usage_and_model_price_round_trip_serde_json_and_saturate_on_overflow() {
        let usage = TokenUsage::new(5_000, 2_000, 1_000);
        let usage_json = serde_json::to_string(&usage).unwrap();
        let usage_back: TokenUsage = serde_json::from_str(&usage_json).unwrap();
        assert_eq!(usage_back, usage);

        let price = flash();
        let price_json = serde_json::to_string(&price).unwrap();
        let price_back: ModelPrice = serde_json::from_str(&price_json).unwrap();
        assert_eq!(price_back, price);

        let huge_price = ModelPrice {
            input_miss: MicroUsd::from_micros(u64::MAX),
            input_hit: MicroUsd::ZERO,
            output: MicroUsd::ZERO,
        };
        let huge_usage = TokenUsage::new(u64::MAX, 0, 0);
        assert_eq!(huge_price.cost(huge_usage), MicroUsd::from_micros(u64::MAX));
    }

    #[test]
    fn usage_and_price_verify_invariants_reject_corrupt_state() {
        let valid_usage = TokenUsage::new(1_000, 400, 200);
        assert!(valid_usage.verify_usage_invariants().is_ok());

        let hit_exceeds_input = TokenUsage {
            input: 100,
            cache_hit: 101,
            output: 10,
        };
        assert!(hit_exceeds_input.verify_usage_invariants().is_err());

        let overflow_total = TokenUsage {
            input: u64::MAX,
            cache_hit: 0,
            output: 1,
        };
        assert!(overflow_total.verify_usage_invariants().is_err());

        assert!(flash().verify_price_invariants().is_ok());
        assert!(ModelPrice::default().verify_price_invariants().is_err());
        let inverted = ModelPrice {
            input_miss: MicroUsd::from_micros(100),
            input_hit: MicroUsd::from_micros(200),
            output: MicroUsd::from_micros(300),
        };
        assert!(inverted.verify_price_invariants().is_err());
    }
}
