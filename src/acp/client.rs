use super::{AcpError, AcpResolvedLaunch, AcpResult, ClientUpdate};

pub trait AcpSession: Send {
    fn session_id(&self) -> &str;
    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>>;
    fn close(&mut self) -> AcpResult<()>;
}

pub trait AcpConnector {
    fn connect(&self, launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>>;
}

#[derive(Debug, Clone, Default)]
pub struct SubprocessConnector;

impl AcpConnector for SubprocessConnector {
    fn connect(&self, _launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>> {
        Err(AcpError::protocol(
            "ACP subprocess transport is not wired yet",
        ))
    }
}
