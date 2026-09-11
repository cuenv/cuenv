use super::events::{ControlEvent, EventBridge};
use super::input::TerminalKeyEvent;
use super::model::{SamplingToken, TerminalFrame};
use super::rio_adapter::snapshot;
use super::rio_input::{encode_key, encode_paste};
use super::sizing::{CellSize, TerminalSize, terminal_size};
use rio_vt::ansi::CursorShape;
use rio_vt::corcovado::channel::Sender;
use rio_vt::crosswords::grid::Scroll;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::sync::FairMutex;
use rio_vt::event::{EventListener, Msg, RioEvent, WindowId, WindowSize};
use rio_vt::performer::{Machine, State};
use rio_vt::teletypewriter::{self, Pty};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub struct SessionStart {
    pub session: Box<dyn TerminalSession>,
    pub wake_rx: async_channel::Receiver<()>,
}

pub trait TerminalSessionFactory: Send + Sync {
    fn start(&self, width: u32, height: u32, cell: CellSize) -> Result<SessionStart, String>;
}

fn default_working_directory() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|directory| !directory.is_empty())
}

pub struct RioTerminalFactory;
impl TerminalSessionFactory for RioTerminalFactory {
    fn start(&self, width: u32, height: u32, cell: CellSize) -> Result<SessionStart, String> {
        RioTerminalSession::new(width, height, cell).map(|(session, wake_rx)| SessionStart {
            session: Box::new(session),
            wake_rx,
        })
    }
}

pub trait TerminalSession: Send {
    fn resize(&mut self, width: u32, height: u32, cell: CellSize) -> Result<(), String>;
    fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String>;
    fn paste(&mut self, text: &str) -> Result<(), String>;
    fn scroll(&mut self, action: TerminalScroll) -> Result<(), String>;
    fn frame(&mut self) -> TerminalFrame;
    /// OSC 7 directory, with a foreground-process fallback while the PTY is open.
    fn working_directory(&self) -> Option<String>;
    fn drain_effects(&self) -> Vec<ControlEvent>;
    fn close(&mut self) -> Result<(), String>;
}

/// Backend-neutral movement through the terminal engine's authoritative
/// history. Positive line deltas move toward older output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalScroll {
    Lines(i32),
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// Explicit spawn configuration permits deterministic, GUI-free PTY tests.
pub(crate) struct SessionOptions {
    pub shell: Option<String>,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            working_directory: default_working_directory(),
        }
    }
}

#[derive(Clone)]
struct Listener {
    bridge: Arc<EventBridge>,
    writer: Arc<Mutex<Option<Sender<Msg>>>>,
    size: Arc<Mutex<WindowSize>>,
}

impl Listener {
    fn reply(&self, text: String) {
        if let Some(writer) = self.writer.lock().expect("PTY writer poisoned").as_ref() {
            let _ = writer.send(Msg::Input(Cow::Owned(text.into_bytes())));
        }
    }

    fn dispatch(&self, event: RioEvent) {
        // The parser invokes this while holding the terminal lock. Never
        // acquire that lock here, including when answering terminal queries.
        match event {
            RioEvent::Title(title) | RioEvent::TitleWithSubtitle(title, _) => {
                self.bridge.control(ControlEvent::Title(title));
            }
            RioEvent::ResetTitle => self.bridge.control(ControlEvent::Title(String::new())),
            RioEvent::Bell => self.bridge.control(ControlEvent::Bell),
            RioEvent::ClipboardStore(_, text) => self.bridge.clipboard_write(text),
            // Deny OSC 52 reads by default. Printing an escape sequence must
            // not grant applications access to the user's system clipboard.
            RioEvent::ClipboardLoad(_, _, format) => self.reply(format("")),
            RioEvent::PtyWrite(_, text) => self.reply(text),
            RioEvent::TextAreaSizeRequest(_, format) => {
                let size = *self.size.lock().expect("PTY size poisoned");
                self.reply(format(size));
            }
            // Color queries are not qualified until the host can supply the
            // exact configured renderer palette. Do not invent a default RGB
            // answer or re-lock Crosswords from its own parser callback.
            RioEvent::ColorRequest(..) => self.bridge.wake(),
            RioEvent::ChildExited(_, status) => {
                self.writer.lock().expect("PTY writer poisoned").take();
                self.bridge.control(ControlEvent::ChildExited(status));
                self.bridge.close();
            }
            RioEvent::CloseTerminal(_) | RioEvent::Exit => self.bridge.close(),
            _ => self.bridge.wake(),
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: RioEvent, _: WindowId) {
        self.dispatch(event);
    }
    fn send_event_with_high_priority(&self, event: RioEvent, _: WindowId) {
        self.dispatch(event);
    }
}

fn next_sampling_token(counter: &mut u64) -> SamplingToken {
    *counter = counter
        .checked_add(1)
        .expect("sampling token space exhausted");
    SamplingToken(*counter)
}

fn window_size(size: TerminalSize) -> WindowSize {
    WindowSize {
        cols: size.cols,
        rows: size.rows,
        width: size.pixels_width.min(u32::from(u16::MAX)) as u16,
        height: size.pixels_height.min(u32::from(u16::MAX)) as u16,
    }
}

fn grid_size(size: WindowSize) -> CrosswordsSize {
    CrosswordsSize {
        columns: usize::from(size.cols),
        screen_lines: usize::from(size.rows),
        width: u32::from(size.width),
        height: u32::from(size.height),
        square_width: u32::from(size.width) / u32::from(size.cols),
        square_height: u32::from(size.height) / u32::from(size.rows),
    }
}

type IoThread = JoinHandle<(Machine<Pty, Listener>, State)>;

pub struct RioTerminalSession {
    terminal: Arc<FairMutex<Crosswords<Listener>>>,
    listener: Listener,
    io_thread: Option<IoThread>,
    next_sampling_token: u64,
    #[cfg(unix)]
    shell_pid: u32,
    #[cfg(unix)]
    main_fd: std::os::fd::RawFd,
}

impl RioTerminalSession {
    pub fn new(
        width: u32,
        height: u32,
        cell: CellSize,
    ) -> Result<(Self, async_channel::Receiver<()>), String> {
        Self::with_options(
            terminal_size(width, height, cell),
            SessionOptions::default(),
        )
    }

    pub(crate) fn with_options(
        size: TerminalSize,
        options: SessionOptions,
    ) -> Result<(Self, async_channel::Receiver<()>), String> {
        let size = window_size(size);
        if size.cols == 0 || size.rows == 0 {
            return Err("terminal dimensions must contain at least one cell".into());
        }
        let (bridge, wake_rx) = EventBridge::new();
        let listener = Listener {
            bridge,
            writer: Arc::new(Mutex::new(None)),
            size: Arc::new(Mutex::new(size)),
        };
        let terminal = Arc::new(FairMutex::new(Crosswords::new(
            grid_size(size),
            CursorShape::Block,
            listener.clone(),
            WindowId::from(0),
            0,
            10_000,
        )));
        // Do not select Rio's frontend terminfo: Cuetty has not wired its
        // graphics, extended keyboard, or mouse frontend protocol support.
        let environment = Some(vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            // A terminal window is a new top-level shell session, regardless
            // of the shell depth of the process which launched Cuetty. Shells
            // increment this inherited value as they initialize.
            ("SHLVL".into(), "0".into()),
        ]);
        #[cfg(unix)]
        let pty = teletypewriter::create_pty_with_spawn(
            options.shell.as_deref(),
            options.args,
            &options.working_directory,
            environment,
            size.cols,
            size.rows,
            size.width,
            size.height,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        let pty = teletypewriter::create_pty(
            options.shell.as_deref(),
            options.args,
            &options.working_directory,
            environment,
            size.cols,
            size.rows,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        let shell_pid = *pty.child.pid as u32;
        #[cfg(unix)]
        let main_fd = *pty.child.id;
        let machine = Machine::new(
            Arc::clone(&terminal),
            pty,
            listener.clone(),
            WindowId::from(0),
            0,
        )
        .map_err(|error| error.to_string())?;
        *listener.writer.lock().expect("PTY writer poisoned") = Some(machine.channel());
        let io_thread = Some(machine.spawn());
        Ok((
            Self {
                terminal,
                listener,
                io_thread,
                next_sampling_token: 0,
                #[cfg(unix)]
                shell_pid,
                #[cfg(unix)]
                main_fd,
            },
            wake_rx,
        ))
    }

    fn send(&self, message: Msg) -> Result<(), String> {
        self.listener
            .writer
            .lock()
            .expect("PTY writer poisoned")
            .as_ref()
            .ok_or_else(|| "terminal session is closed".to_string())?
            .send(message)
            .map_err(|error| format!("terminal PTY channel closed: {error}"))
    }

    fn ensure_open(&self) -> Result<(), String> {
        if self.io_thread.as_ref().is_none_or(JoinHandle::is_finished)
            || self
                .listener
                .writer
                .lock()
                .expect("PTY writer poisoned")
                .is_none()
        {
            Err("terminal session is closed or its child has exited".into())
        } else {
            Ok(())
        }
    }

    fn write(&self, bytes: Vec<u8>) -> Result<(), String> {
        self.send(Msg::Input(Cow::Owned(bytes)))?;
        let mut terminal = self.terminal.lock();
        terminal.scroll_display(Scroll::Bottom);
        terminal.selection = None;
        Ok(())
    }
}

impl TerminalSession for RioTerminalSession {
    fn resize(&mut self, width: u32, height: u32, cell: CellSize) -> Result<(), String> {
        self.ensure_open()?;
        let size = window_size(terminal_size(width, height, cell));
        if *self.listener.size.lock().expect("PTY size poisoned") != size {
            self.terminal.lock().resize(grid_size(size));
            *self.listener.size.lock().expect("PTY size poisoned") = size;
            self.send(Msg::Resize(size))?;
            self.listener.bridge.wake();
        }
        Ok(())
    }

    fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String> {
        self.ensure_open()?;
        let bytes = {
            let terminal = self.terminal.lock();
            encode_key(event, terminal.mode(), terminal.modify_other_keys())
        };
        match bytes {
            Some(bytes) => self.write(bytes).map(|()| true),
            None => Ok(false),
        }
    }

    fn paste(&mut self, text: &str) -> Result<(), String> {
        self.ensure_open()?;
        if text.is_empty() {
            return Ok(());
        }
        let bytes = encode_paste(text, self.terminal.lock().mode());
        self.write(bytes)
    }

    fn scroll(&mut self, action: TerminalScroll) -> Result<(), String> {
        self.ensure_open()?;
        let action = match action {
            TerminalScroll::Lines(lines) => Scroll::Delta(lines),
            TerminalScroll::PageUp => Scroll::PageUp,
            TerminalScroll::PageDown => Scroll::PageDown,
            TerminalScroll::Top => Scroll::Top,
            TerminalScroll::Bottom => Scroll::Bottom,
        };
        self.terminal.lock().scroll_display(action);
        self.listener.bridge.wake();
        Ok(())
    }

    fn frame(&mut self) -> TerminalFrame {
        let token = next_sampling_token(&mut self.next_sampling_token);
        let mut terminal = self.terminal.lock();
        let frame = snapshot(&terminal, token);
        terminal.reset_damage();
        terminal.damage_event_in_flight = false;
        frame
    }

    fn working_directory(&self) -> Option<String> {
        let reported = self
            .terminal
            .lock()
            .current_directory
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        if reported.is_some() {
            return reported;
        }
        #[cfg(unix)]
        if self
            .io_thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            return teletypewriter::foreground_process_path(self.main_fd, self.shell_pid)
                .ok()
                .map(|path| path.to_string_lossy().into_owned());
        }
        None
    }

    fn drain_effects(&self) -> Vec<ControlEvent> {
        self.listener.bridge.drain()
    }

    fn close(&mut self) -> Result<(), String> {
        let Some(io_thread) = self.io_thread.take() else {
            return Ok(());
        };
        if let Some(writer) = self
            .listener
            .writer
            .lock()
            .expect("PTY writer poisoned")
            .take()
        {
            let _ = writer.send(Msg::Shutdown);
        }
        // Own the returned Machine until its PTY has completed Rio's bounded
        // shutdown and reap path, without joining on the GPUI thread. The
        // pinned PR #1927 lifecycle fix retires reaped PIDs before signalling
        // and escalates a running child from SIGHUP to SIGKILL after its grace
        // period. This does not guarantee termination of every descendant
        // process which independently detached from the PTY session.
        std::thread::spawn(move || {
            let _ = io_thread.join();
        });
        Ok(())
    }
}

impl Drop for RioTerminalSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Rio's process-wide Unix child notifications must not overlap between
    // these host integration probes. Recover poison so one failed regression
    // does not turn every later probe into an unrelated mutex failure.
    #[cfg(unix)]
    static REAL_PTY_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    fn serial_pty_test() -> std::sync::MutexGuard<'static, ()> {
        REAL_PTY_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn sampling_tokens_are_strictly_monotonic_and_not_revisions() {
        let mut counter = 0;
        assert_eq!(next_sampling_token(&mut counter), SamplingToken(1));
        assert_eq!(next_sampling_token(&mut counter), SamplingToken(2));
    }

    #[test]
    fn listener_preserves_exit_status_and_deduplicates_close() {
        let (bridge, _) = EventBridge::new();
        let listener = Listener {
            bridge: bridge.clone(),
            writer: Arc::new(Mutex::new(None)),
            size: Arc::new(Mutex::new(WindowSize::default())),
        };
        listener.dispatch(RioEvent::Title("shell".into()));
        listener.dispatch(RioEvent::Bell);
        listener.dispatch(RioEvent::ChildExited(0, Some(1792)));
        listener.dispatch(RioEvent::CloseTerminal(0));
        assert_eq!(
            bridge.drain(),
            vec![
                ControlEvent::Title("shell".into()),
                ControlEvent::Bell,
                ControlEvent::ChildExited(Some(1792)),
                ControlEvent::Close,
            ]
        );
    }

    #[test]
    fn invalid_dimensions_are_rejected_before_spawn() {
        let result = RioTerminalSession::with_options(
            TerminalSize {
                cols: 0,
                rows: 1,
                pixels_width: 0,
                pixels_height: 16,
            },
            SessionOptions::default(),
        );
        assert!(result.is_err());
    }

    #[cfg(unix)]
    fn fixture(args: &[&str]) -> RioTerminalSession {
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/m0-pty-child.sh"
        );
        let (session, _) = RioTerminalSession::with_options(
            TerminalSize {
                cols: 100,
                rows: 20,
                pixels_width: 800,
                pixels_height: 320,
            },
            SessionOptions {
                shell: Some("/bin/sh".into()),
                args: std::iter::once(script)
                    .chain(args.iter().copied())
                    .map(String::from)
                    .collect(),
                working_directory: None,
            },
        )
        .expect("deterministic child should spawn");
        session
    }

    #[cfg(unix)]
    fn wait_for_text(session: &mut RioTerminalSession, expected: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let frame = session.frame();
            frame
                .validate()
                .expect("PTY output must produce a valid frame");
            let text = frame_text(&frame);
            if text.contains(expected) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing {expected:?} in {text:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn frame_text(frame: &TerminalFrame) -> String {
        frame
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .filter_map(|cell| cell.text.as_deref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_retains_final_output_and_exit_status() {
        use std::os::unix::process::ExitStatusExt;
        let _serial = serial_pty_test();
        let mut session = fixture(&["exit"]);
        wait_for_text(&mut session, "M0-FINAL-OUTPUT");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut effects = Vec::new();
        while !effects.contains(&ControlEvent::Close) {
            effects.extend(session.drain_effects());
            assert!(
                std::time::Instant::now() < deadline,
                "child exit was not delivered"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let status = effects
            .iter()
            .find_map(|event| match event {
                ControlEvent::ChildExited(Some(status)) => Some(*status),
                _ => None,
            })
            .expect("Unix child exit must preserve raw wait status");
        assert_eq!(std::process::ExitStatus::from_raw(status).code(), Some(23));
        assert_eq!(
            effects
                .iter()
                .filter(|event| **event == ControlEvent::Close)
                .count(),
            1
        );
        wait_for_text(&mut session, "M0-FINAL-OUTPUT");
        assert!(session.paste("after exit").is_err());
        assert!(
            session
                .input(TerminalKeyEvent {
                    key: super::super::input::KeyInput::Enter,
                    modifiers: super::super::input::InputModifiers::default(),
                    repeat: false,
                })
                .is_err()
        );
        assert!(
            session
                .resize(
                    800,
                    320,
                    CellSize {
                        width: 8,
                        height: 16
                    }
                )
                .is_err()
        );
        session.close().expect("close should succeed");
        session.close().expect("close should be idempotent");
        assert!(session.paste("x").is_err());
        assert!(
            session
                .resize(
                    800,
                    320,
                    CellSize {
                        width: 8,
                        height: 16
                    }
                )
                .is_err()
        );
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_paste_obeys_live_bracketed_mode() {
        let _serial = serial_pty_test();
        for (mode, count, expected) in [
            ("raw", "3", "M0-BYTES:616263"),
            ("bracketed", "15", "M0-BYTES:1b5b3230307e6162631b5b3230317e"),
        ] {
            let mut session = fixture(&[mode, count]);
            wait_for_text(&mut session, "M0-READY");
            session.paste("abc").expect("paste should reach the PTY");
            wait_for_text(&mut session, expected);
            session.close().expect("close should succeed");
        }
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_input_encodes_printable_control_and_enter() {
        use super::super::input::{InputModifiers, KeyInput};
        let _serial = serial_pty_test();
        let mut session = fixture(&["raw", "4"]);
        wait_for_text(&mut session, "M0-READY");
        for (key, modifiers) in [
            (KeyInput::Character('a'), InputModifiers::default()),
            (KeyInput::Character('c'), InputModifiers::CONTROL),
            (KeyInput::Character('u'), InputModifiers::CONTROL),
            (KeyInput::Enter, InputModifiers::default()),
        ] {
            assert!(
                session
                    .input(TerminalKeyEvent {
                        key,
                        modifiers,
                        repeat: false
                    })
                    .expect("input should reach the PTY")
            );
        }
        wait_for_text(&mut session, "M0-BYTES:6103150d");
        session.close().expect("close should succeed");
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_input_obeys_negotiated_extended_keyboard_modes() {
        use super::super::input::{InputModifiers, KeyInput};
        let _serial = serial_pty_test();
        for (mode, count, expected) in [
            ("kitty", "13", "M0-BYTES:611b5b39393b35751b5b313375"),
            ("modify-other-keys", "11", "M0-BYTES:611b5b32373b353b39397e"),
        ] {
            let mut session = fixture(&[mode, count]);
            wait_for_text(&mut session, "M0-READY");
            for (key, modifiers) in [
                (KeyInput::Character('a'), InputModifiers::default()),
                (KeyInput::Character('c'), InputModifiers::CONTROL),
            ] {
                assert!(
                    session
                        .input(TerminalKeyEvent {
                            key,
                            modifiers,
                            repeat: false,
                        })
                        .expect("negotiated keyboard input should reach the PTY")
                );
            }
            if mode == "kitty" {
                assert!(
                    session
                        .input(TerminalKeyEvent {
                            key: KeyInput::Enter,
                            modifiers: InputModifiers::default(),
                            repeat: false,
                        })
                        .expect("Kitty enter should reach the PTY")
                );
            }
            wait_for_text(&mut session, expected);
            session.close().expect("close should succeed");
        }
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_normalizes_inherited_shell_level() {
        let _serial = serial_pty_test();
        let mut session = fixture(&["shell-level"]);
        wait_for_text(&mut session, "M0-SHLVL:1");
        session.close().expect("close should succeed");
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_resize_updates_both_grid_and_child() {
        let _serial = serial_pty_test();
        let mut session = fixture(&["resize"]);
        wait_for_text(&mut session, "M0-READY");
        session
            .resize(
                400,
                160,
                CellSize {
                    width: 8,
                    height: 16,
                },
            )
            .expect("resize should succeed");
        let frame = session.frame();
        assert_eq!(frame.dimensions.columns, 50);
        assert_eq!(frame.dimensions.rows, 10);
        session
            .write(b"size\r".to_vec())
            .expect("fixture command should reach the PTY");
        wait_for_text(&mut session, "M0-SIZE:10 50");
        session.close().expect("close should succeed");
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_scrolls_authoritative_history_and_returns_to_bottom() {
        let _serial = serial_pty_test();
        let mut session = fixture(&["history"]);
        wait_for_text(&mut session, "M0-READY");

        session
            .scroll(TerminalScroll::Top)
            .expect("history should scroll to the oldest retained row");
        let top = session.frame();
        assert!(frame_text(&top).contains("M0-HISTORY-01"));
        assert!(top.viewport_offset.is_some_and(|offset| offset > 0));

        session
            .scroll(TerminalScroll::Bottom)
            .expect("history should return to the live viewport");
        let bottom = session.frame();
        assert!(frame_text(&bottom).contains("M0-READY"));
        assert_eq!(bottom.viewport_offset, Some(0));
        session.close().expect("close should succeed");
    }

    #[test]
    #[cfg(unix)]
    fn final_output_survives_snapshot_lock_contention() {
        use std::os::unix::process::ExitStatusExt;
        let _serial = serial_pty_test();
        let mut session = fixture(&["raw", "3"]);
        wait_for_text(&mut session, "M0-READY");

        let terminal = Arc::clone(&session.terminal);
        let lock = terminal.lock();
        // Send on the production Machine channel without the UI's subsequent
        // scroll-to-bottom lock acquisition. Hold the snapshot lock long
        // enough for this tiny child to write its reply and exit. This is the
        // intentional race trigger, not a sleep used to assert completion.
        session
            .send(Msg::Input(Cow::Owned(b"abc".to_vec())))
            .expect("fixture bytes should reach the PTY");
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(lock);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut effects = Vec::new();
        while !effects.contains(&ControlEvent::Close) {
            effects.extend(session.drain_effects());
            assert!(
                std::time::Instant::now() < deadline,
                "fixture did not exit; effects: {effects:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let status = effects
            .iter()
            .find_map(|event| match event {
                ControlEvent::ChildExited(Some(status)) => Some(*status),
                _ => None,
            })
            .expect("fixture must report its raw Unix exit status");
        assert_eq!(std::process::ExitStatus::from_raw(status).code(), Some(0));
        // Assert the correct contract. No should_panic or inverted assertion:
        // this ignored regression must become green when Rio fixes the loss.
        wait_for_text(&mut session, "M0-BYTES:616263");
        session.close().expect("close should succeed");
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a real host PTY; run real_session tests with --ignored"]
    fn real_session_close_eventually_reaps_the_child() {
        let _serial = serial_pty_test();
        let mut session = fixture(&["hold"]);
        wait_for_text(&mut session, "M0-READY");
        let pid = session.shell_pid as libc::pid_t;

        session
            .close()
            .expect("close should begin bounded teardown");
        session
            .close()
            .expect("repeated close should remain idempotent");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "PTY child {pid} was not reaped after close"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
