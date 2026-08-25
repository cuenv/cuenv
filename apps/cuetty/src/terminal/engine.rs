use super::config::TerminalConfig;
use super::events::ControlEvent;
use super::host::{RioTerminalFactory, TerminalSession, TerminalSessionFactory};
use super::input::{InputModifiers, KeyInput, TerminalKeyEvent};
use super::interaction::{CellHitTest, InteractionState, SearchKey};
use super::model::{CursorShape, Known, Rgb, TerminalCell, TerminalFrame, UnderlineStyle};
#[cfg(test)]
use super::model::{CursorState, SamplingToken, TerminalDimensions, TerminalRow};
use super::overlay::{CellOverlaySpan, OverlayKind, TerminalOverlays};
use super::presentation::{TerminalMetrics, TerminalTheme, gpui_rgb};
use super::settings::SettingsDraft;
use super::sizing::CellSize;
use super::workspace_shell::{ShellAction, ShellError, WorkspaceShell, WorkspaceShortcut};
use crate::workspace::{LayoutRect, MinSizePolicy, TabId};
use gpui::{
    App, AppContext, Application, BorderStyle, Bounds, ClipboardItem, Context, Edges, Element,
    FocusHandle, Font, FontFeatures, FontStyle, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyBinding, Menu, MenuItem, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Render, SharedString, Size, StatefulInteractiveElement,
    StrikethroughStyle, Styled, SystemMenuType, TextRun, UnderlineStyle as GpuiUnderlineStyle,
    Window, WindowBounds, WindowControlArea, WindowOptions, actions, canvas, div, point,
    prelude::FluentBuilder, px, quad, size,
};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::tooltip::Tooltip;
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

fn shell_notice_text(notice: Option<&str>) -> Option<String> {
    notice.map(|notice| format!("• {notice}"))
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
                let mut end = start + 1;
                while end < cells.len() && colors(&cells[end], &self.theme, false).1 == background {
                    end += 1;
                }
                window.paint_quad(quad(
                    self.cell_bounds(bounds.origin, row, start, end - start),
                    px(0.0),
                    gpui::rgb(gpui_rgb(background)),
                    Edges::default(),
                    Hsla::transparent_black(),
                    BorderStyle::default(),
                ));
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
                if cell.codepoint.is_none() {
                    start += 1;
                    continue;
                }
                let cursor_block = cursor_style_at(&self.frame, self.focused, row, start)
                    .is_some_and(|style| matches!(style, HostCursorStyle::Block));
                let (foreground, _) = colors(cell, &self.theme, cursor_block);
                let decoration_color = cell
                    .underline_color
                    .map(|color| self.theme.resolve(color, true))
                    .unwrap_or(foreground);
                let mut end = start + 1;
                while end < cells.len() {
                    let next = &cells[end];
                    if next.codepoint.is_none() {
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
                    .filter_map(|cell| cell.codepoint)
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
    let mut foreground = theme.resolve(cell.foreground, true);
    let mut background = theme.resolve(cell.background, false);
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

struct PaneState {
    generation: SessionGeneration,
    session: Box<dyn TerminalSession>,
    frame: TerminalFrame,
    interaction: InteractionState,
    title: String,
    measured_bounds: Arc<Mutex<(u32, u32)>>,
    viewport_bounds: Arc<Mutex<ViewportBounds>>,
    error: Option<String>,
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
struct SessionRegistry {
    sessions: HashMap<crate::workspace::PaneId, PaneState>,
    factory: Arc<dyn TerminalSessionFactory>,
    next_generation: u64,
    workspace: WorkspaceShell,
    notice: Option<String>,
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
                title: "Cuetty".into(),
                measured_bounds: Arc::new(Mutex::new((width, height))),
                viewport_bounds: Arc::new(Mutex::new((0.0, 0.0, width as f32, height as f32))),
                error: None,
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
                title: "Cuetty".into(),
                measured_bounds: Arc::new(Mutex::new((width, height))),
                viewport_bounds: Arc::new(Mutex::new((0.0, 0.0, width as f32, height as f32))),
                error: None,
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
            Some(state) if state.generation == generation => state.session.drain_effects(),
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
                ControlEvent::Close => should_close = true,
            }
        }
        if !should_close
            && let Some(state) = self.sessions.get_mut(&pane)
            && state.generation == generation
        {
            state.frame = state.session.frame();
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

struct TerminalView {
    registry: SessionRegistry,
    config: TerminalConfig,
    metrics: TerminalMetrics,
    theme: TerminalTheme,
    focus: FocusHandle,
    renderer: Box<dyn TerminalRenderer>,
    workspace_bounds: Arc<Mutex<(f32, f32)>>,
    rail_width: Arc<Mutex<f32>>,
    expanded_rail_width: Arc<Mutex<f32>>,
    rail_collapsed: bool,
    rail_layout_generation: u64,
    destination: Destination,
    settings_draft: Option<SettingsDraft>,
    settings_error: Option<String>,
}

const SIDEBAR_DEFAULT_WIDTH: f32 = 156.0;
const SIDEBAR_MIN_WIDTH: f32 = 50.0;
const SIDEBAR_MAX_WIDTH: f32 = 280.0;
const SIDEBAR_COMPACT_THRESHOLD: f32 = 84.0;
const SIDEBAR_FOOTER_HEIGHT: f32 = 42.0;
const SHELL_TITLEBAR_HEIGHT: f32 = 34.0;
const MAIN_MIN_WIDTH: f32 = 400.0;

fn rail_is_compact(width: f32) -> bool {
    width < SIDEBAR_COMPACT_THRESHOLD
}

fn normalized_rail_width(width: f32) -> f32 {
    width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
}

fn terminal_workspace_size(
    viewport: Size<Pixels>,
    rail_width: f32,
    rail_collapsed: bool,
) -> (f32, f32) {
    let rail = if rail_collapsed {
        SIDEBAR_MIN_WIDTH
    } else {
        normalized_rail_width(rail_width)
    };
    (
        (f32::from(viewport.width) - rail).max(1.0),
        (f32::from(viewport.height) - SHELL_TITLEBAR_HEIGHT).max(1.0),
    )
}

fn rail_collapsed_after_toggle(collapsed: bool, width: f32) -> bool {
    !collapsed && !rail_is_compact(width)
}

fn sidebar_tab_label(label: &str) -> String {
    let trimmed = label.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Destination {
    Terminal(TabId),
    Settings,
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
        cx.observe_window_bounds(window, move |_, _, cx| cx.notify())
            .detach();
        let config = TerminalConfig::default();
        let metrics = TerminalMetrics::from_config(cx.text_system(), &config);
        let theme = TerminalTheme::from_config(&config);
        let factory: Arc<dyn TerminalSessionFactory> = Arc::new(RioTerminalFactory);
        match SessionRegistry::start(factory.clone(), 720, 432, metrics.rio_cell()) {
            Ok((registry, wake_rx)) => {
                let pane = registry.active_pane().expect("initial workspace pane");
                let tab = registry
                    .workspace
                    .active_tab()
                    .expect("initial workspace tab");
                let mut view = Self {
                    registry,
                    config,
                    metrics,
                    theme,
                    focus: cx.focus_handle(),
                    renderer: Box::new(GridRenderer),
                    workspace_bounds: Arc::new(Mutex::new((720.0, 432.0))),
                    rail_width: Arc::new(Mutex::new(SIDEBAR_DEFAULT_WIDTH)),
                    expanded_rail_width: Arc::new(Mutex::new(SIDEBAR_DEFAULT_WIDTH)),
                    rail_collapsed: false,
                    rail_layout_generation: 0,
                    destination: Destination::Terminal(tab),
                    settings_draft: None,
                    settings_error: None,
                };
                view.spawn_wake(cx, pane, SessionGeneration(1), wake_rx);
                view
            }
            Err(error) => {
                let mut registry = SessionRegistry::empty(factory);
                registry.notice = Some(format!("Cuetty could not start Rio: {error}"));
                Self {
                    registry,
                    config,
                    metrics,
                    theme,
                    focus: cx.focus_handle(),
                    renderer: Box::new(GridRenderer),
                    workspace_bounds: Arc::new(Mutex::new((720.0, 432.0))),
                    rail_width: Arc::new(Mutex::new(SIDEBAR_DEFAULT_WIDTH)),
                    expanded_rail_width: Arc::new(Mutex::new(SIDEBAR_DEFAULT_WIDTH)),
                    rail_collapsed: false,
                    rail_layout_generation: 0,
                    destination: Destination::Settings,
                    settings_draft: None,
                    settings_error: None,
                }
            }
        }
    }
    fn active_pane(&self) -> Option<crate::workspace::PaneId> {
        self.registry.active_pane()
    }
    fn active_state(&self) -> Option<&PaneState> {
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
    fn select_terminal_tab(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        match self.registry.workspace.apply(ShellAction::ActivateTab(tab)) {
            Ok(_) => {
                self.destination = Destination::Terminal(tab);
                self.settings_draft = None;
                self.settings_error = None;
                self.focus.focus(window);
            }
            Err(error) => self.registry.notice = Some(shell_notice(error)),
        }
        cx.notify();
    }
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.destination, Destination::Settings) {
            self.settings_draft = Some(SettingsDraft::new(self.config.clone()));
            self.settings_error = None;
            self.destination = Destination::Settings;
        }
        window.blur();
        cx.notify();
    }
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_draft = None;
        self.settings_error = None;
        if let Some(tab) = self.active_tab() {
            self.destination = Destination::Terminal(tab);
            self.focus.focus(window);
        }
        cx.notify();
    }
    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.settings_draft.take() else {
            self.close_settings(window, cx);
            return;
        };
        match draft.apply() {
            Ok(config) => {
                self.metrics = TerminalMetrics::from_config(cx.text_system(), &config);
                self.theme = TerminalTheme::from_config(&config);
                self.config = config;
                self.settings_error = None;
                if let Some(tab) = self.active_tab() {
                    self.destination = Destination::Terminal(tab);
                    self.focus.focus(window);
                }
            }
            Err(error) => {
                self.settings_error = Some(error.to_string());
                self.settings_draft = Some(SettingsDraft::new(self.config.clone()));
            }
        }
        cx.notify();
    }
    fn reset_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(draft) = self.settings_draft.as_mut() {
            draft.reset_defaults();
            self.settings_error = None;
        }
        cx.notify();
    }
    fn adjust_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(draft) = self.settings_draft.as_mut() {
            let next = draft.config().font.size_px + delta;
            if let Err(error) = draft.set_font_size(next) {
                self.settings_error = Some(error.to_string());
            } else {
                self.settings_error = None;
            }
        }
        cx.notify();
    }
    fn adjust_line_height(&mut self, delta: f32, cx: &mut Context<Self>) {
        if let Some(draft) = self.settings_draft.as_mut() {
            let next = draft.config().font.line_height_multiplier + delta;
            if let Err(error) = draft.set_line_height_multiplier(next) {
                self.settings_error = Some(error.to_string());
            } else {
                self.settings_error = None;
            }
        }
        cx.notify();
    }
    fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        let width = *self.rail_width.lock().expect("rail width mutex poisoned");
        if !rail_collapsed_after_toggle(self.rail_collapsed, width) {
            self.rail_collapsed = false;
        } else {
            *self
                .expanded_rail_width
                .lock()
                .expect("expanded rail width mutex poisoned") = width;
            self.rail_collapsed = true;
        }
        self.rail_layout_generation = self.rail_layout_generation.wrapping_add(1);
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
        for text in outcome.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if outcome.should_close {
            self.close_pane(pane);
        }
        cx.notify();
    }
    fn close_pane(&mut self, pane: crate::workspace::PaneId) {
        let was_visible = self.active_pane() == Some(pane)
            && matches!(self.destination, Destination::Terminal(_));
        if let Err(error) = self.registry.close_pane(pane) {
            self.registry.notice = Some(format!("workspace action unavailable: {error}"));
        } else if was_visible {
            self.destination = self
                .active_tab()
                .map(Destination::Terminal)
                .unwrap_or(Destination::Settings);
            if matches!(self.destination, Destination::Settings) {
                self.settings_draft = None;
            }
        }
    }
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = 720;
        let height = 432;
        match self
            .registry
            .start_tab(width, height, self.metrics.rio_cell())
        {
            Ok((pane, generation, wake_rx)) => {
                self.spawn_wake(cx, pane, generation, wake_rx);
                if let Some(tab) = self.active_tab() {
                    self.destination = Destination::Terminal(tab);
                    self.settings_draft = None;
                    self.settings_error = None;
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

    fn workspace_shortcut(key: &str, modifiers: gpui::Modifiers) -> Option<WorkspaceShortcut> {
        if !modifiers.secondary() {
            return None;
        }
        match (modifiers.shift, key.to_ascii_lowercase().as_str()) {
            (_, "t") => Some(WorkspaceShortcut::NewTab),
            (_, "w") => Some(WorkspaceShortcut::CloseTab),
            (false, "d") => Some(WorkspaceShortcut::SplitHorizontal),
            (true, "d") => Some(WorkspaceShortcut::SplitVertical),
            (_, "]") => Some(WorkspaceShortcut::FocusNext),
            (_, "[") => Some(WorkspaceShortcut::FocusPrevious),
            _ => None,
        }
    }

    fn apply_workspace_action(&mut self, action: ShellAction) {
        match self.registry.workspace.apply(action) {
            Ok(_) => self.registry.notice = None,
            Err(error) => self.registry.notice = Some(shell_notice(error)),
        }
    }

    fn key(&mut self, event: &gpui::KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if let Some(shortcut) = Self::workspace_shortcut(key, modifiers) {
            match shortcut {
                WorkspaceShortcut::NewTab => self.new_tab(_window, cx),
                WorkspaceShortcut::CloseTab => {
                    if let Some(pane) = self.active_pane() {
                        self.close_pane(pane);
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
        if modifiers.secondary() && key.eq_ignore_ascii_case("f") {
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
        if modifiers.secondary() && key.eq_ignore_ascii_case("c") {
            if let Some(text) = self
                .active_state()
                .and_then(|state| state.interaction.copy(&state.frame))
            {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
        if modifiers.secondary() && key.eq_ignore_ascii_case("v") {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
                && let Some(state) = self.active_state_mut()
                && let Err(error) = state.session.paste(&text)
            {
                state.error = Some(error);
            }
            return;
        }
        let input = match key {
            "enter" => KeyInput::Enter,
            "tab" => KeyInput::Tab,
            "backspace" => KeyInput::Backspace,
            "escape" => KeyInput::Escape,
            "up" => KeyInput::Up,
            "down" => KeyInput::Down,
            "left" => KeyInput::Left,
            "right" => KeyInput::Right,
            "home" => KeyInput::Home,
            "end" => KeyInput::End,
            "delete" => KeyInput::Delete,
            _ => match event
                .keystroke
                .key_char
                .as_ref()
                .and_then(|s| s.chars().next())
            {
                Some(c) => KeyInput::Character(c),
                None => return,
            },
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
        }
    }

    fn mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.active_state_mut() {
            state.interaction.end_selection();
        }
        cx.notify();
    }

    fn render_sidebar(
        &self,
        active_tab: Option<TabId>,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let rail_width = *self.rail_width.lock().expect("rail width mutex poisoned");
        let compact = self.rail_collapsed || rail_is_compact(rail_width);
        let mut tabs = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(if compact { 2.0 } else { 4.0 }))
            .p(px(if compact { 5.0 } else { 8.0 }))
            .overflow_hidden();
        for (index, tab) in self
            .registry
            .workspace
            .workspace()
            .tabs()
            .iter()
            .enumerate()
        {
            let tab_id = tab.id;
            let selected = matches!(self.destination, Destination::Terminal(id) if id == tab_id)
                && active_tab == Some(tab_id);
            let label = self
                .registry
                .sessions
                .get(&tab.focused)
                .map(|state| state.title.clone())
                .unwrap_or_else(|| tab.title.clone());
            let display_label = sidebar_tab_label(&label);
            let number = format!("{:02}", index + 1);
            let tooltip_label = label.clone();
            let mut tab_button = div()
                .id(("cuetty-rail-tab", tab_id.get()))
                .w_full()
                .min_h(px(if compact { 34.0 } else { 40.0 }))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(if compact { 0.0 } else { 8.0 }))
                .px(px(if compact { 2.0 } else { 8.0 }))
                .border_l_2()
                .border_color(gpui::rgb(gpui_rgb(if selected {
                    theme.cursor
                } else {
                    theme.title_surface
                })))
                .when(selected, |this| this.bg(gpui::rgb(gpui_rgb(theme.surface))))
                .rounded(px(5.0))
                .hover(|this| this.bg(gpui::rgb(gpui_rgb(theme.surface))))
                .cursor_pointer()
                .on_click(cx.listener(move |view, _event, window, cx| {
                    view.select_terminal_tab(tab_id, window, cx);
                }))
                .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx));
            tab_button = tab_button.child(
                div()
                    .w(px(if compact { 40.0 } else { 24.0 }))
                    .text_center()
                    .text_size(px(if compact { 12.0 } else { 11.0 }))
                    .text_color(gpui::rgb(gpui_rgb(if selected {
                        theme.cursor
                    } else {
                        theme.title_text
                    })))
                    .child(number),
            );
            if !compact {
                tab_button = tab_button.child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(gpui::rgb(gpui_rgb(if selected {
                            theme.text
                        } else {
                            theme.title_text
                        })))
                        .child(display_label),
                );
            }
            tabs = tabs.child(tab_button);
        }
        let notice = shell_notice_text(self.registry.notice.as_deref());
        let mut rail = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .bg(gpui::rgb(gpui_rgb(theme.title_surface)))
            .border_r_1()
            .border_color(gpui::rgb(gpui_rgb(theme.host_background)))
            .child(tabs);
        if let Some(notice) = notice {
            rail = rail.child(
                div()
                    .px(px(12.0))
                    .py(px(6.0))
                    .text_size(px(10.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .child(notice),
            );
        }
        let footer_height = if compact {
            SIDEBAR_FOOTER_HEIGHT + 64.0
        } else {
            SIDEBAR_FOOTER_HEIGHT
        };
        let mut footer = div()
            .h(px(footer_height))
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(if compact { 6.0 } else { 8.0 }))
            .border_t_1()
            .border_color(gpui::rgb(gpui_rgb(theme.host_background)));
        let new_tab = div()
            .id("cuetty-new-tab")
            .flex_1()
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
            .text_size(px(12.0))
            .hover(|this| this.bg(gpui::rgb(gpui_rgb(theme.surface))))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("New tab").build(window, cx))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.new_tab(window, cx);
            }))
            .child(if compact { "+" } else { "+ New tab" });
        let settings = div()
            .id("cuetty-settings")
            .w(px(34.0))
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
            .text_size(px(15.0))
            .hover(|this| this.bg(gpui::rgb(gpui_rgb(theme.surface))))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Settings").build(window, cx))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.open_settings(window, cx);
            }))
            .child("⚙");
        let collapse = div()
            .id("cuetty-rail-collapse")
            .w(px(30.0))
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.0))
            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
            .hover(|this| this.bg(gpui::rgb(gpui_rgb(theme.surface))))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Collapse sidebar").build(window, cx))
            .on_click(cx.listener(|view, _event, _window, cx| view.toggle_rail(cx)))
            .child("‹");
        if compact {
            footer = footer.flex_col().child(new_tab).child(settings).child(
                div()
                    .id("cuetty-rail-expand")
                    .w_full()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .hover(|this| this.bg(gpui::rgb(gpui_rgb(theme.surface))))
                    .cursor_pointer()
                    .tooltip(|window, cx| Tooltip::new("Expand sidebar").build(window, cx))
                    .on_click(cx.listener(|view, _event, _window, cx| view.toggle_rail(cx)))
                    .child("›"),
            );
        } else {
            footer = footer
                .flex_row()
                .child(new_tab)
                .child(settings)
                .child(collapse);
        }
        let rail_width = Arc::clone(&self.rail_width);
        let expanded_rail_width = Arc::clone(&self.expanded_rail_width);
        let entity = cx.weak_entity();
        let rail_was_collapsed = self.rail_collapsed;
        rail.child(footer)
            .child(
                canvas(
                    move |bounds, _, app| {
                        let next = f32::from(bounds.size.width).max(SIDEBAR_MIN_WIDTH);
                        let mut changed = false;
                        {
                            let mut current = rail_width.lock().expect("rail width mutex poisoned");
                            if (*current - next).abs() > f32::EPSILON {
                                *current = next;
                                changed = true;
                            }
                        }
                        if !rail_was_collapsed && next >= SIDEBAR_COMPACT_THRESHOLD {
                            let mut expanded = expanded_rail_width
                                .lock()
                                .expect("expanded rail width mutex poisoned");
                            if (*expanded - next).abs() > f32::EPSILON {
                                *expanded = next;
                                changed = true;
                            }
                        }
                        if changed {
                            let _ = entity.update(app, |_view, cx| cx.notify());
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }

    fn render_settings(&self, theme: &TerminalTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let values = self
            .settings_draft
            .as_ref()
            .map(|draft| draft.config().clone())
            .unwrap_or_else(|| self.config.clone());
        let dirty = self
            .settings_draft
            .as_ref()
            .is_some_and(SettingsDraft::is_dirty);
        let font_size = values.font.size_px;
        let line_height = values.font.line_height_multiplier;
        let button = |id: &'static str, label: String| {
            div()
                .id(id)
                .min_w(px(30.0))
                .h(px(28.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .bg(gpui::rgb(gpui_rgb(theme.title_surface)))
                .text_color(gpui::rgb(gpui_rgb(theme.text)))
                .text_size(px(12.0))
                .hover(|this| this.bg(gpui::rgb(gpui_rgb(theme.host_background))))
                .cursor_pointer()
                .child(label)
        };
        let mut content = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(gpui::rgb(gpui_rgb(theme.surface)))
            .p(px(28.0))
            .gap(px(18.0))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(20.0))
                                    .text_color(gpui::rgb(gpui_rgb(theme.text)))
                                    .child("Settings"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                                    .child("Session-only terminal preferences"),
                            ),
                    )
                    .child(button("settings-close", "×".into()).on_click(
                        cx.listener(|view, _event, window, cx| view.close_settings(window, cx)),
                    )),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                            .child("Appearance"),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_color(gpui::rgb(gpui_rgb(theme.text)))
                                            .child("Font size"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                                            .child("Applied to every terminal tab"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .child(button("settings-font-minus", "−".into()).on_click(
                                        cx.listener(|view, _event, _window, cx| {
                                            view.adjust_font_size(-1.0, cx)
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .w(px(52.0))
                                            .text_center()
                                            .text_color(gpui::rgb(gpui_rgb(theme.text)))
                                            .child(format!("{font_size:.0} px")),
                                    )
                                    .child(button("settings-font-plus", "+".into()).on_click(
                                        cx.listener(|view, _event, _window, cx| {
                                            view.adjust_font_size(1.0, cx)
                                        }),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_color(gpui::rgb(gpui_rgb(theme.text)))
                                            .child("Line height"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                                            .child("Controls vertical terminal rhythm"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .child(button("settings-line-minus", "−".into()).on_click(
                                        cx.listener(|view, _event, _window, cx| {
                                            view.adjust_line_height(-0.05, cx)
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .w(px(52.0))
                                            .text_center()
                                            .text_color(gpui::rgb(gpui_rgb(theme.text)))
                                            .child(format!("{line_height:.2}×")),
                                    )
                                    .child(button("settings-line-plus", "+".into()).on_click(
                                        cx.listener(|view, _event, _window, cx| {
                                            view.adjust_line_height(0.05, cx)
                                        }),
                                    )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .text_size(px(12.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .child("Settings are kept in memory for this run and are not persisted yet."),
            );
        if let Some(error) = &self.settings_error {
            content = content.child(
                div()
                    .text_size(px(11.0))
                    .text_color(gpui::rgb(gpui_rgb(Rgb(230, 110, 100))))
                    .child(error.clone()),
            );
        }
        content
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                            .child(if dirty {
                                "Unsaved changes"
                            } else {
                                "No changes"
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(button("settings-reset", "Reset defaults".into()).on_click(
                                cx.listener(|view, _event, _window, cx| view.reset_settings(cx)),
                            ))
                            .child(button("settings-cancel", "Cancel".into()).on_click(
                                cx.listener(|view, _event, window, cx| {
                                    view.close_settings(window, cx)
                                }),
                            ))
                            .child(
                                button("settings-apply", "Apply".into()).on_click(cx.listener(
                                    |view, _event, window, cx| view.apply_settings(window, cx),
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_shell_layout(
        &self,
        sidebar: gpui::AnyElement,
        content: gpui::AnyElement,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let expanded_width = normalized_rail_width(
            *self
                .expanded_rail_width
                .lock()
                .expect("expanded rail width mutex poisoned"),
        );
        let rail_size = if self.rail_collapsed {
            SIDEBAR_MIN_WIDTH
        } else {
            expanded_width
        };
        let layout_id = ("cuetty-main-layout", self.rail_layout_generation);
        let entity = cx.weak_entity();
        let workspace = div().flex_1().child(
            h_resizable(layout_id)
                .on_resize(move |_, _, app| {
                    let _ = entity.update(app, |_view, cx| cx.notify());
                })
                .child(
                    resizable_panel()
                        .size(px(rail_size))
                        .size_range(px(SIDEBAR_MIN_WIDTH)..px(SIDEBAR_MAX_WIDTH))
                        .child(sidebar),
                )
                .child(
                    resizable_panel()
                        .size_range(px(MAIN_MIN_WIDTH)..Pixels::MAX)
                        .child(content),
                ),
        );
        let titlebar = div()
            .h(px(SHELL_TITLEBAR_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .bg(gpui::rgb(gpui_rgb(theme.title_surface)))
            .border_b_1()
            .border_color(gpui::rgb(gpui_rgb(theme.host_background)))
            .child(
                div()
                    .h_full()
                    .w(px(82.0))
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .h_full()
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .text_size(px(13.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.text)))
                    .child("Cuetty"),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(titlebar)
            .child(workspace)
            .into_any_element()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        if self.registry.close_window_requested {
            window.remove_window();
        }
        let theme = self.theme.clone();
        let metrics = self.metrics.clone();
        let active_tab = self.active_tab();
        let sidebar = self.render_sidebar(active_tab, &theme, cx);
        if matches!(self.destination, Destination::Settings) {
            return self.render_shell_layout(sidebar, self.render_settings(&theme, cx), &theme, cx);
        }
        let Some(active_pane) = self.active_pane() else {
            let message = self
                .registry
                .notice
                .clone()
                .unwrap_or_else(|| "Cuetty has no live terminal session".into());
            return self.render_shell_layout(
                sidebar,
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgb(gpui_rgb(theme.surface)))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .child(message)
                    .into_any_element(),
                &theme,
                cx,
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
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgb(gpui_rgb(theme.surface)))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .child(format!("Cuetty terminal error: {error}"))
                    .into_any_element(),
                &theme,
                cx,
            );
        }
        let workspace_bounds = Arc::clone(&self.workspace_bounds);
        let workspace_entity = cx.weak_entity();
        let rail_width = *self.rail_width.lock().expect("rail width mutex poisoned");
        let next_workspace =
            terminal_workspace_size(window.viewport_size(), rail_width, self.rail_collapsed);
        {
            let mut current = self
                .workspace_bounds
                .lock()
                .expect("workspace bounds mutex poisoned");
            *current = next_workspace;
        }
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
        let active_bounds = layouts
            .iter()
            .find(|pane| pane.pane_id == active_pane)
            .map(|pane| pane.bounds);
        if let Some(state) = self.registry.sessions.get_mut(&active_pane) {
            let bounds = active_bounds
                .map(|bounds| (bounds.width.max(1.0) as u32, bounds.height.max(1.0) as u32))
                .unwrap_or_else(|| *state.measured_bounds.lock().expect("bounds mutex poisoned"));
            *state.measured_bounds.lock().expect("bounds mutex poisoned") = bounds;
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
        let mut surface = div()
            .relative()
            .flex_1()
            .overflow_hidden()
            .bg(gpui::rgb(gpui_rgb(theme.surface)))
            .child(
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
                        .bg(gpui::rgb(gpui_rgb(theme.surface)))
                        .track_focus(&self.focus)
                        .on_key_down(cx.listener(Self::key))
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                        .on_mouse_move(cx.listener(Self::mouse_move))
                        .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
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
                surface = surface.child(viewport);
            }
        }
        self.render_shell_layout(sidebar, surface.into_any_element(), &theme, cx)
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
        cx.open_window(
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
                    traffic_light_position: Some(point(px(12.0), px(9.0))),
                }),
                ..WindowOptions::default()
            },
            |window, cx| {
                let view = cx.new(|cx| TerminalView::new(window, cx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        )
        .expect("open Cuetty window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
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

        fn frame(&mut self) -> TerminalFrame {
            self.frame.clone()
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
    fn shell_notice_keeps_live_action_failures_visible() {
        assert_eq!(
            shell_notice_text(Some(
                "workspace action unavailable: split panes are unavailable"
            )),
            Some("• workspace action unavailable: split panes are unavailable".into())
        );
        assert_eq!(shell_notice_text(None), None);
    }

    #[test]
    fn rail_width_supports_a_compact_number_only_mode() {
        assert!(rail_is_compact(SIDEBAR_MIN_WIDTH));
        assert!(rail_is_compact(SIDEBAR_COMPACT_THRESHOLD - 0.1));
        assert!(!rail_is_compact(SIDEBAR_COMPACT_THRESHOLD));
        assert_eq!(normalized_rail_width(12.0), SIDEBAR_MIN_WIDTH);
        assert_eq!(normalized_rail_width(320.0), SIDEBAR_MAX_WIDTH);
    }

    #[test]
    fn rail_toggle_restores_after_manual_drag_to_compact_width() {
        assert!(!rail_collapsed_after_toggle(false, SIDEBAR_MIN_WIDTH));
        assert!(!rail_collapsed_after_toggle(true, SIDEBAR_MIN_WIDTH));
        assert!(rail_collapsed_after_toggle(false, SIDEBAR_DEFAULT_WIDTH));
    }

    #[test]
    fn terminal_workspace_tracks_window_and_rail_geometry() {
        assert_eq!(
            terminal_workspace_size(size(px(960.0), px(640.0)), 156.0, false),
            (804.0, 606.0)
        );
        assert_eq!(
            terminal_workspace_size(size(px(960.0), px(640.0)), 156.0, true),
            (910.0, 606.0)
        );
    }

    #[test]
    fn sidebar_labels_use_a_stable_single_line_name() {
        assert_eq!(
            sidebar_tab_label("~/Code/src/github.com/cuenv/cuenv"),
            "cuenv"
        );
        assert_eq!(sidebar_tab_label("/"), "/");
        assert_eq!(sidebar_tab_label("build"), "build");
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
        let shaped: String = cells.iter().filter_map(|cell| cell.codepoint).collect();
        assert_eq!(shaped, "界x");
        assert!(cells.iter().any(|cell| cell.codepoint.is_none()));
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
            registry.sessions[&first_pane].frame.rows[0].cells[0].codepoint,
            Some('a')
        );
        assert_eq!(
            registry.sessions[&second_pane].frame.rows[0].cells[0].codepoint,
            Some('b')
        );
    }
}
