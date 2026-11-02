//! Dispatch for remote adapters over the standard A2A protocol.

use super::{Adapter, AgentManager, RunRecord};
use susi_error::{eai_bail as bail, EaiResult as Result};

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::execute(manager, run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn refresh(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::refresh(manager, run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::cancel(run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn send(run: &RunRecord, message: &str) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::send(run, message);
    }
    bail!("not a cloud adapter")
}
