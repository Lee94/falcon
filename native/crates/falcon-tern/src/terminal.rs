use std::io::{self, IsTerminal, Read, Write};
use std::sync::Arc;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use falcon_client::{FalconClient, SessionEvent, SessionSocket};
use falcon_proto::{ProjectType, SessionState};
use falcon_proto::terminal_replies::ReplyFilter;
use tokio::sync::mpsc;

use crate::config::request;

mod input;
mod local;
mod output;
mod replay;
mod signals;
#[cfg(windows)]
mod windows;

const RESET: &[u8] = b"\x18\x1b\\\x1b[!p\x0f\x1b(B\x1b)B\x1b[?1l\x1b>\x1b[?6l\x1b[?7h\x1b[r\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\x1b[?2004l\x1b[2J\x1b[3J\x1b[H";
const RESTORE: &[u8] = b"\x1b[0m\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l\x1b[?2004l\x1b[?25h\x1b[?1049l\x1b[23;0t";

struct Terminal {
    #[cfg(windows)]
    _console: windows::ConsoleModes,
}

impl Terminal {
    fn enter(title: &str, native: bool) -> Result<Self> {
        #[cfg(windows)]
        let console = windows::ConsoleModes::capture()?;
        crossterm::terminal::enable_raw_mode().context("cannot enable terminal raw mode")?;
        let terminal = Self {
            #[cfg(windows)]
            _console: console,
        };
        #[cfg(windows)]
        terminal._console.enable_virtual_terminal()?;
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\x1b[22;0t")?;
        // Inline TSP surfaces remain hidden while Tern shows its alternate buffer.
        // Let the guest's own VT stream control any alternate-screen applications.
        if !native { stdout.write_all(b"\x1b[?1049h")?; }
        write!(stdout, "\x1b]0;{title}\x07")?;
        stdout.flush()?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(RESTORE);
        let _ = stdout.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn display(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_control()).collect()
}

fn dead_error(id: &str, reason: Option<&falcon_proto::DeadReason>) -> anyhow::Error {
    anyhow::anyhow!("session {} is dead ({})", display(id), reason.map(|value| value.as_str()).unwrap_or("unknown reason"))
}

// A dedicated OS thread keeps stdin byte-for-byte (including paste and terminal replies).
// It is not a Tokio blocking task: a blocked read must not hold runtime shutdown open.
fn stdin_reader(tx: mpsc::Sender<io::Result<Vec<u8>>>) -> io::Result<()> {
    std::thread::Builder::new().name("falcon-tern-stdin".into()).spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut bytes = [0; 8192];
        loop {
            match stdin.read(&mut bytes) {
                Ok(0) => break,
                Ok(size) => {
                    if tx.blocking_send(Ok(bytes[..size].to_vec())).is_err() { break; }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = tx.blocking_send(Err(error));
                    break;
                }
            }
        }
    })?;
    Ok(())
}

pub async fn attach(client: &FalconClient, id: &str, sessiond_root: Option<&Path>) -> Result<()> {
    let (sessions, projects) = tokio::try_join!(request(client.list_sessions()), request(client.list_projects()))?;
    let session = sessions.iter().find(|session| session.id == id)
        .with_context(|| format!("session {} does not exist", display(id)))?;
    if session.state == SessionState::Dead {
        return Err(dead_error(id, session.dead_reason.as_ref()));
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("attach requires an interactive terminal on stdin and stdout");
    }
    let size = crossterm::terminal::size().context("cannot read terminal dimensions")?;
    if size.0 == 0 || size.1 == 0 { bail!("terminal dimensions must be nonzero"); }
    let project = projects.iter().find(|project| project.id == session.project_id);
    let title = format!("Falcon · {} · {} · {}",
        display(&session.project_name), session.project_type.as_str(),
        project.and_then(|project| project.working_dir.as_deref()).map(display).unwrap_or_else(|| "(working directory unavailable)".into()));
    eprintln!("{title}\nDetach: Ctrl+\\ (session keeps running)");

    // Local durable sessions already expose a terminal attachment. Using it avoids the
    // server's 16ms output window and a second pair of transport queues on every keystroke.
    if session.durable && session.project_type == ProjectType::Local
        && let Some(root) = sessiond_root
    {
        // A Tern-only client must also deploy the current daemon and hot-upgrade
        // an old VT; no Falcon window may be present to perform lazy attachment.
        request(client.reattach_session(id)).await.context("cannot prepare the persistent terminal")?;
        return local::attach(root, id);
    }

    let native = session.durable && std::env::var("TERM_PROGRAM")
        .is_ok_and(|value| value.eq_ignore_ascii_case("tern"));
    // Install handlers before changing terminal state, so SIGHUP/window closure restores it too.
    let mut signals = signals::Signals::new()?;
    let terminal = Terminal::enter(&title, native)?;
    let (event_tx, mut events) = mpsc::channel(32);
    let sink = Arc::new(move |event| {
        falcon_client::block_in_place(|| { let _ = event_tx.blocking_send(event); });
    });
    let socket = SessionSocket::open(client, id, sink);
    socket.resize(size.0, size.1);
    let mut refresh = None;
    let result = interact(client, &socket, &mut events, &mut signals, size, native, &mut refresh).await;
    // Closing the WS is only detach; never call terminate or clear.
    socket.close();
    drop(events);
    drop(terminal);
    if let Some(refresh) = refresh {
        report_refresh(refresh.await);
    }
    if native {
        // No more output is forwarded to Tern: a fresh hello is now answered
        // by the persistent VT and returns this same process to its ANSI UI.
        if let Err(error) = request(client.reprobe_session(id)).await {
            eprintln!("[Falcon could not restore the terminal display: {error}]");
        }
    }
    result
}

type Refresh = tokio::task::JoinHandle<Result<()>>;

fn report_refresh(result: std::result::Result<Result<()>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("[Falcon display refresh failed: {error}]"),
        Err(error) => eprintln!("[Falcon display refresh task failed: {error}]"),
    }
}

async fn interact(
    client: &FalconClient,
    socket: &SessionSocket,
    events: &mut mpsc::Receiver<SessionEvent>,
    signals: &mut signals::Signals,
    mut size: (u16, u16),
    native: bool,
    refresh: &mut Option<Refresh>,
) -> Result<()> {
    let (input_tx, mut input_rx) = mpsc::channel(16);
    stdin_reader(input_tx).context("cannot read terminal input")?;
    let mut input = input::Utf8Input::default();
    let mut output = output::Output::new();
    let mut connected = false;
    let mut askpass: Option<(String, String)> = None;
    let mut replies = native.then(ReplyFilter::default);
    let mut input_deadline: Option<Instant> = None;
    let mut resize = tokio::time::interval(Duration::from_millis(100));
    resize.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            signal = signals.recv() => {
                signal?;
                return Ok(());
            }
            bytes = input_rx.recv() => {
                let Some(bytes) = bytes else { return Ok(()) };
                let mut bytes = bytes.context("terminal input failed")?;
                if let Some(replies) = &mut replies {
                    bytes = replies.push(&bytes);
                    if replies.has_pending() {
                        input_deadline.get_or_insert_with(|| Instant::now() + Duration::from_millis(30));
                    } else {
                        input_deadline = None;
                    }
                }
                let detach = bytes.iter().position(|byte| *byte == 0x1c);
                if let Some(end) = detach { bytes.truncate(end); }
                let text = if connected || askpass.is_some() { input.push(bytes) } else { String::new() };
                if let Some((prompt_id, password)) = askpass.as_mut() {
                    let mut answer = false;
                    let mut cancel = false;
                    for ch in text.chars() {
                        match ch {
                            '\r' | '\n' => { answer = true; break; }
                            '\x03' => { cancel = true; break; }
                            '\x08' | '\x7f' => { password.pop(); }
                            ch if !ch.is_control() => password.push(ch),
                            _ => {}
                        }
                    }
                    if cancel {
                        request(client.cancel_askpass(prompt_id)).await?;
                    } else if answer {
                        request(client.answer_askpass(prompt_id, password)).await?;
                    }
                    if answer || cancel {
                        askpass = None;
                        output.send(b"\r\n")?;
                    }
                } else if connected {
                    socket.send_input(text);
                }
                if detach.is_some() { return Ok(()); }
            }
            event = events.recv() => {
                let Some(event) = event else { bail!("Falcon session connection closed") };
                match event {
                    SessionEvent::Output(bytes) => {
                        if let Some(replies) = &mut replies {
                            replies.observe_output(&bytes);
                            if replies.has_pending() {
                                input_deadline.get_or_insert_with(|| Instant::now() + Duration::from_millis(30));
                            }
                        }
                        output.send(&bytes)?;
                    }
                    SessionEvent::Replay(bytes) => {
                        output.replay(&bytes)?;
                        if native && refresh.is_none()
                            && !falcon_proto::tsp::snapshot_has_live_surface(&bytes)
                        {
                            let future = client.reprobe_session(socket.session_id());
                            *refresh = Some(tokio::spawn(async move { request(future).await.map(|_| ()) }));
                        }
                    }
                    SessionEvent::Connected { .. } => {
                        input.clear();
                        connected = true;
                    }
                    SessionEvent::Disconnected { retry_attempt, retry_in } => {
                        connected = false;
                        input.clear();
                        askpass = None;
                        replies = native.then(ReplyFilter::default);
                        input_deadline = None;
                        output.send(format!("\r\n[Falcon disconnected; retry {retry_attempt} in {:.0}s; Ctrl+\\ detaches]\r\n", retry_in.as_secs_f64()).as_bytes())?;
                    }
                    SessionEvent::State { state: SessionState::Dead, dead_reason } => {
                        return Err(dead_error(socket.session_id(), dead_reason.as_ref()));
                    }
                    SessionEvent::Unauthorized => {
                        bail!("Falcon authentication required or saved password rejected; update passwordFile or desktopPasswords/profile and reattach");
                    }
                    SessionEvent::Error(message) => bail!("Falcon attachment failed: {}", display(&message)),
                    SessionEvent::Reconnecting { attempt } => {
                        output.send(format!("\r\n[Falcon reconnecting to session host, attempt {attempt}]\r\n").as_bytes())?;
                    }
                    SessionEvent::Askpass { id, prompt } => {
                        askpass = Some((id, String::new()));
                        output.send(format!("\r\n{} (input hidden; Ctrl+C cancels): ", display(&prompt)).as_bytes())?;
                    }
                    _ => {}
                }
            }
            result = async { refresh.as_mut().expect("guarded refresh").await }, if refresh.is_some() => {
                refresh.take();
                report_refresh(result);
            }
            _ = async { tokio::time::sleep_until(input_deadline.expect("guarded input deadline").into()).await }, if input_deadline.is_some() => {
                input_deadline = None;
                if connected && let Some(replies) = &mut replies {
                    let bytes = replies.flush_pending();
                    socket.send_input(input.push(bytes));
                }
            }
            _ = resize.tick() => {
                let next = crossterm::terminal::size().context("cannot read terminal dimensions")?;
                if next != size && next.0 > 0 && next.1 > 0 {
                    socket.resize(next.0, next.1);
                    size = next;
                }
            }
        }
    }
}
