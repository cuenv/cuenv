use super::chrome::{ChromeAssets, render_shell_message, resolve_ui_font, traffic_light_position};
use super::config::TerminalConfig;
use super::events::ControlEvent;
use super::host::{
    RioTerminalFactory, TerminalScroll, TerminalSession, TerminalSessionFactory, TerminalWheel,
};
use super::input::{InputModifiers, KeyInput, TerminalKeyEvent};
use super::interaction::{CellHitTest, InteractionState, SearchKey};
use super::model::{CursorShape, Known, Rgb, TerminalCell, TerminalFrame, UnderlineStyle};
#[cfg(test)]
use super::model::{CursorState, SamplingToken, TerminalDimensions, TerminalRow};
use super::overlay::{CellOverlaySpan, OverlayKind, TerminalOverlays};
use super::presentation::{TerminalMetrics, TerminalTheme, gpui_rgb};
use super::sizing::CellSize;
use super::tab_label::{PaneActivity, TabLabelSources, resolve_tab_label};
use super::workspace_shell::{
    ShellAction, ShellError, TabDirection, TabNumber, WorkspaceShell, WorkspaceShortcut,
};
use crate::integrations::cuenv::{
    ChordModifiers, ConfigSource, CuenvPaneState, CuenvProvider, CuenvTask, CuenvWatcher,
    Evaluation, KeyChord, NativeCuenvProvider, SidebarMode, TaskEvaluation, WindowSidebar,
    evaluate_within,
};
use crate::workspace::{LayoutRect, MinSizePolicy, Tab, TabId};
use gpui::{
    App, AppContext, Application, BorderStyle, Bounds, ClipboardItem, Context, Decorations, Edges,
    Element, FocusHandle, Font, FontFeatures, FontStyle, FontWeight, Hsla, InteractiveElement,
    IntoElement, KeyBinding, Menu, MenuItem, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ScrollDelta, ScrollHandle,
    ScrollWheelEvent, SharedString, Size, StrikethroughStyle, Styled, SystemMenuType, TextRun,
    Timer, UnderlineStyle as GpuiUnderlineStyle, Window, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, actions, canvas, div, point, px, quad, size,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

actions!([Quit]);

struct RenderRequest<'a> {
    frame: &'a TerminalFrame,
    metrics: &'a TerminalMetrics,
    theme: &'a TerminalTheme,
    focused: bool,
    overlays: TerminalOverlays,
}

trait TerminalRenderer: Send + Sync {
    fn render(&self, request: RenderRequest<'_>) -> gpui::AnyElement;
}

struct GridRenderer;
impl TerminalRenderer for GridRenderer {
    fn render(&self, request: RenderRequest<'_>) -> gpui::AnyElement {
        TerminalGrid::new(
            request.frame,
            request.metrics,
            request.theme,
            request.focused,
            request.overlays,
        )
        .into_any_element()
    }
}

fn uses_terminal_surface_background(cell: &TerminalCell) -> bool {
    !cell.style.inverse && matches!(cell.background, super::model::Color::DefaultBackground)
}

/// Fixed-layout terminal surface informed by Termy/Okena design patterns; no
/// source was copied. This deliberately avoids a GPUI element per cell:
/// background spans and compatible glyphs are painted in row batches.
struct TerminalGrid {
    frame: TerminalFrame,
    metrics: TerminalMetrics,
    theme: TerminalTheme,
    focused: bool,
    overlays: TerminalOverlays,
}

impl TerminalGrid {
    fn new(
        frame: &TerminalFrame,
        metrics: &TerminalMetrics,
        theme: &TerminalTheme,
        focused: bool,
        overlays: TerminalOverlays,
    ) -> Self {
        Self {
            frame: frame.clone(),
            metrics: metrics.clone(),
            theme: theme.clone(),
            focused,
            overlays,
        }
    }

    fn overlay_color(&self, kind: OverlayKind) -> Rgb {
        match kind {
            OverlayKind::Selection => self.theme.selection,
            OverlayKind::CurrentSearchMatch => self.theme.current_search_match,
        }
    }

    fn paint_row_overlays(
        &self,
        origin: gpui::Point<Pixels>,
        row: usize,
        overlays: &[CellOverlaySpan],
        window: &mut Window,
    ) {
        for span in overlays.iter().filter(|span| span.row == row) {
            window.paint_quad(quad(
                self.cell_bounds(
                    origin,
                    span.row,
                    span.start_column,
                    span.end_column - span.start_column,
                ),
                px(0.0),
                gpui::rgb(gpui_rgb(self.overlay_color(span.kind))),
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }

    fn layer_at(&self, overlays: &[CellOverlaySpan], position: CellPosition) -> CellLayer {
        cell_layer(
            cursor_style_at(&self.frame, self.focused, position.row, position.column),
            overlays,
            position,
        )
    }

    fn font_for(&self, cell: &TerminalCell) -> Font {
        Font {
            family: self.metrics.font.family.clone(),
            // Terminal columns must not be changed by contextual or standard ligatures.
            features: FontFeatures(std::sync::Arc::new(vec![
                ("calt".into(), 0),
                ("liga".into(), 0),
                ("clig".into(), 0),
            ])),
            fallbacks: self.metrics.font.fallbacks.clone(),
            weight: if cell.style.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if cell.style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        }
    }

    fn cell_bounds(
        &self,
        origin: gpui::Point<Pixels>,
        row: usize,
        start: usize,
        len: usize,
    ) -> Bounds<Pixels> {
        let grid_cell = self.metrics.render_cell();
        Bounds {
            origin: point(
                origin.x + grid_cell.width * start as f32,
                origin.y + grid_cell.height * row as f32,
            ),
            size: Size {
                width: grid_cell.width * len as f32,
                height: grid_cell.height,
            },
        }
    }

    fn paint_cursor(
        &self,
        origin: gpui::Point<Pixels>,
        row: usize,
        block_layer: bool,
        window: &mut Window,
    ) {
        let Some(style) = cursor_style_at(
            &self.frame,
            self.focused,
            row,
            self.frame
                .cursor
                .position
                .map_or(usize::MAX, |(_, col)| col as usize),
        ) else {
            return;
        };
        let Some((_, col)) = self.frame.cursor.position else {
            return;
        };
        if col as usize >= self.frame.dimensions.columns as usize {
            return;
        }
        if matches!(style, HostCursorStyle::Block) != block_layer {
            return;
        }
        let bounds = self.cell_bounds(origin, row, col as usize, 1);
        match style {
            HostCursorStyle::Block => window.paint_quad(quad(
                bounds,
                px(0.0),
                gpui::rgb(gpui_rgb(self.theme.cursor)),
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            )),
            HostCursorStyle::Underline => window.paint_quad(quad(
                Bounds {
                    origin: point(bounds.origin.x, bounds.bottom() - px(2.0)),
                    size: Size {
                        width: bounds.size.width,
                        height: px(2.0),
                    },
                },
                px(0.0),
                gpui::rgb(gpui_rgb(self.theme.cursor)),
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            )),
            HostCursorStyle::Bar => window.paint_quad(quad(
                Bounds {
                    origin: bounds.origin,
                    size: Size {
                        width: px(2.0),
                        height: bounds.size.height,
                    },
                },
                px(0.0),
                gpui::rgb(gpui_rgb(self.theme.cursor)),
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            )),
        }
    }
}

impl IntoElement for TerminalGrid {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalGrid {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, ()) {
        let grid_cell = self.metrics.render_cell();
        let size = Size {
            width: grid_cell.width * self.frame.dimensions.columns.max(1) as f32,
            height: grid_cell.height * self.frame.dimensions.rows.max(1) as f32,
        };
        (
            window.request_layout(
                gpui::Style {
                    size: gpui::Size {
                        width: gpui::Length::Definite(gpui::DefiniteLength::Absolute(
                            gpui::AbsoluteLength::Pixels(size.width),
                        )),
                        height: gpui::Length::Definite(gpui::DefiniteLength::Absolute(
                            gpui::AbsoluteLength::Pixels(size.height),
                        )),
                    },
                    ..Default::default()
                },
                [],
                cx,
            ),
            (),
        )
    }
    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let grid_cell = self.metrics.render_cell();
        let overlays = self.overlays.spans(&self.frame);
        for row in 0..self.frame.dimensions.rows as usize {
            let cells = frame_row(&self.frame, row);
            if cells.is_empty() {
                break;
            }
            // Merge adjacent cells with the same resolved background into one quad.
            let mut start = 0;
            while start < cells.len() {
                let (_, background) = colors(&cells[start], &self.theme, CellLayer::Cell);
                let uses_default_background = uses_terminal_surface_background(&cells[start]);
                let mut end = start + 1;
                while end < cells.len()
                    && colors(&cells[end], &self.theme, CellLayer::Cell).1 == background
                    && uses_default_background == uses_terminal_surface_background(&cells[end])
                {
                    end += 1;
                }
                // The terminal surface owns the single translucent default
                // background layer. Painting it again for every default cell
                // compounds alpha until wallpaper passthrough disappears.
                // Explicit TUI backgrounds stay opaque for contrast.
                if !uses_default_background {
                    window.paint_quad(quad(
                        self.cell_bounds(bounds.origin, row, start, end - start),
                        px(0.0),
                        gpui::rgb(gpui_rgb(background)),
                        Edges::default(),
                        Hsla::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
                start = end;
            }
            // Overlays are painted after semantic cell backgrounds but before
            // glyphs and cursor layers. This preserves terminal colours and
            // keeps both block and line cursors visible.
            self.paint_row_overlays(bounds.origin, row, &overlays, window);
            // A block cursor is deliberately below text; line cursors are
            // painted after glyphs so they remain visible.
            self.paint_cursor(bounds.origin, row, true, window);
            let mut start = 0;
            while start < cells.len() {
                let cell = &cells[start];
                // Spacers carry background and occupancy only.  In particular,
                // they are never shaped as a space glyph.
                if cell.text.is_none() {
                    start += 1;
                    continue;
                }
                let layer = self.layer_at(&overlays, CellPosition { row, column: start });
                let (foreground, _) = colors(cell, &self.theme, layer);
                let decoration_color = cell
                    .underline_color
                    .map(|color| self.theme.resolve(color))
                    .unwrap_or(foreground);
                let mut end = start + 1;
                while end < cells.len() {
                    let next = &cells[end];
                    if next.text.is_none() {
                        break;
                    }
                    let next_layer = self.layer_at(&overlays, CellPosition { row, column: end });
                    let (next_fg, _) = colors(next, &self.theme, next_layer);
                    if next_fg != foreground
                        || next.style.bold != cell.style.bold
                        || next.style.italic != cell.style.italic
                        || next.style.underline != cell.style.underline
                        || next.underline_color != cell.underline_color
                        || next.style.strikeout != cell.style.strikeout
                    {
                        break;
                    }
                    end += 1;
                }
                let text: SharedString = cells[start..end]
                    .iter()
                    .filter_map(|cell| cell.text.as_deref())
                    .collect::<String>()
                    .into();
                let underline = cell.style.underline.map(|style| GpuiUnderlineStyle {
                    thickness: px(if matches!(style, UnderlineStyle::Double) {
                        2.0
                    } else {
                        1.0
                    }),
                    color: Some(gpui::rgb(gpui_rgb(decoration_color)).into()),
                    wavy: matches!(style, UnderlineStyle::Curly),
                });
                let run = TextRun {
                    len: text.len(),
                    font: self.font_for(cell),
                    color: gpui::rgb(gpui_rgb(foreground)).into(),
                    background_color: None,
                    underline,
                    strikethrough: cell.style.strikeout.then_some(StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(gpui::rgb(gpui_rgb(foreground)).into()),
                    }),
                };
                let line = window.text_system().shape_line(
                    text,
                    self.metrics.font_size,
                    &[run],
                    Some(grid_cell.width),
                );
                let _ = line.paint(
                    point(
                        bounds.origin.x + grid_cell.width * start as f32,
                        bounds.origin.y + grid_cell.height * row as f32,
                    ),
                    grid_cell.height,
                    window,
                    cx,
                );
                start = end;
            }
            self.paint_cursor(bounds.origin, row, false, window);
        }
    }
}

fn frame_row(frame: &TerminalFrame, row: usize) -> &[TerminalCell] {
    frame.rows.get(row).map_or(&[], |row| row.cells.as_slice())
}

/// The pinned Rio API exposes cursor position but not shape or visibility.
/// While focused, P0 renders an unknown cursor as a visible block;
/// `Known(false)` suppresses it and known shapes are rendered exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCursorStyle {
    Block,
    Underline,
    Bar,
}

fn cursor_style_at(
    frame: &TerminalFrame,
    focused: bool,
    row: usize,
    column: usize,
) -> Option<HostCursorStyle> {
    focused
        .then_some(())
        .filter(|_| !matches!(frame.cursor.visible, Known::Known(false)))
        .filter(|_| frame.cursor.position == Some((row as u16, column as u16)))
        .map(|_| match frame.cursor.shape {
            Known::Known(CursorShape::Underline) => HostCursorStyle::Underline,
            Known::Known(CursorShape::Bar) => HostCursorStyle::Bar,
            Known::Known(CursorShape::Block) | Known::Unknown => HostCursorStyle::Block,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellPosition {
    row: usize,
    column: usize,
}

/// What decides a cell's colours: its own attributes, or an opaque layer painted
/// beneath its glyph that those attributes were not chosen to read against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellLayer {
    Cell,
    BlockCursor,
    CurrentSearchMatch,
}

/// A block cursor stays on top of a search match, matching the paint order. A
/// selection is not a layer here: its quiet tint is chosen to sit beneath any
/// cell foreground, so glyphs keep their own colours on it.
fn cell_layer(
    cursor: Option<HostCursorStyle>,
    overlays: &[CellOverlaySpan],
    position: CellPosition,
) -> CellLayer {
    let in_current_match = || {
        overlays.iter().any(|span| {
            span.kind == OverlayKind::CurrentSearchMatch
                && span.row == position.row
                && (span.start_column..span.end_column).contains(&position.column)
        })
    };
    if cursor == Some(HostCursorStyle::Block) {
        CellLayer::BlockCursor
    } else if in_current_match() {
        CellLayer::CurrentSearchMatch
    } else {
        CellLayer::Cell
    }
}

fn colors(cell: &TerminalCell, theme: &TerminalTheme, layer: CellLayer) -> (Rgb, Rgb) {
    match layer {
        CellLayer::BlockCursor => return (theme.cursor_text, theme.cursor),
        CellLayer::CurrentSearchMatch => {
            return (theme.current_search_match_text, theme.current_search_match);
        }
        CellLayer::Cell => {}
    }
    let mut foreground = theme.resolve(cell.foreground);
    let mut background = theme.resolve(cell.background);
    if cell.style.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.style.dim {
        foreground = Rgb(foreground.0 / 2, foreground.1 / 2, foreground.2 / 2);
    }
    if cell.style.hidden {
        foreground = background;
    }
    (foreground, background)
}

/// Focus only counts as terminal focus while the active workspace pane owns
/// the live Rio session. Multi-session actions are rejected before a second
/// pane can be rendered.
fn terminal_has_focus(window_has_focus: bool, active_pane_is_live: bool) -> bool {
    window_has_focus && active_pane_is_live
}

fn shell_notice(error: ShellError) -> String {
    format!("workspace action unavailable: {error}")
}

type ViewportBounds = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionGeneration(u64);

#[derive(Clone, Copy)]
struct CuenvUpdateTarget {
    pane: crate::workspace::PaneId,
    session_generation: SessionGeneration,
    config_generation: u64,
}

pub(super) struct PaneState {
    generation: SessionGeneration,
    session: Box<dyn TerminalSession>,
    frame: TerminalFrame,
    interaction: InteractionState,
    scroll_remainder: f32,
    /// What the terminal is doing, which is what its tab is named after.
    activity: PaneActivity,
    measured_bounds: Arc<Mutex<(u32, u32)>>,
    viewport_bounds: Arc<Mutex<ViewportBounds>>,
    error: Option<String>,
    pub(super) cuenv: CuenvPaneState,
    cuenv_source: Option<ConfigSource>,
    cuenv_watcher: Option<Box<dyn CuenvWatcher>>,
    cuenv_watch_retry_at: Option<std::time::Instant>,
}

impl PaneState {
    fn new(
        generation: SessionGeneration,
        mut session: Box<dyn TerminalSession>,
        (width, height): (u32, u32),
    ) -> Self {
        let frame = session.frame();
        Self {
            generation,
            session,
            frame,
            interaction: InteractionState::default(),
            scroll_remainder: 0.0,
            activity: PaneActivity::default(),
            measured_bounds: Arc::new(Mutex::new((width, height))),
            viewport_bounds: Arc::new(Mutex::new((0.0, 0.0, width as f32, height as f32))),
            error: None,
            cuenv: CuenvPaneState::default(),
            cuenv_source: None,
            cuenv_watcher: None,
            cuenv_watch_retry_at: None,
        }
    }

    /// Records what the terminal is running now.
    fn sample_foreground(&mut self, now: std::time::Instant) {
        let process = self.session.foreground_process();
        self.activity.sample(process.as_ref(), now);
    }

    /// Paste the task's command into this pane's shell. A task that needs
    /// arguments is only staged at the prompt, so it is not running yet. The
    /// periodic sample may be out of date, and it decides where the paste went.
    ///
    /// A shell in bracketed paste mode takes a pasted newline as text, so running
    /// the command takes an Enter of its own. That is for an interactive shell only:
    /// in a program or a shell running a script it could submit whatever was being
    /// held or answer a question, so there the command is left for the user to act on.
    fn launch_task(&mut self, command: &str, task: &CuenvTask) -> Result<TaskLaunch, String> {
        let now = std::time::Instant::now();
        self.sample_foreground(now);
        self.session.paste(command)?;
        let launch = if task.requires_parameters {
            TaskLaunch::AwaitingArguments
        } else if self.activity.at_prompt() {
            TaskLaunch::Started
        } else {
            TaskLaunch::AwaitingEnter
        };
        if launch == TaskLaunch::Started {
            self.session.input(TerminalKeyEvent {
                key: KeyInput::Enter,
                modifiers: InputModifiers::default(),
                repeat: false,
            })?;
            self.activity.launch_task(task.name.clone(), now);
        }
        self.activity.set_staged_hint(launch.hint(&task.name));
        Ok(launch)
    }

    /// Sends a key the user pressed to the terminal.
    fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String> {
        if acts_on_the_line(&event) {
            self.activity.set_staged_hint(None);
        }
        self.session.input(event)
    }

    /// What the sidebar's notice marker says for this pane. A broken configuration
    /// matters more than a reminder about a pasted task.
    pub(super) fn marker_notice(&self) -> Option<&str> {
        self.cuenv
            .notice
            .as_deref()
            .or(self.cuenv.task_notice.as_deref())
            .or(self.activity.staged_hint())
    }
}

/// How far launching a task got.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskLaunch {
    /// Pasted and submitted to a waiting shell.
    Started,
    /// Pasted for the user to complete, because the task has required parameters.
    AwaitingArguments,
    /// Pasted but not submitted, because no shell could be seen waiting.
    AwaitingEnter,
}

impl TaskLaunch {
    /// What the user has to do next, if anything.
    fn hint(self, task: &str) -> Option<String> {
        match self {
            Self::Started => None,
            Self::AwaitingArguments => Some(format!("Add arguments for '{task}' and press Enter")),
            Self::AwaitingEnter => Some(format!("Pasted '{task}'; press Enter to run it")),
        }
    }
}

/// Whether a key finishes with the line being edited, by submitting or abandoning it.
fn acts_on_the_line(event: &TerminalKeyEvent) -> bool {
    match event.key {
        KeyInput::Enter => true,
        KeyInput::Character('c' | 'C') => event.modifiers.contains(InputModifiers::CONTROL),
        _ => false,
    }
}

#[derive(Debug, PartialEq)]
enum RegistryError {
    Session(String),
    Workspace(ShellError),
    MissingSession(crate::workspace::PaneId),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(error) => write!(f, "{error}"),
            Self::Workspace(error) => write!(f, "{error}"),
            Self::MissingSession(pane) => write!(f, "no session is registered for pane {pane:?}"),
        }
    }
}

struct WakeOutcome {
    accepted: bool,
    clipboard: Vec<String>,
    should_close: bool,
}

/// Owns the workspace/session invariant independently of GPUI. The host
/// delegates creation, wake handling, and close ordering here so those
/// transitions can be tested with a fake session factory.
pub(super) struct SessionRegistry {
    pub(super) sessions: HashMap<crate::workspace::PaneId, PaneState>,
    factory: Arc<dyn TerminalSessionFactory>,
    next_generation: u64,
    pub(super) workspace: WorkspaceShell,
    pub(super) notice: Option<String>,
    close_window_requested: bool,
}

impl SessionRegistry {
    fn empty(factory: Arc<dyn TerminalSessionFactory>) -> Self {
        Self {
            sessions: HashMap::new(),
            factory,
            next_generation: 1,
            workspace: WorkspaceShell::empty(),
            notice: None,
            close_window_requested: false,
        }
    }

    fn start(
        factory: Arc<dyn TerminalSessionFactory>,
        width: u32,
        height: u32,
        cell: CellSize,
    ) -> Result<(Self, async_channel::Receiver<()>), RegistryError> {
        let start = factory
            .start(width, height, cell)
            .map_err(RegistryError::Session)?;
        let workspace = WorkspaceShell::new()
            .map_err(ShellError::from)
            .map_err(RegistryError::Workspace)?;
        let pane =
            workspace
                .active_pane()
                .ok_or(RegistryError::Workspace(ShellError::Workspace(
                    crate::workspace::Error::NoTabs,
                )))?;
        let mut sessions = HashMap::new();
        sessions.insert(
            pane,
            PaneState::new(SessionGeneration(1), start.session, (width, height)),
        );
        Ok((
            Self {
                sessions,
                factory,
                next_generation: 2,
                workspace,
                notice: None,
                close_window_requested: false,
            },
            start.wake_rx,
        ))
    }

    fn active_pane(&self) -> Option<crate::workspace::PaneId> {
        self.workspace.active_pane()
    }

    fn active_terminal_config(&self) -> TerminalConfig {
        self.active_pane()
            .and_then(|pane| self.sessions.get(&pane))
            .map(|state| state.cuenv.presentation.terminal.as_ref().clone())
            .unwrap_or_default()
    }

    fn start_tab(
        &mut self,
        width: u32,
        height: u32,
        cell: CellSize,
    ) -> Result<
        (
            crate::workspace::PaneId,
            SessionGeneration,
            async_channel::Receiver<()>,
        ),
        RegistryError,
    > {
        let mut start = self
            .factory
            .start(width, height, cell)
            .map_err(RegistryError::Session)?;
        let presentation = self
            .active_pane()
            .and_then(|pane| self.sessions.get(&pane))
            .map(|state| state.cuenv.presentation.carried_over())
            .unwrap_or_default();
        let target = match self.workspace.create_tab("Terminal") {
            Ok(target) => target,
            Err(error) => {
                let _ = start.session.close();
                return Err(RegistryError::Workspace(error));
            }
        };
        let generation = SessionGeneration(self.next_generation);
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("session generation exhausted");
        let mut state = PaneState::new(generation, start.session, (width, height));
        state.cuenv.presentation = presentation;
        self.sessions.insert(target.pane, state);
        self.notice = None;
        Ok((target.pane, generation, start.wake_rx))
    }

    fn apply_wake(
        &mut self,
        pane: crate::workspace::PaneId,
        generation: SessionGeneration,
        active_pane: Option<crate::workspace::PaneId>,
    ) -> WakeOutcome {
        let effects = match self.sessions.get_mut(&pane) {
            Some(state) if state.generation == generation => {
                let effects = state.session.drain_effects();
                // Rio publishes Close after its final PTY drain. Sample that
                // state before the caller removes the pane. This preserves
                // the final frame for consumers, not a visible exited tab:
                // handle_wake still closes the pane after this returns.
                state.frame = state.session.frame();
                effects
            }
            _ => {
                return WakeOutcome {
                    accepted: false,
                    clipboard: Vec::new(),
                    should_close: false,
                };
            }
        };
        let mut clipboard = Vec::new();
        let mut should_close = false;
        for effect in effects {
            match effect {
                ControlEvent::ClipboardWrite(text) if active_pane == Some(pane) => {
                    clipboard.push(text);
                }
                ControlEvent::ClipboardWrite(_) => {}
                ControlEvent::Title(title) => {
                    if let Some(state) = self.sessions.get_mut(&pane)
                        && state.generation == generation
                    {
                        // The title belongs to whatever is running as it arrives.
                        state.sample_foreground(std::time::Instant::now());
                        state.activity.set_title(title);
                    }
                }
                ControlEvent::Bell => {}
                ControlEvent::ChildExited(_) => {}
                ControlEvent::Close => should_close = true,
            }
        }
        WakeOutcome {
            accepted: true,
            clipboard,
            should_close,
        }
    }

    fn sample_foreground(&mut self, now: std::time::Instant) {
        self.sessions
            .values_mut()
            .for_each(|state| state.sample_foreground(now));
    }

    /// What `tab` is called right now.
    pub(super) fn tab_label(&self, tab: &Tab) -> String {
        let state = self.sessions.get(&tab.focused);
        resolve_tab_label(TabLabelSources {
            custom_name: tab.custom_name.as_deref(),
            running_task: state.and_then(|state| state.activity.task_name()),
            foreground_program: state.and_then(|state| state.activity.program()),
            title: state.and_then(|state| state.activity.title()),
            directory: state.and_then(|state| state.cuenv.cwd.as_deref()),
        })
    }

    /// What the active tab is called right now, if there is one.
    pub(super) fn active_tab_label(&self) -> Option<String> {
        let active = self.workspace.active_tab()?;
        let tabs = self.workspace.workspace().tabs();
        let tab = tabs.iter().find(|tab| tab.id == active)?;
        Some(self.tab_label(tab))
    }

    fn close_pane(&mut self, pane: crate::workspace::PaneId) -> Result<(), RegistryError> {
        let target = self
            .workspace
            .preflight_close_pane(pane)
            .map_err(RegistryError::Workspace)?;
        let Some(mut state) = self.sessions.remove(&pane) else {
            return Err(RegistryError::MissingSession(pane));
        };
        let close_error = state.session.close().err();
        self.workspace.commit_close_preflighted(target);
        self.close_window_requested = target.is_last_tab;
        if let Some(error) = &close_error {
            self.notice = Some(format!("terminal close reported: {error}"));
        }
        Ok(())
    }
}

pub(super) struct TerminalView {
    pub(super) registry: SessionRegistry,
    cuenv: Arc<dyn CuenvProvider>,
    config: TerminalConfig,
    pub(super) metrics: TerminalMetrics,
    theme: TerminalTheme,
    focus: FocusHandle,
    renderer: Box<dyn TerminalRenderer>,
    workspace_bounds: Arc<Mutex<(f32, f32)>>,
    window_background: WindowBackgroundAppearance,
    pub(super) window_decorations: Decorations,
    pub(super) task_palette_open: bool,
    pub(super) task_sidebar_open: bool,
    pub(super) task_query: String,
    pub(super) task_selection: usize,
    pub(super) task_palette_scroll: ScrollHandle,
    pub(super) rail_scroll: ScrollHandle,
    pub(super) rail_revealed_tab: Option<TabId>,
    /// Whether the rail is showing each tab's number key. Stays off until the
    /// modifier has been held a moment, so typing Ctrl-C never flashes it.
    pub(super) tab_hints_visible: bool,
    /// Identifies the latest modifier change, so a delayed reveal can tell it has
    /// been overtaken.
    tab_hint_request: u64,
    sidebar: WindowSidebar,
    /// What the window manager was last told this window is called.
    window_title: String,
}

/// What the window is called with no tab to name it after.
const APP_TITLE: &str = "Cuetty";

/// The key that toggles the sidebar with the application chord.
pub(super) const SIDEBAR_TOGGLE_KEY: &str = "b";

/// How long a window's first frame may wait on the global configuration. Going over
/// only means the configuration arrives after the frame, as a directory's does.
const FIRST_FRAME_CONFIG_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// How long the tab number modifier must be held before the hints appear.
const TAB_HINT_DELAY: std::time::Duration = std::time::Duration::from_millis(350);

/// How many filtered tasks the palette shows and the keyboard can reach.
pub(super) const PALETTE_RESULT_LIMIT: usize = 9;

struct CuenvIntegrationUpdate {
    presentation: Evaluation,
    tasks: TaskEvaluation,
}

pub(super) fn filtered_tasks<'a>(tasks: &'a [CuenvTask], query: &str) -> Vec<&'a CuenvTask> {
    let query = query.trim().to_ascii_lowercase();
    tasks
        .iter()
        .filter(|task| {
            query.is_empty()
                || task.name.to_ascii_lowercase().contains(&query)
                || task
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_ascii_lowercase().contains(&query))
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The command line for a task, ready for the arguments it may need to follow.
fn task_command(cwd: &std::path::Path, task: &CuenvTask) -> String {
    format!(
        "cuenv task --path {} --package cuenv {}{}",
        shell_quote(&cwd.to_string_lossy()),
        shell_quote(&task.name),
        if task.requires_parameters { " " } else { "" }
    )
}

fn wheel_scroll_lines(delta: ScrollDelta, line_height: f32, remainder: &mut f32) -> i32 {
    let vertical_lines = match delta {
        ScrollDelta::Pixels(delta) => f32::from(delta.y) / line_height.max(1.0),
        ScrollDelta::Lines(delta) => delta.y,
    };
    // AppKit has already adjusted NSEvent's scrolling delta for the user's
    // Natural Scrolling preference, and GPUI forwards that value unchanged.
    // Preserve its sign when translating pixels to Rio history lines; another
    // inversion here would make Cuetty behave opposite to native macOS views.
    let total = *remainder + vertical_lines;
    let lines = total.trunc().clamp(i32::MIN as f32, i32::MAX as f32) as i32;
    *remainder = total - lines as f32;
    lines
}

/// Modifier chord that addresses Cuetty itself instead of the shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppChord {
    Primary,
    Alternate,
}

/// Resolves Cuetty's application chord without stealing shell control keys.
///
/// GPUI's `secondary()` is Control off macOS, which would swallow Ctrl-C,
/// Ctrl-D, and friends. macOS uses Command (Shift for the alternate form);
/// other platforms use Ctrl-Shift (Alt for the alternate form), matching
/// conventional Linux terminals.
fn app_chord(modifiers: gpui::Modifiers) -> Option<AppChord> {
    let (base, alternate) = if cfg!(target_os = "macos") {
        (modifiers.platform && !modifiers.control, modifiers.shift)
    } else {
        (
            modifiers.control && modifiers.shift && !modifiers.platform,
            modifiers.alt,
        )
    };
    base.then_some(if alternate {
        AppChord::Alternate
    } else {
        AppChord::Primary
    })
}

/// The modifiers tabs are selected by number with: Command on macOS and Control
/// elsewhere. Ctrl-2 to Ctrl-8 are also legacy aliases for control bytes, so a chord
/// is only taken when it reaches a tab; see [`route_chord`].
fn tab_number_modifiers() -> ChordModifiers {
    let macos = cfg!(target_os = "macos");
    ChordModifiers {
        control: !macos,
        platform: macos,
        ..ChordModifiers::default()
    }
}

/// Whether exactly the tab number modifiers are held.
fn tab_number_modifiers_held(modifiers: gpui::Modifiers) -> bool {
    let expected = tab_number_modifiers();
    modifiers.control == expected.control
        && modifiers.platform == expected.platform
        && !modifiers.alt
        && !modifiers.shift
}

fn tab_number_shortcut(key: &str, modifiers: gpui::Modifiers) -> Option<TabNumber> {
    tab_number_modifiers_held(modifiers)
        .then(|| TabNumber::from_key(key))
        .flatten()
}

/// The modifiers of Cuetty's primary application chord, as [`app_chord`] reads it.
fn app_chord_modifiers() -> ChordModifiers {
    let macos = cfg!(target_os = "macos");
    ChordModifiers {
        control: !macos,
        shift: !macos,
        platform: macos,
        ..ChordModifiers::default()
    }
}

/// How the application chord on `key` is written on this platform.
pub(super) fn app_chord_label(key: &str) -> String {
    KeyChord {
        key: key.into(),
        modifiers: app_chord_modifiers(),
    }
    .label()
}

/// The chord shown for a tab's number key, such as `⌘1` or `Ctrl+1`.
pub(super) fn tab_number_hint(number: TabNumber) -> String {
    KeyChord {
        key: number.digit().to_string(),
        modifiers: tab_number_modifiers(),
    }
    .label()
}

/// Whether a key press is exactly the configured chord.
fn chord_matches(chord: &KeyChord, key: &str, modifiers: gpui::Modifiers) -> bool {
    key.eq_ignore_ascii_case(&chord.key)
        && modifiers.control == chord.modifiers.control
        && modifiers.alt == chord.modifiers.alt
        && modifiers.shift == chord.modifiers.shift
        && modifiers.platform == chord.modifiers.platform
}

/// Where a key press goes before the terminal gets a look at it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChordRoute {
    OpenPalette,
    SelectTab(TabId),
    /// Not claimed here, which leaves it to the remaining shortcuts and the terminal.
    Unclaimed,
}

/// What a key press could select.
struct ChordTargets<'a> {
    /// The palette's chord, when there is a task catalogue for it to open.
    palette: Option<&'a KeyChord>,
    tabs: &'a WorkspaceShell,
}

/// Routes a key press. The palette's chord comes first because the user chose it. A
/// tab number that reaches no tab is unclaimed, since off macOS most of those chords
/// are also control bytes an application may be waiting for.
fn route_chord(key: &str, modifiers: gpui::Modifiers, targets: &ChordTargets<'_>) -> ChordRoute {
    if targets
        .palette
        .is_some_and(|palette| chord_matches(palette, key, modifiers))
    {
        return ChordRoute::OpenPalette;
    }
    tab_number_shortcut(key, modifiers)
        .and_then(|number| targets.tabs.tab_at_number(number))
        .map_or(ChordRoute::Unclaimed, ChordRoute::SelectTab)
}

fn terminal_key_input(key: &str, key_char: Option<&str>) -> Option<KeyInput> {
    match key {
        "enter" => Some(KeyInput::Enter),
        "tab" => Some(KeyInput::Tab),
        "backspace" => Some(KeyInput::Backspace),
        "escape" => Some(KeyInput::Escape),
        "up" => Some(KeyInput::Up),
        "down" => Some(KeyInput::Down),
        "left" => Some(KeyInput::Left),
        "right" => Some(KeyInput::Right),
        "home" => Some(KeyInput::Home),
        "end" => Some(KeyInput::End),
        "delete" => Some(KeyInput::Delete),
        "space" => Some(KeyInput::Character(' ')),
        _ => key_char
            .and_then(single_character)
            // GPUI intentionally omits key_char for macOS Control chords.
            // Its layout-aware key remains available and is the correct
            // source for the control-byte mapping.
            .or_else(|| single_character(key))
            .map(KeyInput::Character),
    }
}

fn single_character(text: &str) -> Option<char> {
    let mut characters = text.chars();
    let character = characters.next()?;
    characters.next().is_none().then_some(character)
}

/// Records a measurement, returning whether it differs from the last one. A panic
/// elsewhere cannot leave one half-written, so a poisoned lock is still good.
fn store_if_changed<T: PartialEq>(slot: &Mutex<T>, next: T) -> bool {
    let mut current = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let changed = *current != next;
    *current = next;
    changed
}

/// Asks for another frame from inside prepaint, where a notify marks nothing dirty and
/// is lost: GPUI ignores invalidation while it draws. Deferred effects run once the
/// draw has finished.
fn redraw_after_this_frame(view: gpui::WeakEntity<TerminalView>, cx: &mut gpui::App) {
    cx.defer(move |cx| {
        let _ = view.update(cx, |_view, cx| cx.notify());
    });
}

impl TerminalView {
    #[cfg(test)]
    fn empty_frame() -> TerminalFrame {
        TerminalFrame {
            dimensions: TerminalDimensions::new(1, 1),
            rows: vec![TerminalRow {
                soft_wrapped: false,
                dirty: false,
                cells: vec![TerminalCell::narrow(' ')],
            }],
            cursor: CursorState {
                position: None,
                shape: Known::Unknown,
                visible: Known::Unknown,
            },
            viewport_offset: None,
            sampling_token: SamplingToken(0),
            stable_revision: None,
        }
    }
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_provider(window, cx, Arc::new(NativeCuenvProvider))
    }

    /// Construct the host with an explicit Cuenv provider. The production
    /// entry point uses the native provider, while tests and alternate hosts
    /// can inject a deterministic evaluator/watcher without touching GPUI or
    /// the Rio session implementation.
    fn new_with_provider(
        window: &mut Window,
        cx: &mut Context<Self>,
        cuenv: Arc<dyn CuenvProvider>,
    ) -> Self {
        cx.observe_window_bounds(window, move |_, _, cx| cx.notify())
            .detach();
        // A modifier released while another window has focus is never reported.
        cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active() {
                view.hide_tab_hints(cx);
            }
        })
        .detach();
        let config = TerminalConfig::default();
        let metrics = TerminalMetrics::from_config(cx.text_system(), &config);
        let theme = TerminalTheme::from_config(&config);
        let window_background = theme.window_background();
        let factory: Arc<dyn TerminalSessionFactory> = Arc::new(RioTerminalFactory);
        match SessionRegistry::start(factory.clone(), 720, 432, metrics.rio_cell()) {
            Ok((mut registry, wake_rx)) => {
                let pane = registry.active_pane().expect("initial workspace pane");
                // The window's first frame sizes the grid, so what shapes it has to be
                // known by then. A pane carries this look over until its directory
                // has evaluated.
                let sidebar = ConfigSource::from_working_directory(None)
                    .ok()
                    .flatten()
                    .and_then(|source| {
                        evaluate_within(Arc::clone(&cuenv), source, FIRST_FRAME_CONFIG_WAIT)
                    })
                    .map_or_else(WindowSidebar::default, |global| {
                        let sidebar = WindowSidebar::expecting(&global);
                        if let Some(state) = registry.sessions.get_mut(&pane) {
                            let generation = state.cuenv.begin_source();
                            state.cuenv.apply(generation, global);
                        }
                        sidebar
                    });
                let mut view = Self {
                    registry,
                    cuenv,
                    config,
                    metrics,
                    theme,
                    focus: cx.focus_handle(),
                    renderer: Box::new(GridRenderer),
                    workspace_bounds: Arc::new(Mutex::new((720.0, 432.0))),
                    window_background,
                    window_decorations: Decorations::default(),
                    task_palette_open: false,
                    task_sidebar_open: false,
                    task_query: String::new(),
                    task_selection: 0,
                    task_palette_scroll: ScrollHandle::new(),
                    rail_scroll: ScrollHandle::new(),
                    rail_revealed_tab: None,
                    tab_hints_visible: false,
                    tab_hint_request: 0,
                    sidebar,
                    window_title: APP_TITLE.into(),
                };
                view.spawn_wake(cx, pane, SessionGeneration(1), wake_rx);
                view.spawn_cwd_poll(cx);
                view.refresh_cuenv_integration(cx);
                view
            }
            Err(error) => {
                let mut registry = SessionRegistry::empty(factory);
                registry.notice = Some(format!("Cuetty could not start Rio: {error}"));
                Self {
                    registry,
                    cuenv,
                    config,
                    metrics,
                    theme,
                    focus: cx.focus_handle(),
                    renderer: Box::new(GridRenderer),
                    workspace_bounds: Arc::new(Mutex::new((720.0, 432.0))),
                    window_background,
                    window_decorations: Decorations::default(),
                    task_palette_open: false,
                    task_sidebar_open: false,
                    task_query: String::new(),
                    task_selection: 0,
                    task_palette_scroll: ScrollHandle::new(),
                    rail_scroll: ScrollHandle::new(),
                    rail_revealed_tab: None,
                    tab_hints_visible: false,
                    tab_hint_request: 0,
                    sidebar: WindowSidebar::default(),
                    window_title: APP_TITLE.into(),
                }
            }
        }
    }
    fn refresh_cuenv_integration(&mut self, cx: &mut Context<Self>) {
        let panes: Vec<_> = self.registry.sessions.keys().copied().collect();
        for pane in panes {
            self.refresh_cuenv_pane(pane, cx);
        }
    }

    fn refresh_cuenv_pane(&mut self, pane: crate::workspace::PaneId, cx: &mut Context<Self>) {
        let (session_generation, cwd) = self
            .registry
            .sessions
            .get(&pane)
            .map(|state| (state.generation, state.session.working_directory()))
            .expect("active pane registry invariant");
        let canonical_cwd = cwd.map(|cwd| {
            let cwd = std::path::PathBuf::from(cwd);
            cwd.canonicalize().unwrap_or(cwd)
        });
        let cwd_changed = {
            let state = self
                .registry
                .sessions
                .get_mut(&pane)
                .expect("active pane registry invariant");
            let changed = match &canonical_cwd {
                Some(cwd) => state.cuenv.observe_cwd(cwd.clone()),
                None => state.cuenv.forget_cwd(),
            };
            if changed {
                state.cuenv_source = None;
                state.cuenv_watcher = None;
                state.cuenv_watch_retry_at = None;
            }
            changed
        };
        if cwd_changed {
            self.sync_active_config(cx);
            if self.active_pane() == Some(pane) {
                self.reconcile_task_ui();
            }
        }
        let source = ConfigSource::from_working_directory(canonical_cwd.as_deref());
        let source = match source {
            Ok(Some(source)) => source,
            Ok(None) => {
                let state = self
                    .registry
                    .sessions
                    .get_mut(&pane)
                    .expect("active pane registry invariant");
                // A changed directory carried the old look over for an evaluation
                // that is not coming, so this has to settle it too.
                if state.cuenv_source.take().is_some() || cwd_changed {
                    let generation = state.cuenv.begin_source();
                    state.cuenv.apply(generation, Evaluation::Absent);
                    state.cuenv_watcher = None;
                    state.cuenv_watch_retry_at = None;
                    self.sync_active_config(cx);
                }
                return;
            }
            Err(error) => {
                if let Some(state) = self.registry.sessions.get_mut(&pane) {
                    state.cuenv.notice = Some(error);
                }
                return;
            }
        };
        let discover_tasks = canonical_cwd.is_some();
        let now = std::time::Instant::now();
        let Some(state) = self.registry.sessions.get_mut(&pane) else {
            return;
        };
        let watcher_error = state
            .cuenv_watcher
            .as_ref()
            .and_then(|watcher| watcher.take_error());
        if let Some(error) = watcher_error {
            state.cuenv_watcher = None;
            state.cuenv_watch_retry_at = Some(now + std::time::Duration::from_secs(1));
            state.cuenv.notice = Some(format!("Cuetty CUE watcher failed: {error}"));
        }
        let same_source = state.cuenv_source.as_ref() == Some(&source);
        let retry_watcher = same_source
            && state.cuenv_watcher.is_none()
            && state
                .cuenv_watch_retry_at
                .is_none_or(|retry_at| now >= retry_at);
        if same_source && !retry_watcher {
            return;
        }
        let config_generation = state.cuenv.begin_source();
        state.cuenv_source = Some(source.clone());
        state.cuenv_watcher = None;
        let (wake_tx, wake_rx) = async_channel::bounded(1);
        let cuenv = Arc::clone(&self.cuenv);
        match cuenv.watch(&source, wake_tx) {
            Ok(watcher) => {
                state.cuenv_watcher = Some(watcher);
                state.cuenv_watch_retry_at = None;
                if retry_watcher {
                    state.cuenv.notice = None;
                }
            }
            Err(error) => {
                state.cuenv_watch_retry_at = Some(now + std::time::Duration::from_secs(1));
                state.cuenv.notice = Some(error);
            }
        }
        let (updates_tx, updates_rx) = async_channel::unbounded();
        cx.spawn(async move |weak, cx| {
            while let Ok(update) = updates_rx.recv().await {
                if weak
                    .update(cx, |view, cx| {
                        view.apply_cuenv_update(
                            CuenvUpdateTarget {
                                pane,
                                session_generation,
                                config_generation,
                            },
                            update,
                            cx,
                        )
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let evaluator = Arc::clone(&cuenv);
        cx.spawn(async move |_weak, _cx| {
            let (result_tx, result_rx) = async_channel::bounded(1);
            let initial_source = source.clone();
            let initial_evaluator = Arc::clone(&evaluator);
            std::thread::spawn(move || {
                let _ = result_tx.send_blocking(CuenvIntegrationUpdate {
                    presentation: initial_evaluator.evaluate(&initial_source),
                    tasks: if discover_tasks {
                        initial_evaluator.evaluate_tasks(&initial_source)
                    } else {
                        TaskEvaluation::Absent
                    },
                });
            });
            let Ok(update) = result_rx.recv().await else {
                return;
            };
            if updates_tx.send(update).await.is_err() {
                return;
            }
            while wake_rx.recv().await.is_ok() {
                // Coalesce rapid atomic-save events before starting one
                // blocking evaluation. The bounded watcher channel plus this
                // serialized worker prevent an editor save storm from
                // creating an unbounded thread queue.
                while wake_rx.try_recv().is_ok() {}
                Timer::after(std::time::Duration::from_millis(120)).await;
                while wake_rx.try_recv().is_ok() {}
                let (result_tx, result_rx) = async_channel::bounded(1);
                let source = source.clone();
                let evaluator = Arc::clone(&evaluator);
                std::thread::spawn(move || {
                    let _ = result_tx.send_blocking(CuenvIntegrationUpdate {
                        presentation: evaluator.evaluate(&source),
                        tasks: if discover_tasks {
                            evaluator.evaluate_tasks(&source)
                        } else {
                            TaskEvaluation::Absent
                        },
                    });
                });
                let Ok(update) = result_rx.recv().await else {
                    break;
                };
                if updates_tx.send(update).await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply_cuenv_update(
        &mut self,
        target: CuenvUpdateTarget,
        update: CuenvIntegrationUpdate,
        cx: &mut Context<Self>,
    ) {
        let is_active = self.active_pane() == Some(target.pane);
        let Some(state) = self.registry.sessions.get_mut(&target.pane) else {
            return;
        };
        if state.generation != target.session_generation {
            return;
        }
        if is_active && state.cuenv.generation == target.config_generation {
            self.sidebar.observe(&update.presentation);
        }
        if !state
            .cuenv
            .apply(target.config_generation, update.presentation)
        {
            return;
        }
        state
            .cuenv
            .apply_tasks(target.config_generation, update.tasks);
        if is_active {
            self.reconcile_task_ui();
        }
        self.sync_active_config(cx);
        cx.notify();
    }

    fn sync_active_config(&mut self, cx: &mut Context<Self>) {
        let config = self.registry.active_terminal_config();
        if self.config != config {
            self.metrics = TerminalMetrics::from_config(cx.text_system(), &config);
            self.theme = TerminalTheme::from_config(&config);
            self.config = config;
        }
    }

    fn reconcile_task_ui(&mut self) {
        let count = self
            .active_state()
            .map_or(0, |state| state.cuenv.tasks.len());
        if count == 0 {
            self.task_palette_open = false;
            self.task_sidebar_open = false;
        }
        self.task_selection = self.task_selection.min(count.saturating_sub(1));
    }
    fn spawn_cwd_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |weak, cx| {
            loop {
                Timer::after(std::time::Duration::from_millis(500)).await;
                if weak
                    .update(cx, |view, cx| {
                        view.registry.sample_foreground(std::time::Instant::now());
                        view.refresh_cuenv_integration(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn active_pane(&self) -> Option<crate::workspace::PaneId> {
        self.registry.active_pane()
    }
    pub(super) fn active_state(&self) -> Option<&PaneState> {
        self.active_pane()
            .and_then(|pane| self.registry.sessions.get(&pane))
    }
    fn active_state_mut(&mut self) -> Option<&mut PaneState> {
        self.active_pane()
            .and_then(|pane| self.registry.sessions.get_mut(&pane))
    }
    fn active_tab(&self) -> Option<TabId> {
        self.registry.workspace.active_tab()
    }
    pub(super) fn select_terminal_tab(
        &mut self,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.registry.workspace.apply(ShellAction::ActivateTab(tab)) {
            Ok(_) => {
                self.registry.notice = None;
                self.focus.focus(window);
                self.refresh_cuenv_integration(cx);
                self.sync_active_config(cx);
                self.reconcile_task_ui();
            }
            Err(error) => self.registry.notice = Some(shell_notice(error)),
        }
        cx.notify();
    }
    fn spawn_wake(
        &mut self,
        cx: &mut Context<Self>,
        pane: crate::workspace::PaneId,
        generation: SessionGeneration,
        wake_rx: async_channel::Receiver<()>,
    ) {
        cx.spawn(async move |weak, cx| {
            while wake_rx.recv().await.is_ok() {
                if weak
                    .update(cx, |view, cx| {
                        view.handle_wake(pane, generation, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn handle_wake(
        &mut self,
        pane: crate::workspace::PaneId,
        generation: SessionGeneration,
        cx: &mut Context<Self>,
    ) {
        let outcome = self
            .registry
            .apply_wake(pane, generation, self.registry.active_pane());
        if !outcome.accepted {
            return;
        }
        self.refresh_cuenv_pane(pane, cx);
        for text in outcome.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if outcome.should_close {
            self.close_pane(pane, cx);
        }
        cx.notify();
    }
    fn close_pane(&mut self, pane: crate::workspace::PaneId, cx: &mut Context<Self>) {
        if let Err(error) = self.registry.close_pane(pane) {
            self.registry.notice = Some(format!("workspace action unavailable: {error}"));
        } else {
            self.sync_active_config(cx);
            self.reconcile_task_ui();
        }
    }
    pub(super) fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = 720;
        let height = 432;
        match self
            .registry
            .start_tab(width, height, self.metrics.rio_cell())
        {
            Ok((pane, generation, wake_rx)) => {
                self.spawn_wake(cx, pane, generation, wake_rx);
                self.refresh_cuenv_integration(cx);
                self.sync_active_config(cx);
                self.reconcile_task_ui();
                if self.active_tab().is_some() {
                    self.focus.focus(window);
                }
                cx.notify();
            }
            Err(RegistryError::Session(_)) => {
                self.registry.notice = Some("unable to start a new Rio session".into())
            }
            Err(error) => {
                self.registry.notice = Some(format!("workspace action unavailable: {error}"))
            }
        }
    }
    fn hit_test(
        &self,
        position: gpui::Point<Pixels>,
    ) -> (CellHitTest, super::selection::LogicalPosition) {
        let Some(state) = self.active_state() else {
            return (
                CellHitTest {
                    cell_width: 1.0,
                    cell_height: 1.0,
                    rows: 1,
                    columns: 1,
                },
                super::selection::LogicalPosition { line: 0, column: 0 },
            );
        };
        let (x, y, _, _) = *state
            .viewport_bounds
            .lock()
            .expect("viewport mutex poisoned");
        let cell = self.metrics.render_cell();
        let hit_test = CellHitTest {
            cell_width: f32::from(cell.width),
            cell_height: f32::from(cell.height),
            rows: state.frame.dimensions.rows as usize,
            columns: state.frame.dimensions.columns as usize,
        };
        (
            hit_test,
            hit_test.hit(f32::from(position.x) - x, f32::from(position.y) - y),
        )
    }

    fn search_key(&mut self, key: SearchKey) {
        if let Some(state) = self.active_state_mut()
            && let Some(matched) = state.interaction.search_key(key, &state.frame)
        {
            state.interaction.select_match(matched);
        }
    }

    fn scroll_active(&mut self, action: TerminalScroll) {
        if let Some(state) = self.active_state_mut() {
            match state.session.scroll(action) {
                Ok(()) => {
                    state.interaction.viewport_changed();
                    state.frame = state.session.frame();
                }
                Err(error) => state.error = Some(error),
            }
        }
    }

    fn scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let line_height = f32::from(self.metrics.render_cell().height);
        let (_, position) = self.hit_test(event.position);
        let mut bits = 0;
        if event.modifiers.control {
            bits |= InputModifiers::CONTROL.bits();
        }
        if event.modifiers.alt {
            bits |= InputModifiers::ALT.bits();
        }
        if event.modifiers.shift {
            bits |= InputModifiers::SHIFT.bits();
        }
        if event.modifiers.platform {
            bits |= InputModifiers::SUPER.bits();
        }
        let action = self.active_state_mut().and_then(|state| {
            // Retain sub-line trackpad motion per pane.
            let lines = wheel_scroll_lines(event.delta, line_height, &mut state.scroll_remainder);
            (lines != 0).then_some(TerminalScroll::Wheel(TerminalWheel {
                lines,
                column: u16::try_from(position.column).unwrap_or(u16::MAX),
                row: u16::try_from(position.line).unwrap_or(u16::MAX),
                modifiers: InputModifiers::from_bits(bits),
            }))
        });
        if let Some(action) = action {
            self.scroll_active(action);
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn workspace_shortcut(key: &str, modifiers: gpui::Modifiers) -> Option<WorkspaceShortcut> {
        let chord = app_chord(modifiers)?;
        match (chord, key.to_ascii_lowercase().as_str()) {
            (_, "t") => Some(WorkspaceShortcut::NewTab),
            (_, "w") => Some(WorkspaceShortcut::CloseTab),
            (AppChord::Primary, "d") => Some(WorkspaceShortcut::SplitHorizontal),
            (AppChord::Alternate, "d") => Some(WorkspaceShortcut::SplitVertical),
            (_, "]" | "}") => Some(WorkspaceShortcut::FocusNext),
            (_, "[" | "{") => Some(WorkspaceShortcut::FocusPrevious),
            _ => None,
        }
    }

    fn apply_workspace_action(&mut self, action: ShellAction) {
        match self.registry.workspace.apply(action) {
            Ok(_) => self.registry.notice = None,
            Err(error) => self.registry.notice = Some(shell_notice(error)),
        }
    }

    pub(super) fn sidebar_mode(&self) -> SidebarMode {
        self.sidebar.mode()
    }

    /// The terminal grid follows within a few frames: it is resized from the bounds
    /// measured in the layout this change causes.
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar.toggle();
        cx.notify();
    }

    /// Toggles the task palette, returning whether the active pane has a
    /// task catalogue to show.
    fn toggle_task_palette(&mut self) -> bool {
        if self
            .active_state()
            .is_none_or(|state| state.cuenv.tasks.is_empty())
        {
            return false;
        }
        self.task_palette_open = !self.task_palette_open;
        self.task_query.clear();
        self.task_selection = 0;
        self.task_palette_scroll.scroll_to_item(0);
        true
    }

    pub(super) fn launch_task(&mut self, task: CuenvTask, cx: &mut Context<Self>) {
        let Some(cwd) = self
            .active_state()
            .and_then(|state| state.cuenv.cwd.clone())
        else {
            return;
        };
        let Some(active_pane) = self.active_pane() else {
            return;
        };
        let command = task_command(&cwd, &task);
        if let Some(state) = self.registry.sessions.get_mut(&active_pane)
            && let Err(error) = state.launch_task(&command, &task)
        {
            state.error = Some(error);
        }
        self.close_task_palette(cx);
    }

    pub(super) fn close_task_palette(&mut self, cx: &mut Context<Self>) {
        self.task_palette_open = false;
        self.task_query.clear();
        self.task_selection = 0;
        cx.notify();
    }

    fn handle_task_palette_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.task_palette_open {
            return false;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => self.task_palette_open = false,
            "backspace" => {
                self.task_query.pop();
                self.task_selection = 0;
            }
            "up" => self.task_selection = self.task_selection.saturating_sub(1),
            "down" => {
                let count = self
                    .active_state()
                    .map(|state| filtered_tasks(&state.cuenv.tasks, &self.task_query).len())
                    .unwrap_or_default()
                    .min(PALETTE_RESULT_LIMIT);
                self.task_selection = (self.task_selection + 1).min(count.saturating_sub(1));
            }
            "enter" => {
                let selected = self.active_state().and_then(|state| {
                    filtered_tasks(&state.cuenv.tasks, &self.task_query)
                        .get(self.task_selection)
                        .cloned()
                        .cloned()
                });
                if let Some(task) = selected {
                    self.launch_task(task, cx);
                    return true;
                }
            }
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.alt
                && !event.keystroke.modifiers.platform =>
            {
                if let Some(text) = event.keystroke.key_char.as_deref() {
                    self.task_query.push_str(text);
                    self.task_selection = 0;
                }
            }
            _ => {}
        }
        // A short window clips the results, so the selection has to stay visible.
        self.task_palette_scroll.scroll_to_item(self.task_selection);
        cx.notify();
        true
    }

    fn tab_shortcut(key: &str, modifiers: gpui::Modifiers) -> Option<TabDirection> {
        if !modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform {
            return None;
        }
        // The function modifier may be needed to type Page Up/Down on macOS.
        match key.to_ascii_lowercase().as_str() {
            "pageup" => Some(TabDirection::Previous),
            "pagedown" => Some(TabDirection::Next),
            _ => None,
        }
    }

    /// Reveals the tab number hints once their modifier has been held a moment.
    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !tab_number_modifiers_held(event.modifiers) {
            self.hide_tab_hints(cx);
            return;
        }
        self.tab_hint_request = self.tab_hint_request.wrapping_add(1);
        let request = self.tab_hint_request;
        cx.spawn(async move |weak, cx| {
            Timer::after(TAB_HINT_DELAY).await;
            let _ = weak.update(cx, |view, cx| {
                if view.tab_hint_request == request {
                    view.tab_hints_visible = true;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn hide_tab_hints(&mut self, cx: &mut Context<Self>) {
        self.tab_hint_request = self.tab_hint_request.wrapping_add(1);
        if self.tab_hints_visible {
            self.tab_hints_visible = false;
            cx.notify();
        }
    }

    fn key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let palette = self
            .active_state()
            .filter(|state| !state.cuenv.tasks.is_empty())
            .map(|state| &state.cuenv.presentation.keybindings.palette);
        let route = route_chord(
            key,
            modifiers,
            &ChordTargets {
                palette,
                tabs: &self.registry.workspace,
            },
        );
        // The hints stay up, so another number can follow without letting go.
        if let ChordRoute::SelectTab(tab) = route {
            self.select_terminal_tab(tab, window, cx);
            return;
        }
        // The modifier was held for some other chord, such as Ctrl-C.
        self.hide_tab_hints(cx);
        // Without a task catalogue the chord is unclaimed, so a plain Ctrl-K palette
        // binding still reaches the shell as kill-line.
        if route == ChordRoute::OpenPalette && self.toggle_task_palette() {
            cx.notify();
            return;
        }
        if let Some(direction) = Self::tab_shortcut(key, modifiers) {
            if let Some(tab) = self.registry.workspace.tab_in_direction(direction) {
                self.select_terminal_tab(tab, window, cx);
            }
            return;
        }
        if self.handle_task_palette_key(event, cx) {
            return;
        }
        if let Some(shortcut) = Self::workspace_shortcut(key, modifiers) {
            match shortcut {
                WorkspaceShortcut::NewTab => self.new_tab(window, cx),
                WorkspaceShortcut::CloseTab => {
                    if let Some(pane) = self.active_pane() {
                        self.close_pane(pane, cx);
                    }
                }
                _ => self.apply_workspace_action(ShellAction::Shortcut(shortcut)),
            }
            cx.notify();
            return;
        }
        if app_chord(modifiers).is_some() && key.eq_ignore_ascii_case(SIDEBAR_TOGGLE_KEY) {
            self.toggle_sidebar(cx);
            return;
        }
        // Every pane in the workspace registry owns a live Rio session. Keep
        // input, search, copy, and paste routed through that registry entry.
        if self.active_state().is_none() {
            return;
        }
        if modifiers.shift {
            let scroll = match key {
                "pageup" => Some(TerminalScroll::PageUp),
                "pagedown" => Some(TerminalScroll::PageDown),
                "home" => Some(TerminalScroll::Top),
                "end" => Some(TerminalScroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.scroll_active(scroll);
                cx.notify();
                return;
            }
        }
        if app_chord(modifiers).is_some() && key.eq_ignore_ascii_case("f") {
            if let Some(state) = self.active_state_mut() {
                state.interaction.enter_search(&state.frame);
            }
            cx.notify();
            return;
        }
        if self
            .active_state()
            .is_some_and(|state| state.interaction.search().is_some())
        {
            let search_key = match key {
                "backspace" => Some(SearchKey::Backspace),
                "enter" => Some(SearchKey::Enter {
                    reverse: modifiers.shift,
                }),
                "escape" => Some(SearchKey::Escape),
                _ => event
                    .keystroke
                    .key_char
                    .as_ref()
                    .and_then(|text| text.chars().next())
                    .map(SearchKey::Character),
            };
            if let Some(search_key) = search_key {
                self.search_key(search_key);
                cx.notify();
            }
            return;
        }
        if app_chord(modifiers).is_some() && key.eq_ignore_ascii_case("c") {
            if let Some(text) = self
                .active_state()
                .and_then(|state| state.interaction.copy(&state.frame))
            {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
        if app_chord(modifiers).is_some() && key.eq_ignore_ascii_case("v") {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
                && let Some(state) = self.active_state_mut()
                && let Err(error) = state.session.paste(&text)
            {
                state.error = Some(error);
            }
            return;
        }
        let Some(input) = terminal_key_input(key, event.keystroke.key_char.as_deref()) else {
            return;
        };
        let mut bits = 0;
        if modifiers.control {
            bits |= InputModifiers::CONTROL.bits();
        }
        if modifiers.alt {
            bits |= InputModifiers::ALT.bits();
        }
        if modifiers.shift {
            bits |= InputModifiers::SHIFT.bits();
        }
        if modifiers.platform {
            bits |= InputModifiers::SUPER.bits();
        }
        if let Some(state) = self.active_state_mut()
            && let Err(error) = state.input(TerminalKeyEvent {
                key: input,
                modifiers: InputModifiers::from_bits(bits),
                repeat: event.is_held,
            })
        {
            state.error = Some(error);
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.active_pane() {
            self.apply_workspace_action(ShellAction::FocusPane(pane));
        }
        self.focus.focus(window);
        let (hit_test, position) = self.hit_test(event.position);
        if let Some(state) = self.active_state_mut() {
            state.interaction.begin_selection(hit_test, position);
        }
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.dragging() {
            let (hit_test, position) = self.hit_test(event.position);
            if let Some(state) = self.active_state_mut() {
                state.interaction.extend_selection(hit_test, position);
            }
            cx.notify();
        } else if let Some(state) = self.active_state_mut() {
            // Moving with no button held means the press ended where no release
            // could reach this view, such as outside the window.
            state.interaction.end_selection();
        }
    }

    fn mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.active_state_mut() {
            state.interaction.end_selection();
        }
        cx.notify();
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        if self.registry.close_window_requested {
            window.remove_window();
        }
        // The active pane controls both painting and PTY geometry. Background
        // configuration updates must never alter the visible pane's metrics.
        self.sync_active_config(cx);
        let window_background = self.theme.window_background();
        if self.window_background != window_background {
            window.set_background_appearance(window_background);
            self.window_background = window_background;
        }
        self.window_decorations = window.window_decorations();
        // Window switchers and compositors have only this to go by, and with the
        // title strip hidden and the sidebar collapsed nothing else names the tab.
        let window_title = self
            .registry
            .active_tab_label()
            .unwrap_or_else(|| APP_TITLE.into());
        if self.window_title != window_title {
            window.set_window_title(&window_title);
            self.window_title = window_title;
        }
        let theme = self.theme.clone();
        let metrics = self.metrics.clone();
        let padding = self.config.terminal_padding;
        let active_tab = self.active_tab();
        self.reveal_active_rail_tab(active_tab);
        let sidebar = self.render_sidebar(active_tab, &theme, cx);
        let Some(active_pane) = self.active_pane() else {
            let message = self
                .registry
                .notice
                .clone()
                .unwrap_or_else(|| "Cuetty has no live terminal session".into());
            return self.render_shell_layout(
                sidebar,
                render_shell_message(message, &theme),
                &theme,
            );
        };
        let state = self
            .registry
            .sessions
            .get(&active_pane)
            .expect("active pane registry invariant");
        let interaction = state.interaction.clone();
        let pane_error = state.error.clone();
        let measured = Arc::clone(&state.measured_bounds);
        let viewport_bounds = Arc::clone(&state.viewport_bounds);
        let entity = cx.weak_entity();
        let terminal_focused = terminal_has_focus(self.focus.is_focused(window), true);
        if let Some(error) = &pane_error {
            return self.render_shell_layout(
                sidebar,
                render_shell_message(format!("Cuetty terminal error: {error}"), &theme),
                &theme,
            );
        }
        let workspace_bounds = Arc::clone(&self.workspace_bounds);
        let workspace_entity = cx.weak_entity();
        let (workspace_width, workspace_height) = *self
            .workspace_bounds
            .lock()
            .expect("workspace bounds mutex poisoned");
        let layouts = self
            .registry
            .workspace
            .project(
                LayoutRect::new(
                    0.0,
                    0.0,
                    workspace_width.max(1.0),
                    workspace_height.max(1.0),
                ),
                MinSizePolicy::new(1.0, 1.0),
            )
            .unwrap_or_default();
        let frame = self.registry.sessions.get_mut(&active_pane).map(|state| {
            // The canvas is inside the actual content panel, so this includes
            // the title strip, terminal inset, and the sidebar at its current
            // width without trying to reconstruct GPUI's layout arithmetic.
            let bounds = *state.measured_bounds.lock().expect("bounds mutex poisoned");
            if let Err(error) = state
                .session
                .resize(bounds.0, bounds.1, self.metrics.rio_cell())
            {
                state.error = Some(error);
            }
            // Sampled after the resize, so the draw that resizes the grid also shows it.
            state.frame = state.session.frame();
            state.frame.clone()
        });
        let Some(frame) = frame else {
            return self.render_shell_layout(
                sidebar,
                render_shell_message("Cuetty lost this tab's terminal session", &theme),
                &theme,
            );
        };
        let overlays = TerminalOverlays {
            selection: interaction.selection(),
            current_search_match: interaction.current_match(),
        };
        let mut content = Some(self.renderer.render(RenderRequest {
            frame: &frame,
            metrics: &metrics,
            theme: &theme,
            focused: terminal_focused,
            overlays,
        }));
        let mut terminal_surface = div().relative().overflow_hidden().child(
            canvas(
                move |bounds, _, app| {
                    let next = (
                        f32::from(bounds.size.width).max(1.0),
                        f32::from(bounds.size.height).max(1.0),
                    );
                    if store_if_changed(&workspace_bounds, next) {
                        redraw_after_this_frame(workspace_entity, app);
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        );
        for pane in layouts {
            let bounds = pane.bounds;
            let pane_id = pane.pane_id;
            let pane_shell = div()
                .id(("cuetty-pane", pane_id.get()))
                .absolute()
                .left(px(bounds.x))
                .top(px(bounds.y))
                .w(px(bounds.width.max(1.0)))
                .h(px(bounds.height.max(1.0)))
                .overflow_hidden();
            if pane_id == active_pane {
                let pane_measured = Arc::clone(&measured);
                let pane_viewport_bounds = Arc::clone(&viewport_bounds);
                let pane_entity = entity.clone();
                let live_content = content
                    .take()
                    .expect("the active tab contains its live Rio pane");
                // The projected pane is the outer absolute element.  Keep its
                // geometry intact and put the interactive terminal in a
                // full-size relative child; calling `relative()` on the outer
                // element would override its projected positioning.
                let viewport = pane_shell.child(
                    div()
                        .relative()
                        .size_full()
                        .track_focus(&self.focus)
                        .on_key_down(cx.listener(Self::key))
                        .on_modifiers_changed(cx.listener(Self::modifiers_changed))
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                        .on_mouse_move(cx.listener(Self::mouse_move))
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                        // A drag can end over chrome, whose panels occlude this
                        // element. The gesture still has to end there, or a later
                        // drag starting on that chrome extends the old selection.
                        .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                        .on_scroll_wheel(cx.listener(Self::scroll_wheel))
                        .child(
                            canvas(
                                move |bounds, _, app| {
                                    let next = (
                                        f32::from(bounds.size.width).max(1.0) as u32,
                                        f32::from(bounds.size.height).max(1.0) as u32,
                                    );
                                    if store_if_changed(&pane_measured, next) {
                                        redraw_after_this_frame(pane_entity, app);
                                    }
                                    *pane_viewport_bounds
                                        .lock()
                                        .expect("viewport mutex poisoned") = (
                                        f32::from(bounds.origin.x),
                                        f32::from(bounds.origin.y),
                                        f32::from(bounds.size.width),
                                        f32::from(bounds.size.height),
                                    );
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .inset_0(),
                        )
                        .child(live_content),
                );
                terminal_surface = terminal_surface.child(viewport);
            }
        }
        if let Some(task_sidebar) = self.render_task_sidebar(&theme, cx) {
            terminal_surface = terminal_surface.child(task_sidebar);
        }
        if let Some(task_palette) = self.render_task_palette(&theme, cx) {
            terminal_surface = terminal_surface.child(task_palette);
        }
        let top_padding = px(f32::from(padding.top));
        let surface = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                terminal_surface
                    .absolute()
                    .top(self.top_inset(top_padding))
                    .right(px(f32::from(padding.right)))
                    .bottom(px(f32::from(padding.bottom)))
                    .left(px(f32::from(padding.left))),
            )
            .children(self.window_controls_band(top_padding));
        self.render_shell_layout(sidebar, surface.into_any_element(), &theme)
    }
}

pub fn run() {
    Application::new()
        .with_assets(ChromeAssets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            resolve_ui_font(cx.text_system());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus(vec![Menu {
                name: "Cuetty".into(),
                items: vec![
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit Cuetty", Quit),
                ],
            }]);
            let window_size = size(px(960.), px(640.));
            let window_min_size = size(px(640.), px(400.));
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                            None,
                            window_size,
                            cx,
                        ))),
                        window_min_size: Some(window_min_size),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some(APP_TITLE.into()),
                            appears_transparent: true,
                            traffic_light_position: Some(traffic_light_position()),
                        }),
                        window_background: TerminalTheme::default().window_background(),
                        // Matches the installed `cuetty.desktop` so Linux
                        // compositors associate windows with the launcher entry.
                        app_id: Some("cuetty".to_string()),
                        ..WindowOptions::default()
                    },
                    |window, cx| {
                        // Cuetty owns its entire window surface. gpui-component's
                        // generic Root paints an opaque theme background behind its
                        // child, which prevents the terminal alpha from ever reaching
                        // the macOS compositor.
                        cx.new(|cx| TerminalView::new(window, cx))
                    },
                )
                .expect("open Cuetty window");
            // The native window being active does not select a GPUI focus
            // target. Focus the live terminal once its view has been mounted so
            // its key handler receives input without an initial mouse click.
            window
                .update(cx, |view, window, _cx| {
                    if view.active_state().is_some() {
                        view.focus.focus(window);
                    }
                })
                .expect("focus Cuetty terminal");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::foreground::ForegroundProcess;
    use crate::terminal::host::SessionStart;
    use crate::terminal::model::Color;
    use crate::terminal::rio_input::{encode_key, encode_paste};
    use crate::terminal::selection::LogicalPosition;
    use rio_vt::crosswords::Mode;
    use std::collections::VecDeque;

    struct FakeSession {
        frame: TerminalFrame,
        effects: Arc<Mutex<Vec<ControlEvent>>>,
        close_error: Option<String>,
        foreground: Arc<Mutex<String>>,
        /// What a shell with bracketed paste on would read from the PTY.
        sent: Arc<Mutex<Vec<u8>>>,
    }

    impl TerminalSession for FakeSession {
        fn resize(&mut self, _: u32, _: u32, _: CellSize) -> Result<(), String> {
            Ok(())
        }

        fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String> {
            let bytes = encode_key(event, Mode::BRACKETED_PASTE, None).unwrap_or_default();
            self.sent.lock().unwrap().extend(bytes);
            Ok(true)
        }

        fn paste(&mut self, text: &str) -> Result<(), String> {
            let bytes = encode_paste(text, Mode::BRACKETED_PASTE);
            self.sent.lock().unwrap().extend(bytes);
            Ok(())
        }

        fn scroll(&mut self, _: TerminalScroll) -> Result<(), String> {
            Ok(())
        }

        fn frame(&mut self) -> TerminalFrame {
            self.frame.clone()
        }

        fn working_directory(&self) -> Option<String> {
            None
        }

        fn foreground_process(&self) -> Option<ForegroundProcess> {
            let foreground = self.foreground.lock().unwrap();
            let mut words = foreground.split_whitespace().map(str::to_owned);
            Some(ForegroundProcess {
                name: words.next().unwrap_or_default(),
                arguments: Some(words.collect()),
            })
        }

        fn drain_effects(&self) -> Vec<ControlEvent> {
            std::mem::take(&mut *self.effects.lock().expect("fake effects mutex poisoned"))
        }

        fn close(&mut self) -> Result<(), String> {
            self.close_error.take().map_or(Ok(()), Err)
        }
    }

    type FakeForeground = Arc<Mutex<String>>;
    type FakeEffects = Arc<Mutex<Vec<ControlEvent>>>;

    struct FakeFactory {
        starts: Mutex<VecDeque<Result<SessionStart, String>>>,
    }

    impl TerminalSessionFactory for FakeFactory {
        fn start(&self, _: u32, _: u32, _: CellSize) -> Result<SessionStart, String> {
            self.starts
                .lock()
                .expect("fake factory mutex poisoned")
                .pop_front()
                .unwrap_or_else(|| Err("fake factory exhausted".into()))
        }
    }

    fn frame_with(character: char) -> TerminalFrame {
        let mut frame = TerminalView::empty_frame();
        frame.rows[0].cells[0] = TerminalCell::narrow(character);
        frame
    }

    #[test]
    fn task_filter_matches_names_and_descriptions() {
        let tasks = vec![
            CuenvTask {
                name: "docs.build".into(),
                description: Some("Build the website".into()),
                kind: crate::integrations::cuenv::CuenvTaskKind::Task,
                requires_parameters: false,
            },
            CuenvTask {
                name: "lint".into(),
                description: None,
                kind: crate::integrations::cuenv::CuenvTaskKind::Task,
                requires_parameters: false,
            },
        ];
        assert_eq!(filtered_tasks(&tasks, "DOC").len(), 1);
        assert_eq!(filtered_tasks(&tasks, "website")[0].name, "docs.build");
        assert!(filtered_tasks(&tasks, "release").is_empty());
    }

    #[test]
    fn task_command_quotes_paths_and_stages_required_arguments() {
        let mut task = CuenvTask {
            name: "release's build".into(),
            description: None,
            kind: crate::integrations::cuenv::CuenvTaskKind::Task,
            requires_parameters: true,
        };
        let directory = std::path::Path::new("/tmp/project's dir");
        assert_eq!(
            task_command(directory, &task),
            "cuenv task --path '/tmp/project'\\''s dir' --package cuenv 'release'\\''s build' "
        );
        task.requires_parameters = false;
        assert_eq!(
            task_command(directory, &task),
            "cuenv task --path '/tmp/project'\\''s dir' --package cuenv 'release'\\''s build'"
        );
    }

    fn fake_start(
        character: char,
        effects: Vec<ControlEvent>,
        close_error: Option<&str>,
    ) -> (SessionStart, Arc<Mutex<Vec<ControlEvent>>>) {
        let (wake_tx, wake_rx) = async_channel::unbounded();
        drop(wake_tx);
        let effects = Arc::new(Mutex::new(effects));
        let session = FakeSession {
            frame: frame_with(character),
            effects: Arc::clone(&effects),
            close_error: close_error.map(str::to_owned),
            foreground: Arc::new(Mutex::new("zsh".into())),
            sent: Arc::default(),
        };
        (
            SessionStart {
                session: Box::new(session),
                wake_rx,
            },
            effects,
        )
    }

    /// A session whose foreground process and control events a test can change.
    fn fake_start_in_foreground(process: &str) -> (SessionStart, FakeForeground, FakeEffects) {
        let (wake_tx, wake_rx) = async_channel::unbounded();
        drop(wake_tx);
        let foreground = Arc::new(Mutex::new(process.to_owned()));
        let effects = Arc::new(Mutex::new(Vec::new()));
        let session = FakeSession {
            frame: frame_with('a'),
            effects: Arc::clone(&effects),
            close_error: None,
            foreground: Arc::clone(&foreground),
            sent: Arc::default(),
        };
        (
            SessionStart {
                session: Box::new(session),
                wake_rx,
            },
            foreground,
            effects,
        )
    }

    fn fake_factory(plans: Vec<Result<SessionStart, String>>) -> Arc<dyn TerminalSessionFactory> {
        Arc::new(FakeFactory {
            starts: Mutex::new(plans.into_iter().collect()),
        })
    }

    fn test_cell() -> CellSize {
        CellSize {
            width: 8,
            height: 16,
        }
    }

    #[test]
    fn wheel_scrolling_preserves_system_direction_and_accumulates_pixels() {
        let mut remainder = 0.0;
        assert_eq!(
            wheel_scroll_lines(
                ScrollDelta::Pixels(point(px(0.0), px(9.0))),
                20.0,
                &mut remainder
            ),
            0
        );
        assert_eq!(
            wheel_scroll_lines(
                ScrollDelta::Pixels(point(px(0.0), px(12.0))),
                20.0,
                &mut remainder
            ),
            1
        );
        assert!(remainder > 0.0);
        assert_eq!(
            wheel_scroll_lines(ScrollDelta::Lines(point(0.0, -2.0)), 20.0, &mut remainder),
            -1
        );
    }

    #[test]
    fn shell_control_chords_are_never_app_shortcuts() {
        let control = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(app_chord(control), None);
        for key in ["c", "d", "f", "k", "t", "v", "w"] {
            assert_eq!(TerminalView::workspace_shortcut(key, control), None);
        }
    }

    #[test]
    fn control_page_keys_select_tab_direction_without_stealing_other_chords() {
        let control = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            TerminalView::tab_shortcut("pageup", control),
            Some(TabDirection::Previous)
        );
        assert_eq!(
            TerminalView::tab_shortcut("PageDown", control),
            Some(TabDirection::Next)
        );
        assert_eq!(
            TerminalView::tab_shortcut("pageup", gpui::Modifiers::default()),
            None
        );
        for modifiers in [
            gpui::Modifiers {
                shift: true,
                ..control
            },
            gpui::Modifiers {
                alt: true,
                ..control
            },
            gpui::Modifiers {
                platform: true,
                ..control
            },
        ] {
            assert_eq!(TerminalView::tab_shortcut("pageup", modifiers), None);
        }
        assert_eq!(
            TerminalView::tab_shortcut(
                "pageup",
                gpui::Modifiers {
                    function: true,
                    ..control
                }
            ),
            Some(TabDirection::Previous)
        );
        assert_eq!(TerminalView::tab_shortcut("c", control), None);
    }

    #[test]
    fn tabs_are_selected_by_number_on_the_platform_modifier_alone() {
        let held = gpui::Modifiers {
            control: !cfg!(target_os = "macos"),
            platform: cfg!(target_os = "macos"),
            ..Default::default()
        };
        let digit = |key| tab_number_shortcut(key, held).map(TabNumber::digit);
        assert_eq!(digit("1"), Some(1));
        assert_eq!(digit("9"), Some(9));
        assert_eq!(digit("0"), None);
        assert_eq!(digit("c"), None);
        assert_eq!(tab_number_shortcut("1", gpui::Modifiers::default()), None);
        for extra in [
            gpui::Modifiers {
                shift: true,
                ..held
            },
            gpui::Modifiers { alt: true, ..held },
        ] {
            assert_eq!(tab_number_shortcut("1", extra), None);
        }
    }

    /// The modifiers that select a tab by number on this platform.
    fn tab_number_modifiers_for_test() -> gpui::Modifiers {
        let expected = tab_number_modifiers();
        gpui::Modifiers {
            control: expected.control,
            platform: expected.platform,
            ..gpui::Modifiers::default()
        }
    }

    fn shell_with_tabs(count: usize) -> WorkspaceShell {
        let mut shell = WorkspaceShell::new().unwrap();
        (1..count).for_each(|_| {
            shell.create_tab("Terminal").unwrap();
        });
        shell
    }

    #[test]
    fn a_number_reaching_no_tab_is_left_for_the_terminal() {
        let tabs = shell_with_tabs(1);
        let targets = ChordTargets {
            palette: None,
            tabs: &tabs,
        };
        let held = tab_number_modifiers_for_test();
        assert_eq!(
            route_chord("6", held, &targets),
            ChordRoute::Unclaimed,
            "Ctrl-6 has to stay RS for an application when there is no sixth tab",
        );
        assert_eq!(
            route_chord("1", held, &targets),
            ChordRoute::SelectTab(tabs.active_tab().unwrap()),
        );
    }

    #[test]
    fn a_palette_chord_on_a_digit_wins_only_when_there_is_a_catalogue() {
        let tabs = shell_with_tabs(1);
        let held = tab_number_modifiers_for_test();
        let palette = KeyChord {
            key: "1".into(),
            modifiers: tab_number_modifiers(),
        };
        let with_catalogue = ChordTargets {
            palette: Some(&palette),
            tabs: &tabs,
        };
        assert_eq!(
            route_chord("1", held, &with_catalogue),
            ChordRoute::OpenPalette
        );
        let without_catalogue = ChordTargets {
            palette: None,
            tabs: &tabs,
        };
        assert_eq!(
            route_chord("1", held, &without_catalogue),
            ChordRoute::SelectTab(tabs.active_tab().unwrap()),
        );
    }

    #[test]
    fn the_tab_hint_names_the_chord_that_selects_the_tab() {
        assert_eq!(
            tab_number_hint(TabNumber::from_key("3").unwrap()),
            if cfg!(target_os = "macos") {
                "⌘3"
            } else {
                "Ctrl+3"
            }
        );
    }

    #[test]
    fn the_app_chord_label_describes_a_chord_the_app_accepts() {
        let ChordModifiers {
            control,
            alt,
            shift,
            platform,
        } = app_chord_modifiers();
        assert_eq!(
            app_chord(gpui::Modifiers {
                control,
                alt,
                shift,
                platform,
                function: false,
            }),
            Some(AppChord::Primary)
        );
        assert_eq!(
            app_chord_label("b"),
            if cfg!(target_os = "macos") {
                "⌘B"
            } else {
                "Ctrl+Shift+B"
            }
        );
    }

    #[test]
    fn a_measurement_reports_a_change_only_when_it_differs() {
        let slot = Mutex::new((1, 2));
        assert!(!store_if_changed(&slot, (1, 2)));
        assert!(store_if_changed(&slot, (3, 2)));
        assert_eq!(*slot.lock().unwrap(), (3, 2));
        assert!(!store_if_changed(&slot, (3, 2)));
    }

    #[test]
    fn palette_chord_matches_exact_modifiers() {
        let chord = KeyChord {
            key: "k".into(),
            modifiers: ChordModifiers {
                control: true,
                ..ChordModifiers::default()
            },
        };
        let control = gpui::Modifiers {
            control: true,
            ..Default::default()
        };
        assert!(chord_matches(&chord, "k", control));
        assert!(chord_matches(&chord, "K", control));
        assert!(!chord_matches(&chord, "j", control));
        assert!(!chord_matches(
            &chord,
            "k",
            gpui::Modifiers {
                shift: true,
                ..control
            }
        ));
        assert!(!chord_matches(&chord, "k", gpui::Modifiers::default()));
    }

    #[test]
    fn app_chords_follow_the_platform_convention() {
        let (primary, alternate) = if cfg!(target_os = "macos") {
            let primary = gpui::Modifiers {
                platform: true,
                ..Default::default()
            };
            (
                primary,
                gpui::Modifiers {
                    shift: true,
                    ..primary
                },
            )
        } else {
            let primary = gpui::Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            };
            (
                primary,
                gpui::Modifiers {
                    alt: true,
                    ..primary
                },
            )
        };
        assert_eq!(app_chord(primary), Some(AppChord::Primary));
        assert_eq!(app_chord(alternate), Some(AppChord::Alternate));
        assert_eq!(
            TerminalView::workspace_shortcut("d", primary),
            Some(WorkspaceShortcut::SplitHorizontal)
        );
        assert_eq!(
            TerminalView::workspace_shortcut("d", alternate),
            Some(WorkspaceShortcut::SplitVertical)
        );
    }

    #[test]
    fn macos_control_chords_fall_back_to_the_layout_aware_key() {
        assert_eq!(
            terminal_key_input("c", None),
            Some(KeyInput::Character('c'))
        );
        assert_eq!(
            terminal_key_input("u", None),
            Some(KeyInput::Character('u'))
        );
        assert_eq!(
            terminal_key_input("space", None),
            Some(KeyInput::Character(' '))
        );
        assert_eq!(terminal_key_input("pageup", None), None);
    }

    #[test]
    fn renderer_applies_inverse_once_from_semantic_frame() {
        let mut cell = TerminalCell::narrow('x');
        cell.foreground = Color::Rgb(Rgb(1, 2, 3));
        cell.background = Color::Rgb(Rgb(4, 5, 6));
        cell.style.inverse = true;
        assert_eq!(
            colors(&cell, &TerminalTheme::default(), CellLayer::Cell),
            (Rgb(4, 5, 6), Rgb(1, 2, 3))
        );
    }

    #[test]
    fn current_search_match_replaces_the_cell_colours_with_its_own_pair() {
        let theme = TerminalTheme::default();
        let mut cell = TerminalCell::narrow('x');
        cell.foreground = Color::Rgb(Rgb(1, 2, 3));
        assert_eq!(
            colors(&cell, &theme, CellLayer::CurrentSearchMatch),
            (theme.current_search_match_text, theme.current_search_match)
        );
    }

    #[test]
    fn cell_layer_splits_at_the_edges_of_the_current_match_only() {
        let span = |kind, row| CellOverlaySpan {
            row,
            start_column: 2,
            end_column: 4,
            kind,
        };
        let overlays = [
            span(OverlayKind::Selection, 0),
            span(OverlayKind::CurrentSearchMatch, 1),
        ];
        let at = |row, column| cell_layer(None, &overlays, CellPosition { row, column });
        assert_eq!(at(1, 1), CellLayer::Cell);
        assert_eq!(at(1, 2), CellLayer::CurrentSearchMatch);
        assert_eq!(at(1, 3), CellLayer::CurrentSearchMatch);
        assert_eq!(at(1, 4), CellLayer::Cell);
        // A selection and another row's match leave the cell's own colours.
        assert_eq!(at(0, 2), CellLayer::Cell);
    }

    #[test]
    fn only_a_block_cursor_outranks_the_current_match() {
        let overlays = [CellOverlaySpan {
            row: 0,
            start_column: 0,
            end_column: 1,
            kind: OverlayKind::CurrentSearchMatch,
        }];
        let position = CellPosition { row: 0, column: 0 };
        assert_eq!(
            cell_layer(Some(HostCursorStyle::Block), &overlays, position),
            CellLayer::BlockCursor
        );
        assert_eq!(
            cell_layer(Some(HostCursorStyle::Bar), &overlays, position),
            CellLayer::CurrentSearchMatch
        );
    }

    #[test]
    fn only_default_non_inverse_cells_share_the_transparent_surface() {
        let default = TerminalCell::narrow(' ');
        assert!(uses_terminal_surface_background(&default));

        let mut explicit = default.clone();
        explicit.background = Color::Rgb(Rgb(4, 5, 6));
        assert!(!uses_terminal_surface_background(&explicit));

        let mut inverse = default;
        inverse.style.inverse = true;
        assert!(!uses_terminal_surface_background(&inverse));
    }

    #[test]
    fn hidden_overrides_dimmed_foreground() {
        let mut cell = TerminalCell::narrow('x');
        cell.foreground = Color::Rgb(Rgb(100, 120, 140));
        cell.background = Color::Rgb(Rgb(4, 5, 6));
        cell.style.dim = true;
        cell.style.hidden = true;

        assert_eq!(
            colors(&cell, &TerminalTheme::default(), CellLayer::Cell),
            (Rgb(4, 5, 6), Rgb(4, 5, 6))
        );
    }

    #[test]
    fn cursor_unknown_policy_is_visible_block_while_focused() {
        let mut frame = TerminalView::empty_frame();
        frame.cursor.position = Some((0, 0));
        assert_eq!(
            cursor_style_at(&frame, true, 0, 0),
            Some(HostCursorStyle::Block)
        );
        assert_eq!(cursor_style_at(&frame, false, 0, 0), None);
        assert_eq!(cursor_style_at(&frame, true, 0, 1), None);
        frame.cursor.visible = Known::Known(false);
        assert_eq!(cursor_style_at(&frame, true, 0, 0), None);
    }

    #[test]
    fn terminal_focus_requires_a_live_pane_as_well_as_window_focus() {
        assert!(terminal_has_focus(true, true));
        assert!(!terminal_has_focus(false, true));
        assert!(!terminal_has_focus(true, false));
        assert!(!terminal_has_focus(false, false));
    }

    #[test]
    fn split_rejection_is_a_recoverable_shell_notice() {
        let notice = shell_notice(ShellError::MultiSessionUnavailable);
        assert!(notice.contains("split panes"));
    }

    #[test]
    fn short_or_empty_frames_do_not_produce_invalid_row_slices() {
        let mut frame = TerminalView::empty_frame();
        frame.dimensions.rows = 3;
        assert_eq!(frame_row(&frame, 0).len(), 1);
        assert!(frame_row(&frame, 1).is_empty());
    }

    #[test]
    fn spacers_do_not_supply_text_for_shaping() {
        let cells = [
            TerminalCell::wide('界'),
            TerminalCell::trailing_spacer(),
            TerminalCell::narrow('x'),
        ];
        let shaped: String = cells
            .iter()
            .filter_map(|cell| cell.text.as_deref())
            .collect();
        assert_eq!(shaped, "界x");
        assert!(cells.iter().any(|cell| cell.text.is_none()));
    }

    #[test]
    fn failed_tab_creation_leaves_workspace_and_registry_unchanged() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Err("spawn failed".into())]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let active = registry.active_pane().unwrap();
        assert_eq!(registry.workspace.workspace().tabs().len(), 1);
        assert_eq!(registry.sessions.len(), 1);
        let error = registry.start_tab(80, 40, test_cell()).unwrap_err();
        assert_eq!(error, RegistryError::Session("spawn failed".into()));
        assert_eq!(registry.active_pane(), Some(active));
        assert_eq!(registry.workspace.workspace().tabs().len(), 1);
        assert_eq!(registry.sessions.len(), 1);
    }

    #[test]
    fn terminal_configuration_follows_the_active_tab() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, _) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let first_tab = registry.workspace.active_tab().unwrap();
        registry
            .sessions
            .get_mut(&first_pane)
            .unwrap()
            .cuenv
            .presentation
            .terminal
            .font
            .size_px = 20.0;
        assert_eq!(registry.active_terminal_config().font.size_px, 20.0);

        let (second_pane, _, _) = registry.start_tab(80, 40, test_cell()).unwrap();
        registry
            .sessions
            .get_mut(&second_pane)
            .unwrap()
            .cuenv
            .presentation
            .terminal
            .terminal_padding
            .left = 2;
        // A new tab starts with the look of the tab it opened from, not the defaults.
        assert_eq!(registry.active_terminal_config().font.size_px, 20.0);
        assert_eq!(registry.active_terminal_config().terminal_padding.left, 2);

        // Updating an inactive pane must not change the active configuration.
        registry
            .sessions
            .get_mut(&first_pane)
            .unwrap()
            .cuenv
            .presentation
            .terminal
            .font
            .size_px = 24.0;
        assert_eq!(registry.active_terminal_config().font.size_px, 20.0);
        let previous_tab = registry
            .workspace
            .tab_in_direction(TabDirection::Previous)
            .unwrap();
        assert_eq!(previous_tab, first_tab);
        registry
            .workspace
            .apply(ShellAction::ActivateTab(previous_tab))
            .unwrap();
        assert_eq!(registry.active_pane(), Some(first_pane));
        assert_eq!(registry.active_terminal_config().font.size_px, 24.0);
        assert_eq!(registry.active_terminal_config().terminal_padding.left, 14);

        let next_tab = registry
            .workspace
            .tab_in_direction(TabDirection::Next)
            .unwrap();
        registry
            .workspace
            .apply(ShellAction::ActivateTab(next_tab))
            .unwrap();
        assert_eq!(registry.active_pane(), Some(second_pane));
        assert_eq!(registry.active_terminal_config().terminal_padding.left, 2);
    }

    #[test]
    fn tab_lifecycle_is_exact_and_final_close_requests_window_removal() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, _) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let (second_pane, _, _) = registry.start_tab(80, 40, test_cell()).unwrap();
        assert_eq!(registry.workspace.workspace().tabs().len(), 2);
        assert_eq!(registry.sessions.len(), 2);

        registry.close_pane(first_pane).unwrap();
        assert!(!registry.close_window_requested);
        assert_eq!(registry.active_pane(), Some(second_pane));
        assert_eq!(registry.sessions.len(), 1);

        registry.close_pane(second_pane).unwrap();
        assert!(registry.close_window_requested);
        assert!(registry.active_pane().is_none());
        assert!(registry.sessions.is_empty());
    }

    #[test]
    fn close_failure_is_visible_but_does_not_leave_a_dead_tab() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, _) = fake_start('b', Vec::new(), Some("already exited"));
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let _ = registry.start_tab(80, 40, test_cell()).unwrap();
        let second_pane = registry.active_pane().unwrap();
        registry.close_pane(second_pane).unwrap();
        assert_eq!(registry.workspace.workspace().tabs().len(), 1);
        assert_eq!(registry.sessions.len(), 1);
        assert!(
            registry
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("already exited"))
        );
    }

    #[test]
    fn wake_routing_is_generation_safe_and_background_clipboard_is_suppressed() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, effects) = fake_start(
            'b',
            vec![
                ControlEvent::ClipboardWrite("background".into()),
                ControlEvent::Title("background title".into()),
            ],
            None,
        );
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let (second_pane, generation, _) = registry.start_tab(80, 40, test_cell()).unwrap();

        let background = registry.apply_wake(second_pane, generation, Some(first_pane));
        assert!(background.accepted);
        assert!(background.clipboard.is_empty());
        assert_eq!(
            registry.sessions[&second_pane].activity.title(),
            Some("background title")
        );
        assert_eq!(registry.sessions[&first_pane].activity.title(), None);

        effects
            .lock()
            .unwrap()
            .push(ControlEvent::ClipboardWrite("foreground".into()));
        let foreground = registry.apply_wake(second_pane, generation, Some(second_pane));
        assert_eq!(foreground.clipboard, vec!["foreground"]);

        registry.close_pane(second_pane).unwrap();
        let late = registry.apply_wake(second_pane, generation, Some(first_pane));
        assert!(!late.accepted);
        assert!(late.clipboard.is_empty());
    }

    #[test]
    fn a_program_clearing_its_title_leaves_the_tab_untitled() {
        let (initial, effects) = fake_start('a', vec![ControlEvent::Title("vim".into())], None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let pane = registry.active_pane().unwrap();
        let generation = registry.sessions[&pane].generation;
        registry.apply_wake(pane, generation, Some(pane));
        assert_eq!(registry.sessions[&pane].activity.title(), Some("vim"));

        effects
            .lock()
            .unwrap()
            .push(ControlEvent::Title(String::new()));
        registry.apply_wake(pane, generation, Some(pane));
        assert_eq!(registry.sessions[&pane].activity.title(), None);
    }

    #[test]
    fn a_tab_is_named_after_its_task_while_it_runs_and_its_program_otherwise() {
        let (initial, foreground, _) = fake_start_in_foreground("zsh");
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let pane = registry.active_pane().unwrap();
        let tab = registry.workspace.workspace().tabs()[0].clone();
        assert_eq!(registry.tab_label(&tab), "Terminal");

        let task = CuenvTask {
            name: "docs.build".into(),
            description: None,
            kind: crate::integrations::cuenv::CuenvTaskKind::Task,
            requires_parameters: false,
        };
        let state = registry.sessions.get_mut(&pane).unwrap();
        state.launch_task("cuenv task docs.build", &task).unwrap();
        assert_eq!(registry.tab_label(&tab), "docs.build");

        let mut named = tab.clone();
        named.custom_name = Some("site".into());
        assert_eq!(registry.tab_label(&named), "site");

        // The fake's shell never gave up the foreground, so the task ended
        // between samples, and a sampled shell is not a program to name.
        registry.sample_foreground(std::time::Instant::now() + std::time::Duration::from_secs(60));
        assert_eq!(registry.sessions[&pane].activity.task_name(), None);
        assert_eq!(registry.sessions[&pane].activity.program(), None);
        assert_eq!(registry.tab_label(&tab), "Terminal");

        *foreground.lock().unwrap() = "cuenv".into();
        registry.sample_foreground(std::time::Instant::now());
        assert_eq!(registry.tab_label(&tab), "cuenv");
    }

    #[test]
    fn the_active_tab_label_follows_the_active_tab_and_goes_with_it() {
        let (initial, foreground, _) = fake_start_in_foreground("zsh");
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        assert_eq!(registry.active_tab_label().as_deref(), Some("Terminal"));

        *foreground.lock().unwrap() = "claude".into();
        registry.sample_foreground(std::time::Instant::now());
        assert_eq!(registry.active_tab_label().as_deref(), Some("claude"));

        let empty = SessionRegistry::empty(fake_factory(Vec::new()));
        assert_eq!(empty.active_tab_label(), None);
    }

    #[test]
    fn a_title_lasts_exactly_as_long_as_the_program_that_set_it() {
        let (initial, foreground, effects) = fake_start_in_foreground("zsh");
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let pane = registry.active_pane().unwrap();
        let generation = registry.sessions[&pane].generation;
        let tab = registry.workspace.workspace().tabs()[0].clone();

        // The program starts and sets its title between two periodic samples.
        *foreground.lock().unwrap() = "deploy".into();
        effects
            .lock()
            .unwrap()
            .push(ControlEvent::Title("deploy logs".into()));
        registry.apply_wake(pane, generation, Some(pane));
        registry.sample_foreground(std::time::Instant::now());
        assert_eq!(registry.tab_label(&tab), "deploy logs");

        *foreground.lock().unwrap() = "zsh".into();
        registry.sample_foreground(std::time::Instant::now());
        assert_eq!(registry.tab_label(&tab), "Terminal");
    }

    /// A pane whose shell has bracketed paste on, and what it has been sent.
    fn bracketed_pane(process: &str) -> (PaneState, Arc<Mutex<Vec<u8>>>) {
        let sent = Arc::<Mutex<Vec<u8>>>::default();
        let session = FakeSession {
            frame: frame_with('a'),
            effects: Arc::default(),
            close_error: None,
            foreground: Arc::new(Mutex::new(process.to_owned())),
            sent: Arc::clone(&sent),
        };
        let pane = PaneState::new(SessionGeneration(1), Box::new(session), (80, 40));
        (pane, sent)
    }

    fn runnable_task(name: &str) -> CuenvTask {
        CuenvTask {
            name: name.into(),
            description: None,
            kind: crate::integrations::cuenv::CuenvTaskKind::Task,
            requires_parameters: false,
        }
    }

    #[test]
    fn running_a_task_presses_enter_outside_the_bracketed_paste() {
        let (mut pane, sent) = bracketed_pane("zsh");
        let task = runnable_task("docs.build");
        let command = task_command(std::path::Path::new("/tmp/x"), &task);
        assert_eq!(pane.launch_task(&command, &task), Ok(TaskLaunch::Started));
        let expected = format!("\x1b[200~{command}\x1b[201~\r");
        assert_eq!(*sent.lock().unwrap(), expected.into_bytes());
        assert_eq!(pane.activity.task_name(), Some("docs.build"));
        assert_eq!(pane.marker_notice(), None);
    }

    #[test]
    fn a_task_needing_arguments_is_left_at_the_prompt_unsent() {
        let (mut pane, sent) = bracketed_pane("zsh");
        let mut task = runnable_task("release");
        task.requires_parameters = true;
        let command = task_command(std::path::Path::new("/tmp/x"), &task);
        assert_eq!(
            pane.launch_task(&command, &task),
            Ok(TaskLaunch::AwaitingArguments)
        );
        let expected = format!("\x1b[200~{command}\x1b[201~");
        assert_eq!(*sent.lock().unwrap(), expected.into_bytes());
        assert_eq!(
            pane.marker_notice(),
            Some("Add arguments for 'release' and press Enter")
        );
    }

    fn pressed(key: KeyInput, modifiers: InputModifiers) -> TerminalKeyEvent {
        TerminalKeyEvent {
            key,
            modifiers,
            repeat: false,
        }
    }

    #[test]
    fn a_staged_tasks_hint_lasts_until_the_line_is_submitted_or_abandoned() {
        let plain = InputModifiers::default();
        for finishing in [
            pressed(KeyInput::Enter, plain),
            pressed(KeyInput::Character('c'), InputModifiers::CONTROL),
        ] {
            let (mut pane, _) = bracketed_pane("zsh");
            let mut task = runnable_task("release");
            task.requires_parameters = true;
            pane.launch_task("cuenv task release ", &task).unwrap();
            // Typing the arguments is what the hint asked for.
            for key in [KeyInput::Character('c'), KeyInput::Character('1')] {
                pane.input(pressed(key, plain)).unwrap();
            }
            assert!(pane.marker_notice().is_some());
            pane.input(finishing).unwrap();
            assert_eq!(pane.marker_notice(), None);
        }
    }

    #[test]
    fn a_configuration_notice_outranks_a_staged_tasks_hint() {
        let (mut pane, _) = bracketed_pane("claude");
        pane.launch_task("cuenv task docs.build", &runnable_task("docs.build"))
            .unwrap();
        pane.cuenv.task_notice = Some("tasks did not evaluate".into());
        assert_eq!(pane.marker_notice(), Some("tasks did not evaluate"));
        pane.cuenv.notice = Some("cuetty.cue is invalid".into());
        assert_eq!(pane.marker_notice(), Some("cuetty.cue is invalid"));
        pane.cuenv.notice = None;
        pane.cuenv.task_notice = None;
        assert_eq!(
            pane.marker_notice(),
            Some("Pasted 'docs.build'; press Enter to run it")
        );
    }

    #[test]
    fn enter_is_never_pressed_for_a_program_or_a_foreground_that_cannot_be_named() {
        // Enter would submit whatever the program holds, such as an agent's prompt.
        // In a shell running a script, it would answer whatever the script is asking.
        for process in ["claude", "", "bash deploy.sh", "sh -c make"] {
            let (mut pane, sent) = bracketed_pane(process);
            let task = runnable_task("docs.build");
            assert_eq!(
                pane.launch_task("cuenv task docs.build", &task),
                Ok(TaskLaunch::AwaitingEnter)
            );
            let expected = "\x1b[200~cuenv task docs.build\x1b[201~";
            assert_eq!(*sent.lock().unwrap(), expected.as_bytes());
            assert_eq!(pane.activity.task_name(), None);
            assert_eq!(
                pane.marker_notice(),
                Some("Pasted 'docs.build'; press Enter to run it")
            );
        }
    }

    #[test]
    fn a_task_needing_arguments_or_a_busy_pane_is_not_marked_running() {
        let (initial, foreground, _) = fake_start_in_foreground("zsh");
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        let mut task = CuenvTask {
            name: "release".into(),
            description: None,
            kind: crate::integrations::cuenv::CuenvTaskKind::Task,
            requires_parameters: true,
        };
        state.launch_task("cuenv task release ", &task).unwrap();
        assert_eq!(state.activity.task_name(), None);

        // Vim started after the last periodic sample, which still says the shell.
        task.requires_parameters = false;
        *foreground.lock().unwrap() = "vim".into();
        state.launch_task("cuenv task release", &task).unwrap();
        assert_eq!(state.activity.task_name(), None);
    }

    #[test]
    fn close_wake_samples_the_final_frame_before_requesting_pane_removal() {
        let (initial, _) = fake_start('z', vec![ControlEvent::Close], None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        let generation = state.generation;
        state.frame = frame_with('a');

        let outcome = registry.apply_wake(pane, generation, Some(pane));
        assert!(outcome.accepted);
        assert!(outcome.should_close);
        assert_eq!(
            registry.sessions[&pane].frame.rows[0].cells[0]
                .text
                .as_deref(),
            Some("z")
        );
        // The UI may now remove the pane; this is an ordering guarantee,
        // not a promise to retain exited output on screen.
    }

    #[test]
    fn interaction_state_is_owned_by_the_tab_that_owns_the_session() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, _) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, 80, 40, test_cell()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let (second_pane, _, _) = registry.start_tab(80, 40, test_cell()).unwrap();
        let hit = CellHitTest {
            cell_width: 1.0,
            cell_height: 1.0,
            rows: 1,
            columns: 1,
        };
        let first_frame = registry.sessions[&first_pane].frame.clone();
        registry
            .sessions
            .get_mut(&first_pane)
            .unwrap()
            .interaction
            .enter_search(&first_frame);
        registry
            .sessions
            .get_mut(&second_pane)
            .unwrap()
            .interaction
            .begin_selection(hit, LogicalPosition { line: 0, column: 0 });
        assert!(
            registry.sessions[&first_pane]
                .interaction
                .search()
                .is_some()
        );
        assert!(
            registry.sessions[&first_pane]
                .interaction
                .selection()
                .is_none()
        );
        assert!(
            registry.sessions[&second_pane]
                .interaction
                .search()
                .is_none()
        );
        assert!(
            registry.sessions[&second_pane]
                .interaction
                .selection()
                .is_some()
        );
        assert_eq!(
            registry.sessions[&first_pane].frame.rows[0].cells[0]
                .text
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            registry.sessions[&second_pane].frame.rows[0].cells[0]
                .text
                .as_deref(),
            Some("b")
        );
    }
}
