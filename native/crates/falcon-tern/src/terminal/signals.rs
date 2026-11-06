use std::io;

#[cfg(unix)]
pub(super) struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    pub(super) fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    pub(super) async fn recv(&mut self) -> io::Result<()> {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
            _ = self.hangup.recv() => {},
            _ = self.quit.recv() => {},
        }
        Ok(())
    }
}

#[cfg(windows)]
pub(super) struct Signals {
    interrupt: tokio::signal::windows::CtrlC,
    close: tokio::signal::windows::CtrlClose,
    shutdown: tokio::signal::windows::CtrlShutdown,
    logoff: tokio::signal::windows::CtrlLogoff,
    ctrl_break: tokio::signal::windows::CtrlBreak,
}

#[cfg(windows)]
impl Signals {
    pub(super) fn new() -> io::Result<Self> {
        use tokio::signal::windows;
        Ok(Self {
            interrupt: windows::ctrl_c()?,
            close: windows::ctrl_close()?,
            shutdown: windows::ctrl_shutdown()?,
            logoff: windows::ctrl_logoff()?,
            ctrl_break: windows::ctrl_break()?,
        })
    }

    pub(super) async fn recv(&mut self) -> io::Result<()> {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.close.recv() => {},
            _ = self.shutdown.recv() => {},
            _ = self.logoff.recv() => {},
            _ = self.ctrl_break.recv() => {},
        }
        Ok(())
    }
}
