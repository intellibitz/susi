//! Dispatch for remote adapters that are not local processes.
//! Closed cloud products (Devin, Manus) are not admitted. A2A and the
//! OpenHands server stay: their projects are permissively licensed.

use super::{Adapter, AgentManager, RunRecord};
use susi_error::{eai_bail as bail, EaiResult as Result};

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::execute(manager, run);
    }
    if matches!(run.adapter, Adapter::OpenHandsServer { .. }) {
        return super::openhands_server::execute(manager, run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn refresh(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::refresh(manager, run);
    }
    if matches!(run.adapter, Adapter::OpenHandsServer { .. }) {
        return super::openhands_server::refresh(manager, run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::cancel(run);
    }
    if matches!(run.adapter, Adapter::OpenHandsServer { .. }) {
        return super::openhands_server::cancel(run);
    }
    bail!("not a cloud adapter")
}

pub(super) fn send(run: &RunRecord, message: &str) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::send(run, message);
    }
    if matches!(run.adapter, Adapter::OpenHandsServer { .. }) {
        return super::openhands_server::send(run, message);
    }
    bail!("not a cloud adapter")
}
