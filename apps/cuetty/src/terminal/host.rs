use super::events::{ControlEvent, EventBridge};
use super::input::{InputModifiers, KeyInput, TerminalKeyEvent};
use super::model::{SamplingToken, TerminalFrame};
use super::rio_adapter::snapshot;
use super::sizing::{CellSize, terminal_size};
use librio::{
    Action, ClipboardType, Engine, KeyEvent, RenderState, Surface, SurfaceDelegate, SurfaceDesc,
    SurfaceId,
};
use std::sync::Arc;

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
        RioTerminalSession::new(width, height, cell)
            .map(|(session, wake_rx)| SessionStart {
                session: Box::new(session),
                wake_rx,
            })
            .map_err(|error| error.to_string())
    }
}

type SessionStartError = Box<dyn std::error::Error + Send + Sync>;

pub trait TerminalSession: Send {
    fn resize(&mut self, width: u32, height: u32, cell: CellSize) -> Result<(), String>;
    fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String>;
    fn paste(&mut self, text: &str) -> Result<(), String>;
    fn frame(&mut self) -> TerminalFrame;
    /// Current shell directory reported by Rio (OSC 7, with Rio's OS fallback).
    fn working_directory(&self) -> Option<String>;
    fn drain_effects(&self) -> Vec<ControlEvent>;
    fn close(&mut self) -> Result<(), String>;
}

fn rio_key(event: TerminalKeyEvent) -> KeyEvent {
    let key = match event.key {
        KeyInput::Character(c) => librio::Key::Char(c),
        KeyInput::Enter => librio::Key::Enter,
        KeyInput::Tab => librio::Key::Tab,
        KeyInput::Backspace => librio::Key::Backspace,
        KeyInput::Escape => librio::Key::Escape,
        KeyInput::Up => librio::Key::Up,
        KeyInput::Down => librio::Key::Down,
        KeyInput::Left => librio::Key::Left,
        KeyInput::Right => librio::Key::Right,
        KeyInput::Home => librio::Key::Home,
        KeyInput::End => librio::Key::End,
        KeyInput::Delete => librio::Key::Delete,
    };
    let mut mods = librio::Modifiers::empty();
    if event.modifiers.contains(InputModifiers::SHIFT) {
        mods |= librio::Modifiers::SHIFT;
    }
    if event.modifiers.contains(InputModifiers::CONTROL) {
        mods |= librio::Modifiers::CTRL;
    }
    if event.modifiers.contains(InputModifiers::ALT) {
        mods |= librio::Modifiers::ALT;
    }
    if event.modifiers.contains(InputModifiers::SUPER) {
        mods |= librio::Modifiers::SUPER;
    }
    KeyEvent {
        action: if event.repeat {
            librio::KeyAction::Repeat
        } else {
            librio::KeyAction::Press
        },
        key: Some(key),
        mods,
        consumed_mods: librio::Modifiers::empty(),
        text: None,
        composing: false,
    }
}

struct Delegate {
    bridge: Arc<EventBridge>,
}

fn control_event_from_action(action: Action) -> Option<ControlEvent> {
    match action {
        Action::SetTitle { title, .. } => Some(ControlEvent::Title(title)),
        Action::RingBell => Some(ControlEvent::Bell),
        Action::CursorBlinkingChange | Action::Progress { .. } => None,
    }
}

fn next_sampling_token(counter: &mut u64) -> SamplingToken {
    *counter = counter
        .checked_add(1)
        .expect("sampling token space exhausted");
    SamplingToken(*counter)
}

impl SurfaceDelegate for Delegate {
    fn wakeup(&self, _: SurfaceId) {
        self.bridge.wake();
    }
    fn action(&self, _: SurfaceId, action: Action) {
        if let Some(event) = control_event_from_action(action) {
            self.bridge.control(event);
        } else {
            // These public actions carry no P0 host effect, but they can
            // change what the next sampled frame should display.
            self.bridge.wake();
        }
    }
    fn clipboard_write(&self, _: SurfaceId, kind: ClipboardType, text: String) {
        if matches!(kind, ClipboardType::Clipboard | ClipboardType::Selection) {
            self.bridge.clipboard_write(text);
        }
    }
    fn close_surface(&self, _: SurfaceId) {
        self.bridge.close();
    }
}

pub struct RioTerminalSession {
    surface: Option<Surface>,
    render_state: RenderState,
    bridge: Arc<EventBridge>,
    next_sampling_token: u64,
    last_size: Option<(u16, u16, u16, u16)>,
}

impl RioTerminalSession {
    pub fn new(
        width: u32,
        height: u32,
        cell: CellSize,
    ) -> Result<(Self, async_channel::Receiver<()>), SessionStartError> {
        let (bridge, wake_rx) = EventBridge::new();
        let engine = Engine::new(Arc::new(Delegate {
            bridge: bridge.clone(),
        }));
        let size = terminal_size(width, height, cell);
        let desc = SurfaceDesc {
            cols: size.cols,
            rows: size.rows,
            pixel_width: size.pixels_width.min(u16::MAX as u32) as u16,
            pixel_height: size.pixels_height.min(u16::MAX as u32) as u16,
            working_dir: default_working_directory(),
            ..SurfaceDesc::default()
        };
        let surface = engine.create_surface(&desc)?;
        let render_state = RenderState::new(&surface);
        Ok((
            Self {
                surface: Some(surface),
                render_state,
                bridge,
                next_sampling_token: 0,
                last_size: Some((desc.cols, desc.rows, desc.pixel_width, desc.pixel_height)),
            },
            wake_rx,
        ))
    }
}

impl TerminalSession for RioTerminalSession {
    fn resize(&mut self, width: u32, height: u32, cell: CellSize) -> Result<(), String> {
        let Some(surface) = self.surface.as_ref() else {
            return Err("terminal surface is closed".into());
        };
        let size = terminal_size(width, height, cell);
        let next = (
            size.cols,
            size.rows,
            size.pixels_width.min(u16::MAX as u32) as u16,
            size.pixels_height.min(u16::MAX as u32) as u16,
        );
        if self.last_size != Some(next) {
            surface.resize(next.0, next.1, next.2, next.3);
            self.last_size = Some(next);
        }
        Ok(())
    }
    fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String> {
        self.surface
            .as_ref()
            .map(|surface| surface.key(&rio_key(event)))
            .ok_or_else(|| "terminal surface is closed".into())
    }
    fn paste(&mut self, text: &str) -> Result<(), String> {
        if let Some(surface) = self.surface.as_ref() {
            surface.text(text);
            Ok(())
        } else {
            Err("terminal surface is closed".into())
        }
    }
    fn frame(&mut self) -> TerminalFrame {
        self.render_state.update();
        let sampling_token = next_sampling_token(&mut self.next_sampling_token);
        snapshot(&self.render_state, sampling_token)
    }
    fn working_directory(&self) -> Option<String> {
        self.surface.as_ref().and_then(Surface::working_dir)
    }
    fn drain_effects(&self) -> Vec<ControlEvent> {
        self.bridge.drain()
    }
    fn close(&mut self) -> Result<(), String> {
        self.surface.take();
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
    #[test]
    fn rio_mapping_preserves_modifier_semantics_and_repeat() {
        let event = rio_key(TerminalKeyEvent {
            key: KeyInput::Left,
            modifiers: InputModifiers::CONTROL,
            repeat: true,
        });
        assert_eq!(event.key, Some(librio::Key::Left));
        assert_eq!(event.action, librio::KeyAction::Repeat);
        assert!(event.mods.contains(librio::Modifiers::CTRL));
        assert!(!event.mods.contains(librio::Modifiers::SHIFT));

        let printable = rio_key(TerminalKeyEvent {
            key: KeyInput::Character('x'),
            modifiers: InputModifiers::ALT,
            repeat: false,
        });
        assert_eq!(printable.key, Some(librio::Key::Char('x')));
        assert!(printable.mods.contains(librio::Modifiers::ALT));
    }

    #[test]
    fn public_rio_title_and_bell_actions_map_to_ordered_host_effects() {
        assert_eq!(
            control_event_from_action(Action::SetTitle {
                title: "shell".into(),
                subtitle: Some("project".into()),
            }),
            Some(ControlEvent::Title("shell".into()))
        );
        assert_eq!(
            control_event_from_action(Action::RingBell),
            Some(ControlEvent::Bell)
        );
        assert_eq!(
            control_event_from_action(Action::CursorBlinkingChange),
            None
        );
    }

    #[test]
    fn sampling_tokens_are_strictly_monotonic_and_not_revisions() {
        let mut counter = 0;
        let first = next_sampling_token(&mut counter);
        let second = next_sampling_token(&mut counter);
        assert!(second > first);
        assert_eq!(first, SamplingToken(1));
        assert_eq!(second, SamplingToken(2));
    }

    #[test]
    fn new_sessions_use_home_as_the_working_directory() {
        let home = std::env::var("HOME").expect("tests run with HOME configured");
        assert_eq!(default_working_directory(), Some(home));
    }

    #[test]
    fn rio_resize_refreshes_the_sampled_grid_dimensions() {
        // Rio intentionally uses macOS's /usr/bin/login for the default shell.
        // Minimal Nix build sandboxes do not expose that host path, so keep
        // the live PTY assertion for macOS environments that can actually
        // spawn the production shell and leave the pure sizing coverage to
        // the renderer/terminal-size tests below it.
        if std::env::var_os("CUETTY_SKIP_PTY_TEST").is_some()
            || (cfg!(target_os = "macos") && !std::path::Path::new("/usr/bin/login").exists())
        {
            return;
        }
        let cell = CellSize {
            width: 8,
            height: 16,
        };
        let (mut session, _wake_rx) = RioTerminalSession::new(800, 320, cell)
            .expect("Rio should create a test terminal session");

        let initial = session.frame();
        assert_eq!(initial.dimensions.columns, 100);
        assert_eq!(initial.dimensions.rows, 20);

        session
            .resize(400, 320, cell)
            .expect("Rio should accept a smaller viewport");
        let resized = session.frame();
        assert_eq!(resized.dimensions.columns, 50);
        assert_eq!(resized.dimensions.rows, 20);
    }
}
