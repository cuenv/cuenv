use super::chrome::{render_shell_message, traffic_light_position};
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
use super::workspace_shell::{
    ShellAction, ShellError, TabDirection, WorkspaceShell, WorkspaceShortcut,
};
use crate::integrations::cuenv::{
    ConfigSource, CuenvPaneState, CuenvProvider, CuenvTask, CuenvWatcher, Evaluation, KeyChord,
    NativeCuenvProvider, TaskEvaluation,
};
use crate::workspace::{LayoutRect, MinSizePolicy, TabId};
use gpui::{
    App, AppContext, Application, BorderStyle, Bounds, ClipboardItem, Context, Edges, Element,
    FocusHandle, Font, FontFeatures, FontStyle, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyBinding, Menu, MenuItem, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    StrikethroughStyle, Styled, SystemMenuType, TextRun, Timer,
    UnderlineStyle as GpuiUnderlineStyle, Window, WindowBackgroundAppearance, WindowBounds,
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
                let (_, background) = colors(&cells[start], &self.theme, false);
                let uses_default_background = uses_terminal_surface_background(&cells[start]);
                let mut end = start + 1;
                while end < cells.len()
                    && colors(&cells[end], &self.theme, false).1 == background
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
                let cursor_block = cursor_style_at(&self.frame, self.focused, row, start)
                    .is_some_and(|style| matches!(style, HostCursorStyle::Block));
                let (foreground, _) = colors(cell, &self.theme, cursor_block);
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
                    let next_cursor_block = cursor_style_at(&self.frame, self.focused, row, end)
                        .is_some_and(|style| matches!(style, HostCursorStyle::Block));
                    let (next_fg, _) = colors(next, &self.theme, next_cursor_block);
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

fn colors(cell: &TerminalCell, theme: &TerminalTheme, cursor: bool) -> (Rgb, Rgb) {
    if cursor {
        return (theme.cursor_text, theme.cursor);
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
    pub(super) title: String,
    measured_bounds: Arc<Mutex<(u32, u32)>>,
    viewport_bounds: Arc<Mutex<ViewportBounds>>,
    error: Option<String>,
    pub(super) cuenv: CuenvPaneState,
    cuenv_source: Option<ConfigSource>,
    cuenv_watcher: Option<Box<dyn CuenvWatcher>>,
    cuenv_watch_retry_at: Option<std::time::Instant>,
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
        let mut start = factory
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
        let frame = start.session.frame();
        let mut sessions = HashMap::new();
        sessions.insert(
            pane,
            PaneState {
                generation: SessionGeneration(1),
                session: start.session,
                frame,
                interaction: InteractionState::default(),
                scroll_remainder: 0.0,
                title: "Cuetty".into(),
                measured_bounds: Arc::new(Mutex::new((width, height))),
                viewport_bounds: Arc::new(Mutex::new((0.0, 0.0, width as f32, height as f32))),
                error: None,
                cuenv: CuenvPaneState::default(),
                cuenv_source: None,
                cuenv_watcher: None,
                cuenv_watch_retry_at: None,
            },
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
        let frame = start.session.frame();
        self.sessions.insert(
            target.pane,
            PaneState {
                generation,
                session: start.session,
                frame,
                interaction: InteractionState::default(),
                scroll_remainder: 0.0,
                title: "Cuetty".into(),
                measured_bounds: Arc::new(Mutex::new((width, height))),
                viewport_bounds: Arc::new(Mutex::new((0.0, 0.0, width as f32, height as f32))),
                error: None,
                cuenv: CuenvPaneState::default(),
                cuenv_source: None,
                cuenv_watcher: None,
                cuenv_watch_retry_at: None,
            },
        );
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
                        state.title = title;
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
    metrics: TerminalMetrics,
    theme: TerminalTheme,
    focus: FocusHandle,
    renderer: Box<dyn TerminalRenderer>,
    workspace_bounds: Arc<Mutex<(f32, f32)>>,
    window_background: WindowBackgroundAppearance,
    pub(super) task_palette_open: bool,
    pub(super) task_sidebar_open: bool,
    pub(super) task_query: String,
    pub(super) task_selection: usize,
}

struct CuenvIntegrationUpdate {
    presentation: Evaluation,
    tasks: TaskEvaluation,
}

#[derive(Clone, Copy)]
enum TaskLaunch {
    Run,
    EditRequiredArguments,
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

fn task_command(cwd: &std::path::Path, task: &CuenvTask, launch: TaskLaunch) -> String {
    let mut command = format!(
        "cuenv task --path {} --package cuenv {}",
        shell_quote(&cwd.to_string_lossy()),
        shell_quote(&task.name)
    );
    match launch {
        TaskLaunch::Run => command.push('\n'),
        TaskLaunch::EditRequiredArguments => command.push(' '),
    }
    command
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

/// Whether a key press is exactly the configured chord.
fn chord_matches(chord: &KeyChord, key: &str, modifiers: gpui::Modifiers) -> bool {
    key.eq_ignore_ascii_case(&chord.key)
        && modifiers.control == chord.modifiers.control
        && modifiers.alt == chord.modifiers.alt
        && modifiers.shift == chord.modifiers.shift
        && modifiers.platform == chord.modifiers.platform
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
        let config = TerminalConfig::default();
        let metrics = TerminalMetrics::from_config(cx.text_system(), &config);
        let theme = TerminalTheme::from_config(&config);
        let window_background = theme.window_background();
        let factory: Arc<dyn TerminalSessionFactory> = Arc::new(RioTerminalFactory);
        match SessionRegistry::start(factory.clone(), 720, 432, metrics.rio_cell()) {
            Ok((registry, wake_rx)) => {
                let pane = registry.active_pane().expect("initial workspace pane");
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
                    task_palette_open: false,
                    task_sidebar_open: false,
                    task_query: String::new(),
                    task_selection: 0,
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
                    task_palette_open: false,
                    task_sidebar_open: false,
                    task_query: String::new(),
                    task_selection: 0,
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
                if state.cuenv_source.take().is_some() {
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
        let launch = if task.requires_parameters {
            TaskLaunch::EditRequiredArguments
        } else {
            TaskLaunch::Run
        };
        let command = task_command(&cwd, &task, launch);
        if let Some(state) = self.registry.sessions.get_mut(&active_pane) {
            if let Err(error) = state.session.paste(&command) {
                state.error = Some(error);
            } else if task.requires_parameters {
                self.registry.notice =
                    Some(format!("Add arguments for '{}' and press Enter", task.name));
            }
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
                    .min(9);
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

    fn key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if let Some(direction) = Self::tab_shortcut(key, modifiers) {
            if let Some(tab) = self.registry.workspace.tab_in_direction(direction) {
                self.select_terminal_tab(tab, window, cx);
            }
            return;
        }
        let palette_chord = self.active_state().is_some_and(|state| {
            chord_matches(
                &state.cuenv.presentation.keybindings.palette,
                key,
                modifiers,
            )
        });
        // Without a task catalogue the chord falls through, so a plain
        // Ctrl-K palette binding still reaches the shell as kill-line.
        if palette_chord && self.toggle_task_palette() {
            cx.notify();
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
            && let Err(error) = state.session.input(TerminalKeyEvent {
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
        let theme = self.theme.clone();
        let metrics = self.metrics.clone();
        let padding = self.config.terminal_padding;
        let active_tab = self.active_tab();
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
        let frame = state.frame.clone();
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
        if let Some(state) = self.registry.sessions.get_mut(&active_pane) {
            // The canvas is inside the actual content panel, so this includes
            // the title strip, terminal inset, divider, and the current rail
            // width without trying to reconstruct GPUI's layout arithmetic.
            let bounds = *state.measured_bounds.lock().expect("bounds mutex poisoned");
            if let Err(error) = state
                .session
                .resize(bounds.0, bounds.1, self.metrics.rio_cell())
            {
                state.error = Some(error);
            }
            state.frame = state.session.frame();
        }
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
                    let changed = {
                        let mut current = workspace_bounds
                            .lock()
                            .expect("workspace bounds mutex poisoned");
                        if *current != next {
                            *current = next;
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        let _ = workspace_entity.update(app, |_view, cx| cx.notify());
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
                                    let changed = {
                                        let mut current =
                                            pane_measured.lock().expect("bounds mutex poisoned");
                                        if *current != next {
                                            *current = next;
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if changed {
                                        let _ = pane_entity.update(app, |_view, cx| cx.notify());
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
        let surface = div().relative().size_full().overflow_hidden().child(
            terminal_surface
                .absolute()
                .top(px(f32::from(padding.top)))
                .right(px(f32::from(padding.right)))
                .bottom(px(f32::from(padding.bottom)))
                .left(px(f32::from(padding.left))),
        );
        self.render_shell_layout(sidebar, surface.into_any_element(), &theme)
    }
}

pub fn run() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
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
                        title: None,
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
    use crate::integrations::cuenv::ChordModifiers;
    use crate::terminal::host::SessionStart;
    use crate::terminal::model::Color;
    use crate::terminal::selection::LogicalPosition;
    use std::collections::VecDeque;

    struct FakeSession {
        frame: TerminalFrame,
        effects: Arc<Mutex<Vec<ControlEvent>>>,
        close_error: Option<String>,
    }

    impl TerminalSession for FakeSession {
        fn resize(&mut self, _: u32, _: u32, _: CellSize) -> Result<(), String> {
            Ok(())
        }

        fn input(&mut self, _: TerminalKeyEvent) -> Result<bool, String> {
            Ok(true)
        }

        fn paste(&mut self, _: &str) -> Result<(), String> {
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

        fn drain_effects(&self) -> Vec<ControlEvent> {
            std::mem::take(&mut *self.effects.lock().expect("fake effects mutex poisoned"))
        }

        fn close(&mut self) -> Result<(), String> {
            self.close_error.take().map_or(Ok(()), Err)
        }
    }

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
        let task = CuenvTask {
            name: "release's build".into(),
            description: None,
            kind: crate::integrations::cuenv::CuenvTaskKind::Task,
            requires_parameters: true,
        };
        assert_eq!(
            task_command(
                std::path::Path::new("/tmp/project's dir"),
                &task,
                TaskLaunch::EditRequiredArguments,
            ),
            "cuenv task --path '/tmp/project'\\''s dir' --package cuenv 'release'\\''s build' "
        );
        assert!(
            task_command(std::path::Path::new("/tmp/x"), &task, TaskLaunch::Run).ends_with('\n')
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
        };
        (
            SessionStart {
                session: Box::new(session),
                wake_rx,
            },
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
            colors(&cell, &TerminalTheme::default(), false),
            (Rgb(4, 5, 6), Rgb(1, 2, 3))
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
            colors(&cell, &TerminalTheme::default(), false),
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
        assert_eq!(registry.active_terminal_config().font.size_px, 16.0);
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
        assert_eq!(registry.active_terminal_config().font.size_px, 16.0);
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
        assert_eq!(registry.sessions[&second_pane].title, "background title");
        assert_eq!(registry.sessions[&first_pane].title, "Cuetty");

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
