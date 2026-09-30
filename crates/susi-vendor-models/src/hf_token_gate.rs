//! Just-in-time Hugging Face token gating — ask for a token only when the
//! requested model actually needs one (T-CLAUDE-182): gated repos or a
//! recorded 401/403. Public models never trigger a prompt.

/// Whether a HF repo requires auth. Callers learn this from the model page
/// metadata (`gated: true`) or from a prior download failure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gatedness {
    /// `gated: true` on the model card — auth required, and approval may lag.
    Gated,
    /// `gated: false` or `gated` field absent — public.
    Public,
    /// Metadata fetch failed; treat as public until proven otherwise.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenDecision {
    /// No prompt — proceed anonymously (or with the token we already hold).
    Proceed,
    /// Show the one-time token prompt with this reason.
    AskOnce,
    /// Stop: a token exists but was rejected (revoked or insufficient scope).
    Blocked,
}

/// Decide whether to prompt for a HF token.
///
/// - `gated` — the model's gating state.
/// - `last_status` — status of the last anonymous attempt, if any (401/403
///   mean the repo needs auth even when metadata said public).
/// - `have_token` — a token is already configured.
pub fn decide(gated: Gatedness, last_status: Option<u16>, have_token: bool) -> TokenDecision {
    if have_token {
        // 401/403 WITH a token means the token itself is the problem.
        return match last_status {
            Some(401) | Some(403) => TokenDecision::Blocked,
            Some(_) | None => TokenDecision::Proceed,
        };
    }
    match last_status {
        Some(401) | Some(403) => return TokenDecision::AskOnce,
        _ => {}
    }
    match gated {
        Gatedness::Gated => TokenDecision::AskOnce,
        Gatedness::Public | Gatedness::Unknown => TokenDecision::Proceed,
    }
}

/// The one-line reason shown with the prompt.
pub fn prompt_reason(gated: Gatedness, last_status: Option<u16>) -> &'static str {
    match last_status {
        Some(401) => "the hub rejected the anonymous request (401) — a token is required",
        Some(403) => {
            "access was denied (403) — this repo needs a token or licence acceptance"
        }
        _ => match gated {
            Gatedness::Gated => {
                "this is a gated repository — paste a token with access, or accept the licence on the hub first"
            }
            Gatedness::Public | Gatedness::Unknown => {
                "a Hugging Face token is required for this download"
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zc_hf_token_jit_public_never_asks() {
        assert_eq!(
            decide(Gatedness::Public, None, false),
            TokenDecision::Proceed
        );
        assert_eq!(
            decide(Gatedness::Unknown, None, false),
            TokenDecision::Proceed
        );
    }

    #[test]
    fn zc_hf_token_jit_gated_asks_once() {
        assert_eq!(
            decide(Gatedness::Gated, None, false),
            TokenDecision::AskOnce
        );
        assert!(prompt_reason(Gatedness::Gated, None).contains("gated"));
    }

    #[test]
    fn zc_hf_token_jit_401_overrides_public_metadata() {
        // a 401 on a supposedly-public repo still means "token needed"
        assert_eq!(
            decide(Gatedness::Public, Some(401), false),
            TokenDecision::AskOnce
        );
        assert_eq!(
            decide(Gatedness::Unknown, Some(403), false),
            TokenDecision::AskOnce
        );
    }

    #[test]
    fn zc_hf_token_jit_existing_token_proceeds() {
        assert_eq!(decide(Gatedness::Gated, None, true), TokenDecision::Proceed);
    }

    #[test]
    fn zc_hf_token_jit_rejected_token_blocks() {
        // 401 with a configured token = the token is bad; don't silently retry
        assert_eq!(
            decide(Gatedness::Public, Some(401), true),
            TokenDecision::Blocked
        );
        assert_eq!(
            decide(Gatedness::Gated, Some(403), true),
            TokenDecision::Blocked
        );
    }
}
