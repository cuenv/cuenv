use super::chrome::{
    ChromeAssets, SessionProblem, ShellPanels, UNFOCUSED_TERMINAL_FADE, exit_notice_height,
    render_exit_notice, resolve_ui_font, sidebar_width, traffic_light_position,
};
use super::color::{blend, dimmed, with_minimum_contrast};
use super::config::TerminalConfig;
use super::cursor::{
    CursorAnimator, CursorFocus, CursorFrame, CursorObservation, TrailSpan, blink_wanted,
};
use super::events::ControlEvent;
use super::git_branch::BranchProbe;
use super::glyphs::{GlyphShape, drawn_glyph, is_drawn, is_graphic};
use super::host::{
    RioTerminalFactory, SessionRequest, SessionStart, TerminalScroll, TerminalSession,
    TerminalSessionFactory, TerminalWheel,
};
use super::input::{InputModifiers, KeyInput, TerminalKeyEvent};
use super::interaction::{CellHitTest, InteractionState, SearchKey};
use super::model::{
    CellOccupancy, CursorShape, Known, Rgb, TerminalCell, TerminalFrame, UnderlineStyle,
};
#[cfg(test)]
use super::model::{CursorState, SamplingToken, TerminalDimensions, TerminalRow};
use super::motion::{
    ChromeMotion, HoverTarget, MotionPace, SidebarButton, SidebarFrame, TabPlaces,
};
use super::overlay::{CellOverlaySpan, OverlayKind, TerminalOverlays};
use super::presentation::{DeviceGrid, ScaleFactor, TerminalMetrics, TerminalTheme, gpui_rgb};
use super::sizing::CellSize;
use super::tab_label::{PaneActivity, TabLabelSources, resolve_tab_label};
use super::tab_rename::{RenameKey, TabRename, rename_key};
use super::tab_status::{SessionExit, StatusEvent, TabPresence, TabStatus, WindowPresence};
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
    Element, FocusHandle, Font, FontStyle, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyBinding, Menu, MenuItem, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathBuilder, Pixels, Render, ScrollDelta, ScrollHandle,
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
    focus: CursorFocus,
    window: WindowPresence,
    cursor: CursorFrame,
    overlays: TerminalOverlays,
}

trait TerminalRenderer: Send + Sync {
    fn render(&self, request: RenderRequest<'_>) -> gpui::AnyElement;
}

struct GridRenderer;
impl TerminalRenderer for GridRenderer {
    fn render(&self, request: RenderRequest<'_>) -> gpui::AnyElement {
        TerminalGrid::new(request).into_any_element()
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
    focus: CursorFocus,
    window: WindowPresence,
    cursor_opacity: f32,
    cursor_trail: Option<TrailSpan>,
    overlays: TerminalOverlays,
}

impl TerminalGrid {
    fn new(request: RenderRequest<'_>) -> Self {
        Self {
            frame: request.frame.clone(),
            metrics: request.metrics.clone(),
            theme: request.theme.clone(),
            focus: request.focus,
            window: request.window,
            cursor_opacity: request.cursor.opacity,
            cursor_trail: request.cursor.trail,
            overlays: request.overlays,
        }
    }

    /// What a colour is painted as, faded here rather than with an element's opacity:
    /// GPUI applies that to each primitive on its own, so a drawn glyph's overlapping
    /// strokes would blend twice and bead every box-drawing rule.
    fn ink(&self, color: Rgb) -> gpui::Rgba {
        gpui::rgb(gpui_rgb(window_ink(color, self.theme.surface, self.window)))
    }

    fn overlay_color(&self, kind: OverlayKind) -> Rgb {
        match kind {
            OverlayKind::Selection => self.theme.selection,
            OverlayKind::SearchMatch => self.theme.search_match,
            OverlayKind::CurrentSearchMatch => self.theme.current_search_match,
        }
    }

    fn paint_row_overlays(&self, row: &RowPaint<'_>, window: &mut Window) {
        for span in row.overlays.iter().filter(|span| span.row == row.row) {
            window.paint_quad(quad(
                self.cell_bounds(row.origin, span.row, span.start_column..span.end_column),
                px(0.0),
                self.ink(self.overlay_color(span.kind)),
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            ));
        }
    }

    fn layer_at(&self, overlays: &[CellOverlaySpan], position: CellPosition) -> CellLayer {
        cell_layer(
            cursor_style_at(&self.frame, self.focus, position),
            overlays,
            position,
        )
    }

    /// The colour a cell's glyph is painted in.
    fn foreground_at(&self, cell: &TerminalCell, row: &RowPaint<'_>, column: usize) -> Rgb {
        let position = CellPosition {
            row: row.row,
            column,
        };
        let tint = quiet_tint_at(row.overlays, position, &self.theme);
        let layer = self.layer_at(row.overlays, position);
        if layer != CellLayer::BlockCursor {
            return readable_foreground(PaintedCell { cell, layer, tint }, &self.theme);
        }
        // A block cursor fading out hands its glyph back to what is underneath it.
        let beneath = cell_layer(None, row.overlays, position);
        blend(
            readable_foreground(
                PaintedCell {
                    cell,
                    layer: beneath,
                    tint,
                },
                &self.theme,
            ),
            self.theme.cursor_text,
            self.cursor_opacity,
        )
    }

    /// Paints a row's glyphs. Adjacent cells are shaped together wherever they share a
    /// font, even across colours, so a font's contextual alternates read the text
    /// around them and do not change as the cursor or syntax colouring moves past.
    fn paint_row_text(&self, row: &RowPaint<'_>, window: &mut Window) {
        let cells = row.cells;
        let mut start = 0;
        while start < cells.len() {
            let cell = &cells[start];
            // Spacers carry background and occupancy only.  In particular,
            // they are never shaped as a space glyph. Concealed text has no glyph
            // either: painting it in its background would show through a translucent
            // terminal as an opaque shape.
            if cell.text.is_none() || cell.style.hidden {
                start += 1;
                continue;
            }
            if let Some(character) = drawn_character(cell) {
                self.paint_drawn_glyph(
                    row.origin,
                    DrawnGlyph {
                        character,
                        row: row.row,
                        column: start,
                        color: self.foreground_at(cell, row, start),
                    },
                    window,
                );
                start += 1;
                continue;
            }
            let end = (start + 1..cells.len())
                .find(|&column| {
                    let next = &cells[column];
                    next.text.is_none()
                        || next.style.hidden
                        || drawn_character(next).is_some()
                        || next.style.bold != cell.style.bold
                        || next.style.italic != cell.style.italic
                })
                .unwrap_or(cells.len());
            self.paint_text_run(row, start..end, window);
            start = end;
        }
    }

    /// Paints one shaped run with every glyph placed from its cell's column.
    ///
    /// This does not paint a `ShapedLine` with a forced width because that cannot hold
    /// the grid: GPUI only corrects a glyph more than a pixel off, so spacing wanders
    /// wherever the advance and the cell differ, and it counts glyphs rather than
    /// characters, so a combining mark or a ligature in `calt` would shift every later
    /// column in the run. Mapping each glyph back to its source text cannot.
    fn paint_text_run(
        &self,
        row: &RowPaint<'_>,
        columns: std::ops::Range<usize>,
        window: &mut Window,
    ) {
        let cells = &row.cells[columns.clone()];
        let mut text = String::new();
        let cluster_starts: Vec<usize> = cells
            .iter()
            .map(|cell| {
                let start = text.len();
                text.push_str(cell.text.as_deref().unwrap_or_default());
                start
            })
            .collect();
        let foregrounds: Vec<Hsla> = cells
            .iter()
            .zip(columns.clone())
            .map(|(cell, column)| self.ink(self.foreground_at(cell, row, column)).into())
            .collect();
        let Some(first) = cells.first() else {
            return;
        };
        let run = TextRun {
            len: text.len(),
            font: cell_font(first, &self.metrics),
            // Colours are per glyph below; this only satisfies the shaper.
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window.text_system().shape_line(
            SharedString::from(text),
            self.metrics.font_size,
            &[run],
            None,
        );
        let grid = DeviceGrid::new(row.origin, &self.metrics);
        let baseline = grid.glyph_baseline(row.row, self.baseline(window));
        let font_size = self.metrics.font_size;
        let line = self.cell_bounds(row.origin, row.row, columns.clone());
        window.paint_layer(line, |window| {
            let mut placer = GlyphPlacer::new(&cluster_starts);
            for run in &shaped.runs {
                for glyph in &run.glyphs {
                    let placed = placer.place(glyph.index, f32::from(glyph.position.x));
                    let x = grid.glyph_x(columns.start + placed.cluster, px(placed.offset));
                    let origin = point(x, baseline);
                    // A glyph that cannot be rasterized leaves its cell empty.
                    let _ = if glyph.is_emoji {
                        window.paint_emoji(origin, run.font_id, glyph.id, font_size)
                    } else {
                        let color = foregrounds.get(placed.cluster).copied().unwrap_or_default();
                        window.paint_glyph(origin, run.font_id, glyph.id, font_size, color)
                    };
                }
            }
        });
    }

    /// The distance in device pixels from the top of a cell to the text's baseline, from
    /// the configured font alone. A fallback glyph therefore sits on the same line as the
    /// text around it, and every row's baseline lands on a device pixel.
    fn baseline(&self, window: &Window) -> i32 {
        let text_system = window.text_system();
        let font = text_system.resolve_font(&self.metrics.font);
        let offset = baseline_in_cell(
            self.metrics.render_cell().height,
            text_system.ascent(font, self.metrics.font_size),
            text_system.descent(font, self.metrics.font_size),
        );
        self.metrics.scale.nearest_device_pixel(offset)
    }

    /// Underlines and strikethroughs follow the cells they decorate, so they are drawn
    /// once a row's runs are placed and do not depend on how it was shaped.
    fn paint_row_decorations(&self, row: &RowPaint<'_>, window: &mut Window) {
        let cells = row.cells;
        if cells.iter().all(|cell| {
            cell.style.hidden || (cell.style.underline.is_none() && !cell.style.strikeout)
        }) {
            return;
        }
        let grid_cell = self.metrics.render_cell();
        let text_system = window.text_system();
        let font = text_system.resolve_font(&self.metrics.font);
        let ascent = text_system.ascent(font, self.metrics.font_size);
        let descent = text_system.descent(font, self.metrics.font_size).abs();
        let baseline = self
            .metrics
            .scale
            .logical_position(self.baseline(window) as f32);
        let top = row.origin.y + grid_cell.height * row.row as f32;
        let x = |column: usize| row.origin.x + grid_cell.width * column as f32;
        let underlines = merged_spans(cells.iter().enumerate().map(|(column, cell)| {
            let foreground = self.foreground_at(cell, row, column);
            let style = cell.style.underline.filter(|_| !cell.style.hidden);
            style.map(|style| {
                let color = cell
                    .underline_color
                    .map_or(foreground, |color| self.theme.resolve(color));
                (style, color)
            })
        }));
        for (columns, (style, color)) in underlines {
            window.paint_underline(
                point(x(columns.start), top + baseline + descent * 0.618),
                grid_cell.width * columns.len() as f32,
                &GpuiUnderlineStyle {
                    thickness: px(if matches!(style, UnderlineStyle::Double) {
                        2.0
                    } else {
                        1.0
                    }),
                    color: Some(self.ink(color).into()),
                    wavy: matches!(style, UnderlineStyle::Curly),
                },
            );
        }
        let strikethroughs = merged_spans(cells.iter().enumerate().map(|(column, cell)| {
            (cell.style.strikeout && !cell.style.hidden)
                .then(|| self.foreground_at(cell, row, column))
        }));
        for (columns, color) in strikethroughs {
            window.paint_strikethrough(
                point(x(columns.start), top + (ascent * 0.5 + baseline) * 0.5),
                grid_cell.width * columns.len() as f32,
                &StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(self.ink(color).into()),
                },
            );
        }
    }

    fn cell_bounds(
        &self,
        origin: gpui::Point<Pixels>,
        row: usize,
        columns: std::ops::Range<usize>,
    ) -> Bounds<Pixels> {
        let grid_cell = self.metrics.render_cell();
        Bounds {
            origin: point(
                origin.x + grid_cell.width * columns.start as f32,
                origin.y + grid_cell.height * row as f32,
            ),
            size: Size {
                width: grid_cell.width * columns.len() as f32,
                height: grid_cell.height,
            },
        }
    }

    /// Paints a shape-drawn character in its cell, in device pixels, so it meets its
    /// neighbours exactly. Its colour is worked out like any glyph's, so selections,
    /// search matches, and a block cursor treat it the same as text.
    fn paint_drawn_glyph(
        &self,
        origin: gpui::Point<Pixels>,
        glyph: DrawnGlyph,
        window: &mut Window,
    ) {
        let Some(shapes) = drawn_glyph(glyph.character, self.metrics.device_cell()) else {
            return;
        };
        let cell = self.cell_bounds(origin, glyph.row, glyph.column..glyph.column + 1);
        let scale = self.metrics.scale;
        let solid = self.ink(glyph.color);
        let fill = |bounds, color: gpui::Rgba| {
            quad(
                bounds,
                px(0.0),
                color,
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            )
        };
        for shape in shapes {
            match shape {
                GlyphShape::Rect(rect) => window.paint_quad(fill(
                    Bounds {
                        origin: point(
                            cell.origin.x + scale.logical(rect.x),
                            cell.origin.y + scale.logical(rect.y),
                        ),
                        size: size(scale.logical(rect.width), scale.logical(rect.height)),
                    },
                    solid,
                )),
                GlyphShape::Shade(shade) => window.paint_quad(fill(
                    cell,
                    gpui::Rgba {
                        a: shade.opacity(),
                        ..solid
                    },
                )),
                GlyphShape::Polygon(outline) => {
                    let mut corners = outline.into_iter().map(|(x, y)| {
                        point(
                            cell.origin.x + scale.logical_position(x),
                            cell.origin.y + scale.logical_position(y),
                        )
                    });
                    let Some(first) = corners.next() else {
                        continue;
                    };
                    let mut path = PathBuilder::fill();
                    path.move_to(first);
                    corners.for_each(|corner| path.line_to(corner));
                    path.close();
                    // A degenerate outline has nothing to fill.
                    if let Ok(path) = path.build() {
                        window.paint_path(path, solid);
                    }
                }
            }
        }
    }

    /// Painted over the row's text but faint enough to read through, like the cursor's
    /// wake rather than a second cursor.
    fn paint_cursor_trail(&self, origin: gpui::Point<Pixels>, window: &mut Window) {
        let Some(trail) = self.cursor_trail else {
            return;
        };
        let cell = self.metrics.render_cell();
        window.paint_quad(quad(
            Bounds {
                origin: point(
                    origin.x + cell.width * trail.start,
                    origin.y + cell.height * f32::from(trail.row),
                ),
                size: size(cell.width * (trail.end - trail.start), cell.height),
            },
            px(0.0),
            gpui::Rgba {
                a: trail.opacity,
                ..self.ink(self.theme.cursor)
            },
            Edges::default(),
            Hsla::transparent_black(),
            BorderStyle::default(),
        ));
    }

    fn paint_cursor(&self, row: &RowPaint<'_>, layer: CursorLayer, window: &mut Window) {
        let Some((_, column)) = self.frame.cursor.position else {
            return;
        };
        let column = usize::from(column);
        let position = CellPosition {
            row: row.row,
            column,
        };
        let Some(style) = cursor_style_at(&self.frame, self.focus, position) else {
            return;
        };
        if column >= usize::from(self.frame.dimensions.columns)
            || style.layer() != layer
            || self.cursor_opacity <= 0.0
        {
            return;
        }
        let bounds = self.cell_bounds(row.origin, row.row, cursor_columns(row.cells, column));
        let scale = self.metrics.scale;
        let color = gpui::Rgba {
            a: self.cursor_opacity,
            ..self.ink(self.theme.cursor)
        };
        let line = scale.logical(scale.whole_device_pixels(px(2.0)));
        let fill = |bounds| {
            quad(
                bounds,
                px(0.0),
                color,
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            )
        };
        match style {
            HostCursorStyle::Block => window.paint_quad(fill(bounds)),
            HostCursorStyle::Underline => window.paint_quad(fill(Bounds {
                origin: point(bounds.origin.x, bounds.bottom() - line),
                size: size(bounds.size.width, line),
            })),
            HostCursorStyle::Bar => window.paint_quad(fill(Bounds {
                origin: bounds.origin,
                size: size(line, bounds.size.height),
            })),
            // A device pixel of outline stays crisp and lets the glyph keep its colours.
            HostCursorStyle::Hollow => window.paint_quad(quad(
                bounds,
                px(0.0),
                Hsla::transparent_black(),
                Edges::all(scale.logical(1)),
                color,
                BorderStyle::Solid,
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
        _: &mut App,
    ) {
        let overlays = self.overlays.spans(&self.frame);
        // Cells are whole device pixels, so a whole-pixel origin puts every edge on one.
        let bounds = Bounds {
            origin: self.metrics.scale.snap(bounds.origin),
            ..bounds
        };
        for row in 0..self.frame.dimensions.rows as usize {
            let cells = frame_row(&self.frame, row);
            if cells.is_empty() {
                break;
            }
            let row_paint = RowPaint {
                origin: bounds.origin,
                row,
                cells,
                overlays: &overlays,
            };
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
                        self.cell_bounds(bounds.origin, row, start..end),
                        px(0.0),
                        self.ink(background),
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
            self.paint_row_overlays(&row_paint, window);
            // A block cursor is deliberately below text; line cursors are
            // painted after glyphs so they remain visible.
            self.paint_cursor(&row_paint, CursorLayer::BelowText, window);
            self.paint_row_text(&row_paint, window);
            self.paint_row_decorations(&row_paint, window);
            self.paint_cursor(&row_paint, CursorLayer::AboveText, window);
        }
        self.paint_cursor_trail(bounds.origin, window);
    }
}

/// One row of the grid while it is being painted.
struct RowPaint<'a> {
    origin: gpui::Point<Pixels>,
    row: usize,
    cells: &'a [TerminalCell],
    overlays: &'a [CellOverlaySpan],
}

/// The font a cell's text is shaped in.
fn cell_font(cell: &TerminalCell, metrics: &TerminalMetrics) -> Font {
    Font {
        family: metrics.font.family.clone(),
        features: metrics.font.features.clone(),
        fallbacks: metrics.font.fallbacks.clone(),
        weight: if cell.style.bold {
            metrics.bold_weight
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

/// Centres a font's line in a cell. Platforms report the descent as negative, which is
/// also why this does not use GPUI's `baseline_offset`: that adds it to the ascent
/// instead of subtracting, and so sits text below the centre of the line.
fn baseline_in_cell(height: Pixels, ascent: Pixels, descent: Pixels) -> Pixels {
    (height - ascent - descent.abs()) / 2.0 + ascent
}

/// Where a shaped glyph belongs: the cell its source text came from, and how far past
/// that cell's left edge it was shaped.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlacedGlyph {
    cluster: usize,
    offset: f32,
}

/// Maps a shaped run's glyphs back onto cells from the text each came from. A cell's
/// first glyph starts exactly on it, and any further glyphs in that cell, such as
/// combining marks, keep their distance from the first. A font's own advances never
/// move a cell, and a glyph that replaced several cells leaves the next one alone.
struct GlyphPlacer<'a> {
    cluster_starts: &'a [usize],
    current: Option<(usize, f32)>,
}

impl<'a> GlyphPlacer<'a> {
    fn new(cluster_starts: &'a [usize]) -> Self {
        Self {
            cluster_starts,
            current: None,
        }
    }

    fn place(&mut self, text_index: usize, shaped_x: f32) -> PlacedGlyph {
        let cluster = self
            .cluster_starts
            .partition_point(|start| *start <= text_index)
            .saturating_sub(1);
        let first_x = match self.current {
            Some((current, first_x)) if current == cluster => first_x,
            _ => {
                self.current = Some((cluster, shaped_x));
                shaped_x
            }
        };
        PlacedGlyph {
            cluster,
            offset: shaped_x - first_x,
        }
    }
}

/// Runs of equal values, with gaps where there is no value at all.
fn merged_spans<T: PartialEq>(
    values: impl IntoIterator<Item = Option<T>>,
) -> Vec<(std::ops::Range<usize>, T)> {
    let mut spans: Vec<(std::ops::Range<usize>, T)> = Vec::new();
    for (column, value) in values.into_iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        match spans.last_mut() {
            Some((columns, last)) if columns.end == column && *last == value => {
                columns.end = column + 1;
            }
            _ => spans.push((column..column + 1, value)),
        }
    }
    spans
}

/// A character drawn as a shape, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DrawnGlyph {
    character: char,
    row: usize,
    column: usize,
    color: Rgb,
}

/// The character a cell draws as a shape rather than through the font. Only a lone
/// narrow character qualifies, since combining marks need shaping.
fn drawn_character(cell: &TerminalCell) -> Option<char> {
    (cell.occupancy == CellOccupancy::Narrow)
        .then_some(cell.text.as_deref())
        .flatten()
        .and_then(single_character)
        .filter(|character| is_drawn(*character))
}

/// Whether a cell holds a piece of a picture rather than text.
fn draws_a_graphic(cell: &TerminalCell) -> bool {
    cell.text
        .as_deref()
        .and_then(single_character)
        .is_some_and(is_graphic)
}

fn frame_row(frame: &TerminalFrame, row: usize) -> &[TerminalCell] {
    frame.rows.get(row).map_or(&[], |row| row.cells.as_slice())
}

/// While focused, an unknown shape renders as a visible block; `Known(false)`
/// suppresses the cursor and known shapes are rendered exactly. Unfocused, any cursor
/// is an outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCursorStyle {
    Block,
    Underline,
    Bar,
    /// An outline, whatever its shape: the cursor of a terminal keys are not going to.
    Hollow,
}

/// Which side of a row's text a cursor is painted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorLayer {
    BelowText,
    AboveText,
}

impl HostCursorStyle {
    /// A block sits beneath its glyph, which takes the cursor's text colour. Anything
    /// thinner would be covered by the glyph, so it goes on top.
    fn layer(self) -> CursorLayer {
        match self {
            Self::Block => CursorLayer::BelowText,
            Self::Underline | Self::Bar | Self::Hollow => CursorLayer::AboveText,
        }
    }
}

/// The columns a cursor at `column` covers: both halves of a wide character, whichever
/// half it landed on, because the whole glyph is painted in the cursor's colours.
fn cursor_columns(cells: &[TerminalCell], column: usize) -> std::ops::Range<usize> {
    let occupancy = |column: usize| cells.get(column).map(|cell| cell.occupancy);
    match occupancy(column) {
        Some(CellOccupancy::Wide) if occupancy(column + 1).is_some() => column..column + 2,
        Some(CellOccupancy::TrailingSpacer)
            if column > 0 && occupancy(column - 1) == Some(CellOccupancy::Wide) =>
        {
            column - 1..column + 1
        }
        _ => column..column + 1,
    }
}

fn cursor_style_at(
    frame: &TerminalFrame,
    focus: CursorFocus,
    position: CellPosition,
) -> Option<HostCursorStyle> {
    let covers = |(row, column): (u16, u16)| {
        let cells = frame
            .rows
            .get(usize::from(row))
            .map_or(&[][..], |row| row.cells.as_slice());
        usize::from(row) == position.row
            && cursor_columns(cells, usize::from(column)).contains(&position.column)
    };
    let shown = !matches!(frame.cursor.visible, Known::Known(false))
        && frame.cursor.position.is_some_and(covers);
    shown.then_some(match (focus, frame.cursor.shape) {
        (CursorFocus::Unfocused, _) => HostCursorStyle::Hollow,
        (_, Known::Known(CursorShape::Underline)) => HostCursorStyle::Underline,
        (_, Known::Known(CursorShape::Bar)) => HostCursorStyle::Bar,
        (_, Known::Known(CursorShape::Block) | Known::Unknown) => HostCursorStyle::Block,
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
/// selection or another search match is not a layer here: its quiet tint is chosen to
/// sit beneath any cell foreground, so glyphs keep their own colours on it.
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
    let (foreground, background) = resolved_colors(cell, theme);
    (styled_foreground(cell, foreground, background), background)
}

/// A cell's own colours, before anything fades its text.
fn resolved_colors(cell: &TerminalCell, theme: &TerminalTheme) -> (Rgb, Rgb) {
    let foreground = theme.resolve(cell.foreground);
    let background = theme.resolve(cell.background);
    if cell.style.inverse {
        (background, foreground)
    } else {
        (foreground, background)
    }
}

/// A cell's text colour once dim and concealed have faded it into what it is drawn on.
fn styled_foreground(cell: &TerminalCell, foreground: Rgb, drawn_on: Rgb) -> Rgb {
    if cell.style.hidden {
        drawn_on
    } else if cell.style.dim {
        dimmed(foreground, drawn_on)
    } else {
        foreground
    }
}

/// The quiet tint beneath a cell, if any: the last painted of a selection and a search
/// match, each chosen to sit under any foreground, so glyphs keep their colours on it.
fn quiet_tint_at(
    overlays: &[CellOverlaySpan],
    position: CellPosition,
    theme: &TerminalTheme,
) -> Option<Rgb> {
    overlays
        .iter()
        .rev()
        .filter(|span| {
            span.row == position.row
                && (span.start_column..span.end_column).contains(&position.column)
        })
        .find_map(|span| match span.kind {
            OverlayKind::Selection => Some(theme.selection),
            OverlayKind::SearchMatch => Some(theme.search_match),
            OverlayKind::CurrentSearchMatch => None,
        })
}

/// A cell, and what its glyph is being painted over.
#[derive(Debug, Clone, Copy)]
struct PaintedCell<'a> {
    cell: &'a TerminalCell,
    layer: CellLayer,
    tint: Option<Rgb>,
}

/// A glyph's colour against what is actually behind it, which is `tint` when one is
/// painted beneath the cell: dim text fades towards that, and the minimum contrast
/// setting measures against it. A layer above the cell has its own colour pair.
///
/// Colours an application spelled out in 24-bit are left alone, as are graphics: image
/// previews and Powerline segments are built from them, and only line up when a cell's
/// foreground matches its neighbour's background exactly.
fn readable_foreground(painted: PaintedCell<'_>, theme: &TerminalTheme) -> Rgb {
    let PaintedCell { cell, layer, tint } = painted;
    if layer != CellLayer::Cell {
        return colors(cell, theme, layer).0;
    }
    let (foreground, background) = resolved_colors(cell, theme);
    let drawn_on = tint.unwrap_or(background);
    let foreground = styled_foreground(cell, foreground, drawn_on);
    let explicit = if cell.style.inverse {
        cell.background
    } else {
        cell.foreground
    };
    if theme.minimum_contrast.is_off()
        || matches!(explicit, super::model::Color::Rgb(_))
        || draws_a_graphic(cell)
    {
        return foreground;
    }
    with_minimum_contrast(foreground, drawn_on, theme.minimum_contrast)
}

fn shell_notice(error: ShellError) -> String {
    format!("workspace action unavailable: {error}")
}

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
    /// What the tab's indicator reports.
    pub(super) status: TabStatus,
    /// The branch the pane's directory is on, for the tab's second line.
    pub(super) branch: BranchProbe,
    /// The terminal surface as laid out, in logical pixels. The grid is worked out from
    /// this at each draw's own scale, so no draw pairs one scale's surface with
    /// another's cell, however long ago the surface was measured.
    measured_bounds: Arc<Mutex<Size<Pixels>>>,
    /// Where the surface starts in window coordinates, for hit testing.
    viewport_origin: Arc<Mutex<(f32, f32)>>,
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
        surface: Size<Pixels>,
    ) -> Self {
        let frame = session.frame();
        Self {
            generation,
            session,
            frame,
            interaction: InteractionState::default(),
            scroll_remainder: 0.0,
            activity: PaneActivity::default(),
            status: TabStatus::default(),
            branch: BranchProbe::default(),
            measured_bounds: Arc::new(Mutex::new(surface)),
            viewport_origin: Arc::new(Mutex::new((0.0, 0.0))),
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
        let task_was_running = self.activity.task_name().is_some();
        self.activity.sample(process.as_ref(), now);
        if task_was_running && self.activity.task_name().is_none() {
            self.status.apply(StatusEvent::TaskEnded);
        }
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
            self.track_task(task.name.clone(), now);
        }
        self.activity.set_staged_hint(launch.hint(&task.name));
        self.activity
            .set_staged_task((launch == TaskLaunch::AwaitingArguments).then(|| task.name.clone()));
        Ok(launch)
    }

    /// Reports a task that has just been run in this pane's shell.
    fn track_task(&mut self, name: String, now: std::time::Instant) {
        self.activity.launch_task(name, now);
        if self.activity.task_name().is_some() {
            self.status.apply(StatusEvent::TaskStarted);
        }
    }

    /// Ends what was staged on the line a key acted on, returning the task that key runs.
    /// That has to be read before the key is sent, while the foreground is still the
    /// shell, and a foreground that has changed hands since has lost the staged task.
    fn finish_staged_line(&mut self, event: &TerminalKeyEvent) -> Option<String> {
        let submits = matches!(event.key, KeyInput::Enter);
        if submits && self.activity.has_staged_task() {
            self.sample_foreground(std::time::Instant::now());
        }
        self.activity.set_staged_hint(None);
        self.activity.take_staged_task().filter(|_| submits)
    }

    /// The surface a new tab opened from this one will get: what this one measured,
    /// with back the room for a failed shell's exit notice, which only this tab has.
    fn surface_for_sibling(&self) -> Size<Pixels> {
        let measured = *self
            .measured_bounds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        size(
            measured.width,
            measured.height + exit_notice_height(self.status.failure()),
        )
    }

    /// Sends a key the user pressed to the terminal.
    fn input(&mut self, event: TerminalKeyEvent) -> Result<bool, String> {
        let submitted_task = acts_on_the_line(&event)
            .then(|| self.finish_staged_line(&event))
            .flatten();
        self.status.apply(StatusEvent::Input);
        let sent = self.session.input(event)?;
        if let Some(task) = submitted_task {
            self.track_task(task, std::time::Instant::now());
        }
        Ok(sent)
    }

    /// Whether this pane's shell has gone and only its final output is left.
    fn has_exited(&self) -> bool {
        self.status.failure().is_some()
    }

    /// The tasks this pane can run. A shell that has gone or broken has nowhere to run
    /// them, so nothing should go on offering any.
    pub(super) fn task_catalogue(&self) -> &[CuenvTask] {
        if self.has_exited() || self.error.is_some() {
            &[]
        } else {
            &self.cuenv.tasks
        }
    }

    /// The chord that opens the palette here, where there are tasks for it to find. A
    /// chord with nothing to open is left to whatever else it means.
    fn palette_chord(&self) -> Option<&KeyChord> {
        (!self.task_catalogue().is_empty()).then_some(&self.cuenv.presentation.keybindings.palette)
    }

    /// Fits the grid to the surface. Only a live shell can be told, but an exited one's
    /// output still has to fit, or its last lines end up below the edge of the surface.
    fn fit_grid(&mut self, width: u32, height: u32, cell: CellSize) {
        if self.has_exited() {
            self.session.reflow_output(width, height, cell);
        } else if let Err(error) = self.session.resize(width, height, cell) {
            self.error = Some(error);
        }
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

/// A click on a tab's row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TabClick {
    pub(super) tab: TabId,
    /// How many clicks in quick succession this is.
    pub(super) click_count: usize,
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

/// Whether a later frame of the same terminal shows output the earlier one did not. A
/// view scrolled back stays on the same text as lines arrive, by moving further from the
/// live screen, so that move is output too. Text rewritten in place below such a view
/// cannot be told from here.
fn shows_new_output(earlier: &TerminalFrame, later: &TerminalFrame) -> bool {
    later.rows != earlier.rows || later.viewport_offset != earlier.viewport_offset
}

/// Whether a key finishes with the line being edited, by submitting or abandoning it.
fn acts_on_the_line(event: &TerminalKeyEvent) -> bool {
    match event.key {
        KeyInput::Enter => true,
        KeyInput::Character('c' | 'C') => event.modifiers.contains(InputModifiers::CONTROL),
        _ => false,
    }
}

/// Whether the user is in this window.
fn window_presence(window: &Window) -> WindowPresence {
    if window.is_window_active() {
        WindowPresence::Active
    } else {
        WindowPresence::Inactive
    }
}

/// The colour the grid paints `color` in, given where the user is.
fn window_ink(color: Rgb, background: Rgb, window: WindowPresence) -> Rgb {
    match window {
        WindowPresence::Active => color,
        WindowPresence::Inactive => blend(color, background, UNFOCUSED_TERMINAL_FADE),
    }
}

/// Whether a key dismisses a tab whose shell has exited. Enter is what the tab says to
/// press, and Ctrl-D is what a shell that exited with its last command's status was
/// most likely just given. It takes a fresh press: a key still held from before the
/// shell died was not meant for the tab, and would take the output it stayed to show.
fn closes_exited_tab(event: &gpui::KeyDownEvent) -> bool {
    let gpui::Modifiers {
        control,
        alt,
        shift,
        platform,
        function: _,
    } = event.keystroke.modifiers;
    if event.is_held {
        return false;
    }
    match event.keystroke.key.as_str() {
        "d" => control && !(alt || shift || platform),
        _ => takes_offered_action(event),
    }
}

/// Whether a key takes the action a tab with no working terminal is offering: a fresh
/// press of a plain Enter, since one held from before the failure was not meant for it.
fn takes_offered_action(event: &gpui::KeyDownEvent) -> bool {
    let gpui::Modifiers {
        control,
        alt,
        shift,
        platform,
        function: _,
    } = event.keystroke.modifiers;
    !event.is_held && event.keystroke.key == "enter" && !(control || alt || shift || platform)
}

/// What holding Alt means on this platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AltKey {
    /// It is Option, which composes characters that are text.
    Composes,
    /// It makes a chord, though the keystroke still carries the letter.
    Chords,
}

impl AltKey {
    const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Composes
        } else {
            Self::Chords
        }
    }
}

/// The character a keystroke types as text, if it does. A chord is not text, whatever
/// character it would have typed.
fn typed_character(keystroke: &gpui::Keystroke, alt_key: AltKey) -> Option<char> {
    let gpui::Modifiers {
        control,
        alt,
        platform,
        ..
    } = keystroke.modifiers;
    if control || platform || (alt && alt_key == AltKey::Chords) {
        return None;
    }
    keystroke
        .key_char
        .as_deref()
        .and_then(|text| text.chars().next())
        .filter(|character| !character.is_control())
}

/// What the poll can change about a pane, for telling whether it did.
#[derive(Debug, PartialEq)]
struct PolledPaneState {
    pane: crate::workspace::PaneId,
    activity: PaneActivity,
    status: TabStatus,
    branch: Option<String>,
    cwd: Option<std::path::PathBuf>,
    notice: Option<String>,
    task_notice: Option<String>,
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
    /// The pane last on screen, for telling when the user arrives at another. `None`
    /// while the user is in another window.
    seen_pane: Option<crate::workspace::PaneId>,
    /// Kept here because wakes arrive with no window to ask.
    window_presence: WindowPresence,
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
            seen_pane: None,
            window_presence: WindowPresence::default(),
        }
    }

    fn start(
        factory: Arc<dyn TerminalSessionFactory>,
        geometry: SessionGeometry,
    ) -> Result<(Self, async_channel::Receiver<()>), RegistryError> {
        let start = geometry
            .start_session(factory.as_ref(), None)
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
            PaneState::new(SessionGeneration(1), start.session, geometry.surface),
        );
        Ok((
            Self {
                sessions,
                factory,
                next_generation: 2,
                workspace,
                notice: None,
                close_window_requested: false,
                seen_pane: None,
                window_presence: WindowPresence::default(),
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
        geometry: SessionGeometry,
    ) -> Result<
        (
            crate::workspace::PaneId,
            SessionGeneration,
            async_channel::Receiver<()>,
        ),
        RegistryError,
    > {
        let mut start = geometry
            .start_session(self.factory.as_ref(), None)
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
        let generation = self.take_generation();
        let mut state = PaneState::new(generation, start.session, geometry.surface);
        state.cuenv.presentation = presentation;
        self.sessions.insert(target.pane, state);
        self.notice = None;
        Ok((target.pane, generation, start.wake_rx))
    }

    fn take_generation(&mut self) -> SessionGeneration {
        let generation = SessionGeneration(self.next_generation);
        // Even a session a millisecond would take half a billion years to get here.
        self.next_generation = self.next_generation.wrapping_add(1);
        generation
    }

    /// Replaces a pane's session with a fresh shell, in the same tab and the same directory
    /// where that still exists. All the pane knows from that directory stays, its project's
    /// look included, until the shell is seen to be somewhere else. The new generation
    /// leaves anything the old session still has on the way to be ignored. If no shell will
    /// start, the pane stays as it was.
    fn restart_pane(
        &mut self,
        pane: crate::workspace::PaneId,
        geometry: SessionGeometry,
    ) -> Result<StartedSession, RegistryError> {
        let directory = self
            .sessions
            .get(&pane)
            .ok_or(RegistryError::MissingSession(pane))?
            .cuenv
            .cwd
            .clone()
            .filter(|directory| directory.is_dir());
        let start = geometry
            .start_session(self.factory.as_ref(), directory.as_deref())
            .map_err(RegistryError::Session)?;
        let generation = self.take_generation();
        let mut fresh = PaneState::new(generation, start.session, geometry.surface);
        if let Some(previous) = self.sessions.get_mut(&pane) {
            fresh.cuenv = std::mem::take(&mut previous.cuenv);
            std::mem::swap(previous, &mut fresh);
            // What is being replaced had already failed, so its closing has nothing to add.
            let _ = fresh.session.close();
        }
        Ok(StartedSession {
            pane,
            generation,
            wake_rx: start.wake_rx,
        })
    }

    fn apply_wake(
        &mut self,
        pane: crate::workspace::PaneId,
        generation: SessionGeneration,
        active_pane: Option<crate::workspace::PaneId>,
    ) -> WakeOutcome {
        let presence = if active_pane == Some(pane) {
            TabPresence::Shown
        } else {
            TabPresence::Hidden
        }
        .within(self.window_presence);
        let effects = match self.sessions.get_mut(&pane) {
            Some(state) if state.generation == generation => {
                let effects = state.session.drain_effects();
                // Rio publishes Close after its final PTY drain. Sample that
                // state before the caller removes the pane, or, for a shell that
                // failed, leaves its final output on screen.
                let frame = state.session.frame();
                if shows_new_output(&state.frame, &frame) {
                    state.status.apply(StatusEvent::Output(presence));
                }
                state.frame = frame;
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
                ControlEvent::Bell => {
                    if let Some(state) = self.sessions.get_mut(&pane)
                        && state.generation == generation
                    {
                        state.status.apply(StatusEvent::Bell(presence));
                    }
                }
                ControlEvent::ChildExited(status) => {
                    if let SessionExit::Failed(failure) = SessionExit::from_wait_status(status)
                        && let Some(state) = self.sessions.get_mut(&pane)
                        && state.generation == generation
                    {
                        state.status.apply(StatusEvent::SessionFailed(failure));
                        // The session closes before this wake reaches the window, so a
                        // draw or key in that gap can have failed on it. The tab's final
                        // output and exit notice say more, and that message would hide them.
                        state.error = None;
                    }
                }
                // A failed shell's tab stays until the user has read it and closes it.
                ControlEvent::Close => {
                    should_close = self
                        .sessions
                        .get(&pane)
                        .is_none_or(|state| !state.has_exited());
                }
            }
        }
        WakeOutcome {
            accepted: true,
            clipboard,
            should_close,
        }
    }

    /// Tells the tab now on screen it has been seen, once for each arrival, so what
    /// happens while it stays on screen is still reported. Coming back to the window
    /// is an arrival too, and nothing is on screen for someone who is elsewhere.
    fn note_active_pane(&mut self) {
        let shown = self
            .active_pane()
            .filter(|_| self.window_presence == WindowPresence::Active);
        if shown != self.seen_pane {
            self.seen_pane = shown;
            if let Some(state) = shown.and_then(|pane| self.sessions.get_mut(&pane)) {
                state.status.apply(StatusEvent::Seen);
            }
        }
    }

    fn sample_foreground(&mut self, now: std::time::Instant) {
        self.sessions
            .values_mut()
            .for_each(|state| state.sample_foreground(now));
    }

    /// Keeps each tab's branch current. This is a file read at most every few seconds,
    /// which is why it is sampled here and never during a draw.
    fn sample_branches(&mut self, now: std::time::Instant) {
        for state in self.sessions.values_mut() {
            state.branch.sample(state.cuenv.cwd.as_deref(), now);
        }
    }

    /// Everything a draw shows that the periodic poll can change, so an idle window
    /// is only redrawn when something it shows did.
    fn polled_state(&self) -> Vec<PolledPaneState> {
        let mut panes: Vec<_> = self
            .sessions
            .iter()
            .map(|(pane, state)| PolledPaneState {
                pane: *pane,
                activity: state.activity.clone(),
                status: state.status,
                branch: state.branch.branch().map(str::to_owned),
                cwd: state.cuenv.cwd.clone(),
                notice: state.cuenv.notice.clone(),
                task_notice: state.cuenv.task_notice.clone(),
            })
            .collect();
        panes.sort_by_key(|state| state.pane);
        panes
    }

    /// Whether the active pane's cursor differs from what was last sampled. Rio marks no
    /// damage for DECTCEM or DECSCUSR, so a program that only shows or hides its cursor
    /// wakes nothing, and this is what notices.
    fn active_cursor_changed(&self) -> bool {
        self.active_pane()
            .and_then(|pane| self.sessions.get(&pane))
            .is_some_and(|state| {
                state.error.is_none() && state.session.cursor() != state.frame.cursor
            })
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
    /// The tab whose name is being edited in place, which takes the keyboard.
    pub(super) tab_rename: Option<TabRename>,
    /// Identifies the latest modifier change, so a delayed reveal can tell it has
    /// been overtaken.
    tab_hint_request: u64,
    sidebar: WindowSidebar,
    /// What the window manager was last told this window is called.
    window_title: String,
    cursor_animator: CursorAnimator,
    /// When the draw an animation has asked for is due.
    frame_due: Option<std::time::Instant>,
    /// What the platform says about motion, read once as the window opens.
    system_motion: MotionPace,
    pub(super) motion: ChromeMotion,
}

/// What besides the tabs is on screen for the pointer to be over.
struct LiveHoverTargets {
    notice: bool,
    tasks: bool,
    palette_hint: bool,
    palette: bool,
    task_sidebar: bool,
    session_actions: bool,
}

impl LiveHoverTargets {
    fn button(&self, button: SidebarButton) -> bool {
        match button {
            SidebarButton::NewTab | SidebarButton::Toggle => true,
            SidebarButton::Notice => self.notice,
            SidebarButton::Tasks => self.tasks,
            SidebarButton::PaletteHint => self.palette_hint,
        }
    }
}

/// What the window is called with no tab to name it after.
const APP_TITLE: &str = "Cuetty";

/// The key that toggles the sidebar with the application chord.
pub(super) const SIDEBAR_TOGGLE_KEY: &str = "b";

/// The key that renames the active tab with the application chord.
const RENAME_TAB_KEY: &str = "r";

/// The logical size assumed for the terminal surface until a draw has measured it.
const UNMEASURED_SURFACE: (f32, f32) = (720.0, 432.0);

fn unmeasured_surface() -> Size<Pixels> {
    size(px(UNMEASURED_SURFACE.0), px(UNMEASURED_SURFACE.1))
}

/// A session that has just started, and how it says it has something new to show.
struct StartedSession {
    pane: crate::workspace::PaneId,
    generation: SessionGeneration,
    wake_rx: async_channel::Receiver<()>,
}

/// The grid a session starts on.
#[derive(Debug, Clone, Copy)]
struct SessionGeometry {
    surface: Size<Pixels>,
    scale: ScaleFactor,
    cell: CellSize,
}

impl SessionGeometry {
    fn start_session(
        self,
        factory: &dyn TerminalSessionFactory,
        working_directory: Option<&std::path::Path>,
    ) -> Result<SessionStart, String> {
        let (width, height) = self.scale.whole_device_size(self.surface);
        factory.start(SessionRequest {
            width,
            height,
            cell: self.cell,
            working_directory,
        })
    }
}

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
                blinking: Known::Unknown,
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
            view.registry.window_presence = window_presence(window);
            if !window.is_window_active() {
                view.hide_tab_hints(cx);
            }
            // The cursor shows whether this window has the keyboard.
            cx.notify();
        })
        .detach();
        let config = TerminalConfig::default();
        let scale = ScaleFactor::new(window.scale_factor());
        let metrics = TerminalMetrics::from_config(cx.text_system(), &config, scale);
        let theme = TerminalTheme::from_config(&config);
        let window_background = theme.window_background();
        let factory: Arc<dyn TerminalSessionFactory> = Arc::new(RioTerminalFactory);
        let started = SessionRegistry::start(
            factory.clone(),
            SessionGeometry {
                surface: unmeasured_surface(),
                scale,
                cell: metrics.device_cell(),
            },
        );
        let (registry, sidebar, session) = match started {
            Ok((mut registry, wake_rx)) => {
                let pane = registry.active_pane().expect("initial workspace pane");
                registry.window_presence = window_presence(window);
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
                let session = StartedSession {
                    pane,
                    generation: SessionGeneration(1),
                    wake_rx,
                };
                (registry, sidebar, Some(session))
            }
            Err(error) => {
                let mut registry = SessionRegistry::empty(factory);
                registry.notice = Some(format!("Could not start a shell: {error}"));
                (registry, WindowSidebar::default(), None)
            }
        };
        let mut view = Self {
            registry,
            cuenv,
            config,
            metrics,
            theme,
            focus: cx.focus_handle(),
            renderer: Box::new(GridRenderer),
            workspace_bounds: Arc::new(Mutex::new(UNMEASURED_SURFACE)),
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
            tab_rename: None,
            tab_hint_request: 0,
            sidebar,
            window_title: APP_TITLE.into(),
            cursor_animator: CursorAnimator::new(std::time::Instant::now()),
            frame_due: None,
            system_motion: MotionPace::default(),
            motion: ChromeMotion::default(),
        };
        view.read_system_motion(cx);
        if let Some(session) = session {
            view.spawn_wake(session, cx);
        }
        // Also with no session yet: a shell started later from the empty window needs it.
        view.spawn_cwd_poll(cx);
        view.refresh_cuenv_integration(cx);
        view
    }
    /// Asks the platform off the UI thread, since that takes a process. The chrome moves
    /// as normal until the answer arrives, which is before anything has had reason to.
    fn read_system_motion(&mut self, cx: &mut Context<Self>) {
        let answer = cx.background_spawn(async { MotionPace::from_system() });
        cx.spawn(async move |weak, cx| {
            let system_motion = answer.await;
            let _ = weak.update(cx, |view, _cx| view.system_motion = system_motion);
        })
        .detach();
    }

    /// How far the chrome may move right now.
    pub(super) fn motion_pace(&self) -> MotionPace {
        MotionPace::resolve(self.config.motion, self.system_motion)
    }

    fn refresh_cuenv_integration(&mut self, cx: &mut Context<Self>) {
        let panes: Vec<_> = self.registry.sessions.keys().copied().collect();
        for pane in panes {
            self.refresh_cuenv_pane(pane, cx);
        }
    }

    fn refresh_cuenv_pane(&mut self, pane: crate::workspace::PaneId, cx: &mut Context<Self>) {
        // An exited shell has no directory to follow, and keeps its look and its name.
        if self
            .registry
            .sessions
            .get(&pane)
            .is_some_and(PaneState::has_exited)
        {
            return;
        }
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
            self.metrics =
                TerminalMetrics::from_config(cx.text_system(), &config, self.metrics.scale);
            self.theme = TerminalTheme::from_config(&config);
            self.config = config;
        }
    }

    fn reconcile_task_ui(&mut self) {
        let count = self
            .active_state()
            .map_or(0, |state| state.task_catalogue().len());
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
                        let shown = view.registry.polled_state();
                        view.registry.sample_foreground(std::time::Instant::now());
                        view.refresh_cuenv_integration(cx);
                        view.registry.sample_branches(std::time::Instant::now());
                        let cursor_changed = view.registry.active_cursor_changed();
                        if cursor_changed {
                            view.cursor_animator.touch(std::time::Instant::now());
                        }
                        if cursor_changed || view.registry.polled_state() != shown {
                            cx.notify();
                        }
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
        self.commit_tab_rename();
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
    fn spawn_wake(&mut self, session: StartedSession, cx: &mut Context<Self>) {
        let StartedSession {
            pane,
            generation,
            wake_rx,
        } = session;
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
        if self.active_pane() == Some(pane) {
            self.cursor_animator.touch(std::time::Instant::now());
        }
        self.refresh_cuenv_pane(pane, cx);
        // The shell may just have gone from under an open palette.
        self.reconcile_task_ui();
        for text in outcome.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if outcome.should_close {
            self.close_pane(pane, cx);
        }
        cx.notify();
    }
    /// What a click on a tab's row does: a second click starts renaming what the first
    /// selected, and clicks on the name being edited leave the edit alone.
    pub(super) fn tab_row_clicked(
        &mut self,
        click: TabClick,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let TabClick { tab, click_count } = click;
        if self.tab_rename.as_ref().map(TabRename::tab) == Some(tab) {
            self.focus.focus(window);
        } else if click_count >= 2 {
            self.begin_tab_rename(tab, window, cx);
        } else {
            self.select_terminal_tab(tab, window, cx);
        }
    }

    /// Starts editing a tab's name in its row, which needs the sidebar to be expanded.
    fn begin_tab_rename(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_tab_rename();
        let tabs = self.registry.workspace.workspace().tabs();
        let Some(index) = tabs.iter().position(|candidate| candidate.id == tab) else {
            return;
        };
        self.tab_rename = Some(TabRename::new(tab, tabs[index].custom_name.as_deref()));
        self.task_palette_open = false;
        // Settled, so that a first evaluation still on its way cannot collapse the row
        // out from under the edit.
        self.sidebar.expand();
        // Keys go to the name until the edit ends, so the name has to be in sight.
        self.rail_scroll.scroll_to_item(index);
        self.focus.focus(window);
        cx.notify();
    }

    /// Settles a name being edited, as anything but Escape does.
    pub(super) fn commit_tab_rename(&mut self) {
        if let Some(rename) = self.tab_rename.take()
            && let Err(error) = self
                .registry
                .workspace
                .rename_tab(rename.tab(), rename.into_name())
        {
            self.registry.notice = Some(shell_notice(error));
        }
    }

    /// Whether the key belonged to a name being edited, which takes every key until the
    /// edit ends so that nothing typed there can reach the shell.
    fn handle_rename_key(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(rename) = self.tab_rename.as_mut() else {
            return false;
        };
        let keystroke = &event.keystroke;
        match rename_key(
            &keystroke.key,
            keystroke.key_char.as_deref(),
            keystroke.modifiers,
        ) {
            RenameKey::Commit => self.commit_tab_rename(),
            RenameKey::Cancel => self.tab_rename = None,
            RenameKey::Backspace => rename.backspace(),
            RenameKey::Text(text) => rename.insert(text),
            RenameKey::Ignored => {}
        }
        cx.notify();
        true
    }

    /// Moves a dragged tab to where it was dropped in the tab list.
    pub(super) fn move_tab(&mut self, tab: TabId, index: usize, cx: &mut Context<Self>) {
        if let Err(error) = self.registry.workspace.move_tab(tab, index) {
            self.registry.notice = Some(shell_notice(error));
        }
        cx.notify();
    }

    /// Hands the keyboard back to the terminal after a click in the chrome.
    pub(super) fn focus_terminal(&self, window: &mut Window) {
        self.focus.focus(window);
    }
    pub(super) fn close_pane(&mut self, pane: crate::workspace::PaneId, cx: &mut Context<Self>) {
        if let Err(error) = self.registry.close_pane(pane) {
            self.registry.notice = Some(format!("workspace action unavailable: {error}"));
        } else {
            let tabs = self.registry.workspace.workspace().tabs();
            self.tab_rename
                .take_if(|rename| tabs.iter().all(|tab| tab.id != rename.tab()));
            self.sync_active_config(cx);
            self.reconcile_task_ui();
        }
    }
    pub(super) fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_tab_rename();
        // A tab shares the surface its siblings measured, so its shell starts on the
        // right grid rather than getting a resize right after its first prompt.
        let surface = self
            .active_state()
            .map_or_else(unmeasured_surface, PaneState::surface_for_sibling);
        match self.registry.start_tab(SessionGeometry {
            surface,
            scale: self.metrics.scale,
            cell: self.metrics.device_cell(),
        }) {
            Ok((pane, generation, wake_rx)) => {
                self.spawn_wake(
                    StartedSession {
                        pane,
                        generation,
                        wake_rx,
                    },
                    cx,
                );
                self.refresh_cuenv_integration(cx);
                self.sync_active_config(cx);
                self.reconcile_task_ui();
                if self.active_tab().is_some() {
                    self.focus.focus(window);
                }
            }
            Err(RegistryError::Session(error)) => {
                self.registry.notice = Some(format!("Could not start a shell: {error}"))
            }
            Err(error) => {
                self.registry.notice = Some(format!("workspace action unavailable: {error}"))
            }
        }
        // Whichever way it went there is something new to show, and a failure has only this.
        cx.notify();
    }
    /// Starts a fresh shell in the active tab, in place of one that stopped working. If
    /// none will start the tab stays as it is, saying why.
    pub(super) fn restart_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let surface = self
            .active_state()
            .map_or_else(unmeasured_surface, PaneState::surface_for_sibling);
        match self.registry.restart_pane(
            pane,
            SessionGeometry {
                surface,
                scale: self.metrics.scale,
                cell: self.metrics.device_cell(),
            },
        ) {
            Ok(session) => {
                self.spawn_wake(session, cx);
                self.refresh_cuenv_pane(pane, cx);
                self.sync_active_config(cx);
                self.focus.focus(window);
            }
            Err(error) => {
                if let Some(state) = self.active_state_mut() {
                    state.error = Some(format!("Could not restart the shell: {error}"));
                }
            }
        }
    }

    /// Closes the active tab and hands the keyboard back to whichever tab is left.
    pub(super) fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.active_pane() {
            self.close_pane(pane, cx);
            self.focus.focus(window);
        }
    }

    /// Makes `element` take the keys the terminal would, for what stands in for it.
    pub(super) fn focused_for_keys(&self, element: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        element
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
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
        let (x, y) = *state
            .viewport_origin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                    state.frame = state.session.frame();
                    state.interaction.viewport_changed(&state.frame);
                }
                // A program's mouse mode outlives it, and there is nothing left to tell.
                Err(_) if state.has_exited() => {}
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

    /// The terminal grid follows once, from the bounds measured in the layout this change
    /// causes, however long the panel takes to get there; see [`SidebarFrame`].
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        // A collapsed tab has nowhere to show the name being edited.
        self.commit_tab_rename();
        self.sidebar.toggle();
        // Rows change height between the two, which can leave the active tab off screen.
        self.rail_revealed_tab = None;
        cx.notify();
    }

    /// Toggles the task palette, returning whether the active pane has a
    /// task catalogue to show.
    pub(super) fn toggle_task_palette(&mut self) -> bool {
        self.commit_tab_rename();
        if self
            .active_state()
            .is_none_or(|state| state.task_catalogue().is_empty())
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
            && !state.has_exited()
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

    /// Works out the cursor's look for this draw, and asks for the next draw if it is
    /// animating. A cursor at rest asks for none, so an idle window stops drawing.
    fn cursor_frame(&mut self, seen: CursorObservation, cx: &mut Context<Self>) -> CursorFrame {
        let frame = self.cursor_animator.frame(seen);
        if let Some(delay) = frame.next_frame {
            self.request_frame(seen.now + delay, delay, cx);
        }
        frame
    }

    /// Works out the sidebar's width for this draw, and asks for the next draw while
    /// anything in the chrome is moving.
    fn animate_chrome(&mut self, cx: &mut Context<Self>) -> SidebarFrame {
        let now = std::time::Instant::now();
        let pace = self.motion_pace();
        let width = sidebar_width(self.sidebar_mode());
        let sidebar = self.motion.sidebar_frame(width, pace, now);
        let tabs: Vec<TabId> = self
            .registry
            .workspace
            .workspace()
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect();
        self.motion.follow_tabs(
            TabPlaces {
                tabs: &tabs,
                active: self.active_tab(),
            },
            pace,
            now,
        );
        let live = self.live_hover_targets();
        self.motion.forget_hovers(now, |target| match *target {
            HoverTarget::Tab(tab) => tabs.contains(&tab),
            HoverTarget::Button(button) => live.button(button),
            HoverTarget::PaletteRow(_) => live.palette,
            HoverTarget::TaskRow(_) => live.task_sidebar,
            HoverTarget::SessionAction(_) => live.session_actions,
        });
        if let Some(delay) = self.motion.next_frame(now) {
            self.request_frame(now + delay, delay, cx);
        }
        sidebar
    }

    /// What is on screen to be hovered, besides the tabs.
    fn live_hover_targets(&self) -> LiveHoverTargets {
        let has_tasks = self
            .active_state()
            .is_some_and(|state| !state.task_catalogue().is_empty());
        LiveHoverTargets {
            notice: self.registry.notice.is_some()
                || self
                    .active_state()
                    .is_some_and(|state| state.marker_notice().is_some()),
            tasks: has_tasks,
            palette_hint: has_tasks && self.sidebar_mode() == SidebarMode::Expanded,
            palette: self.task_palette_open,
            task_sidebar: self.task_sidebar_open,
            session_actions: self
                .active_state()
                .is_none_or(|state| state.error.is_some()),
        }
    }

    /// One timer is enough: only one sooner than any already waiting replaces it.
    fn request_frame(
        &mut self,
        due: std::time::Instant,
        delay: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        if self.frame_due.is_some_and(|waiting| waiting <= due) {
            return;
        }
        self.frame_due = Some(due);
        cx.spawn(async move |weak, cx| {
            Timer::after(delay).await;
            let _ = weak.update(cx, |view, cx| {
                if view.frame_due == Some(due) {
                    view.frame_due = None;
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
        if self.handle_rename_key(event, cx) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let palette = self.active_state().and_then(PaneState::palette_chord);
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
        if app_chord(modifiers).is_some() && key.eq_ignore_ascii_case(RENAME_TAB_KEY) {
            if let Some(tab) = self.active_tab() {
                self.begin_tab_rename(tab, window, cx);
            }
            return;
        }
        // Every pane in the workspace registry owns a live Rio session. Keep
        // input, search, copy, and paste routed through that registry entry.
        if self.active_state().is_none() {
            if takes_offered_action(event) {
                self.new_tab(window, cx);
            }
            return;
        }
        // In place of a terminal that stopped working there is only what it offers.
        if self
            .active_state()
            .is_some_and(|state| state.error.is_some())
        {
            if takes_offered_action(event) {
                self.restart_shell(window, cx);
            }
            // Whatever failed may have asked for no draw, and then this key is what shows it.
            cx.notify();
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
        if app_chord(modifiers).is_some() && key.eq_ignore_ascii_case("c") {
            if let Some(text) = self
                .active_state()
                .and_then(|state| state.interaction.copy(&state.frame))
            {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
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
                _ => typed_character(&event.keystroke, AltKey::current()).map(SearchKey::Character),
            };
            if let Some(search_key) = search_key {
                self.search_key(search_key);
                cx.notify();
            }
            return;
        }
        // Nothing is left to type at, though search and copy above still read the output.
        if self.active_state().is_some_and(PaneState::has_exited) {
            if closes_exited_tab(event)
                && let Some(pane) = self.active_pane()
            {
                self.close_pane(pane, cx);
                cx.notify();
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
                cx.notify();
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
        // A key with no echo, such as at a password prompt, still shows the cursor.
        self.cursor_animator.touch(std::time::Instant::now());
        cx.notify();
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

impl TerminalView {
    /// The padding configured above the terminal.
    pub(super) fn top_padding(&self) -> Pixels {
        px(f32::from(self.config.terminal_padding.top))
    }

    /// A draw of the active tab in place of a terminal that stopped working.
    fn render_pane_failure(
        &mut self,
        panels: ShellPanels,
        error: &str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // The panels it would have shown over have gone, and must not go on taking keys.
        self.reconcile_task_ui();
        let theme = self.theme.clone();
        let problem = self.render_session_problem(SessionProblem::Failed { error }, &theme, cx);
        self.render_shell_layout(panels, problem, &theme)
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        if self.registry.close_window_requested {
            window.remove_window();
        }
        // The active pane controls both painting and PTY geometry. Background
        // configuration updates must never alter the visible pane's metrics.
        self.metrics.scale = ScaleFactor::new(window.scale_factor());
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
        let sidebar_frame = self.animate_chrome(cx);
        let active_tab = self.active_tab();
        self.reveal_active_rail_tab(active_tab);
        self.registry.note_active_pane();
        let sidebar = self.render_sidebar(active_tab, &theme, cx);
        let panels = ShellPanels {
            sidebar,
            sidebar_frame,
        };
        let Some(active_pane) = self.active_pane() else {
            let notice = self.registry.notice.clone();
            let problem = self.render_session_problem(
                SessionProblem::NoSession {
                    notice: notice.as_deref(),
                },
                &theme,
                cx,
            );
            return self.render_shell_layout(panels, problem, &theme);
        };
        let Some(state) = self.registry.sessions.get(&active_pane) else {
            let problem = self.render_session_problem(SessionProblem::Lost, &theme, cx);
            return self.render_shell_layout(panels, problem, &theme);
        };
        let pane_error = state.error.clone();
        let exit_failure = state.status.failure();
        let measured = Arc::clone(&state.measured_bounds);
        let viewport_origin = Arc::clone(&state.viewport_origin);
        let entity = cx.weak_entity();
        // Focus stays with the terminal while another window is active, but keys don't.
        let window_active = window.is_window_active();
        let focus = if self.focus.is_focused(window) && window_active {
            CursorFocus::Focused
        } else {
            CursorFocus::Unfocused
        };
        if let Some(error) = &pane_error {
            return self.render_pane_failure(panels, error, cx);
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
            let surface = *state
                .measured_bounds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (width, height) = self.metrics.scale.whole_device_size(surface);
            state.fit_grid(width, height, self.metrics.device_cell());
            // Sampled after the resize, so the draw that resizes the grid also shows it.
            state.frame = state.session.frame();
            state.interaction.refresh_search(&state.frame);
            state.frame.clone()
        });
        let Some(frame) = frame else {
            let problem = self.render_session_problem(SessionProblem::Lost, &theme, cx);
            return self.render_shell_layout(panels, problem, &theme);
        };
        // Fitting the grid just above is the one failure that happens during a draw.
        let fit_error = self
            .registry
            .sessions
            .get(&active_pane)
            .and_then(|state| state.error.clone());
        if let Some(error) = &fit_error {
            return self.render_pane_failure(panels, error, cx);
        }
        let overlays = self
            .registry
            .sessions
            .get(&active_pane)
            .map(|state| TerminalOverlays {
                selection: state.interaction.selection(),
                search_matches: state.interaction.search_matches().to_vec(),
                current_search_match: state.interaction.current_match(),
            })
            .unwrap_or_default();
        let cursor = self.cursor_frame(
            CursorObservation {
                pane: active_pane,
                position: frame
                    .cursor
                    .position
                    .filter(|_| frame.cursor.visible != Known::Known(false)),
                focus,
                blink: blink_wanted(self.config.cursor_blink, frame.cursor.blinking),
                trail: self.config.cursor_trail && self.motion_pace() == MotionPace::Full,
                now: std::time::Instant::now(),
            },
            cx,
        );
        let mut content = Some(self.renderer.render(RenderRequest {
            frame: &frame,
            metrics: &metrics,
            theme: &theme,
            focus,
            window: window_presence(window),
            cursor,
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
                let pane_viewport_origin = Arc::clone(&viewport_origin);
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
                                    if store_if_changed(&pane_measured, bounds.size) {
                                        redraw_after_this_frame(pane_entity, app);
                                    }
                                    store_if_changed(
                                        &pane_viewport_origin,
                                        (f32::from(bounds.origin.x), f32::from(bounds.origin.y)),
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
        let top_padding = self.top_padding();
        let surface = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                terminal_surface
                    .absolute()
                    .top(self.top_inset(top_padding))
                    .right(px(f32::from(padding.right)))
                    .bottom(self.bottom_inset(px(f32::from(padding.bottom))))
                    .left(px(f32::from(padding.left))),
            )
            .children(exit_failure.map(|failure| render_exit_notice(failure, &theme)))
            .children(self.window_controls_band(top_padding));
        self.render_shell_layout(panels, surface.into_any_element(), &theme)
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
                    view.focus.focus(window);
                })
                .expect("focus Cuetty terminal");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::color::contrast_ratio;
    use crate::terminal::foreground::ForegroundProcess;
    use crate::terminal::model::Color;
    use crate::terminal::rio_input::{encode_key, encode_paste};
    use crate::terminal::selection::LogicalPosition;
    use crate::terminal::tab_status::TabIndicator;
    use rio_vt::crosswords::Mode;
    use std::collections::VecDeque;

    struct FakeSession {
        frame: TerminalFrame,
        effects: Arc<Mutex<Vec<ControlEvent>>>,
        close_error: Option<String>,
        foreground: Arc<Mutex<String>>,
        /// What a shell with bracketed paste on would read from the PTY.
        sent: Arc<Mutex<Vec<u8>>>,
        /// A cursor a test can move, for a change that marks no damage.
        cursor: Option<Arc<Mutex<CursorState>>>,
    }

    impl TerminalSession for FakeSession {
        fn resize(&mut self, _: u32, _: u32, _: CellSize) -> Result<(), String> {
            Ok(())
        }

        fn reflow_output(&mut self, _: u32, height: u32, cell: CellSize) {
            self.frame
                .rows
                .truncate((height / cell.height.max(1)) as usize);
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
            self.frame.cursor = self.cursor();
            self.frame.clone()
        }

        fn cursor(&self) -> CursorState {
            self.cursor
                .as_ref()
                .map_or(self.frame.cursor, |cursor| *cursor.lock().unwrap())
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
        directories: Arc<StartDirectories>,
    }

    /// The directory each session was asked to start in, oldest first.
    type StartDirectories = Mutex<Vec<Option<std::path::PathBuf>>>;

    impl TerminalSessionFactory for FakeFactory {
        fn start(&self, request: SessionRequest<'_>) -> Result<SessionStart, String> {
            self.directories
                .lock()
                .expect("fake factory mutex poisoned")
                .push(request.working_directory.map(std::path::Path::to_path_buf));
            self.starts
                .lock()
                .expect("fake factory mutex poisoned")
                .pop_front()
                .unwrap_or_else(|| Err("fake factory exhausted".into()))
        }
    }

    fn key_press(key: &str, modifiers: gpui::Modifiers) -> gpui::KeyDownEvent {
        gpui::KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers,
                key: key.into(),
                key_char: None,
            },
            is_held: false,
        }
    }

    fn frame_with(character: char) -> TerminalFrame {
        let mut frame = TerminalView::empty_frame();
        frame.rows[0].cells[0] = TerminalCell::narrow(character);
        frame
    }

    fn keystroke_typing(key_char: &str, modifiers: gpui::Modifiers) -> gpui::Keystroke {
        gpui::Keystroke {
            modifiers,
            key: "b".into(),
            key_char: Some(key_char.into()),
        }
    }

    #[test]
    fn alt_types_into_a_query_only_where_it_composes_characters() {
        let alt = gpui::Modifiers {
            alt: true,
            ..gpui::Modifiers::default()
        };
        assert_eq!(
            typed_character(&keystroke_typing("b", alt), AltKey::Chords),
            None
        );
        assert_eq!(
            typed_character(&keystroke_typing("∫", alt), AltKey::Composes),
            Some('∫')
        );
    }

    #[test]
    fn control_and_platform_chords_are_never_text_and_plain_keys_are() {
        let plain = gpui::Modifiers::default();
        assert_eq!(
            typed_character(&keystroke_typing("B", plain), AltKey::Chords),
            Some('B')
        );
        let chords = [
            gpui::Modifiers {
                control: true,
                ..plain
            },
            gpui::Modifiers {
                platform: true,
                ..plain
            },
        ];
        for chord in chords {
            for alt_key in [AltKey::Chords, AltKey::Composes] {
                assert_eq!(
                    typed_character(&keystroke_typing("b", chord), alt_key),
                    None
                );
            }
        }
    }

    #[test]
    fn a_control_character_is_not_text() {
        assert_eq!(
            typed_character(
                &keystroke_typing("\u{7f}", gpui::Modifiers::default()),
                AltKey::Chords
            ),
            None
        );
    }

    #[test]
    fn a_font_line_is_centred_in_its_cell_whichever_way_descent_is_signed() {
        for descent in [px(4.0), px(-4.0)] {
            assert_eq!(baseline_in_cell(px(19.0), px(15.0), descent), px(15.0));
            assert_eq!(baseline_in_cell(px(25.0), px(15.0), descent), px(18.0));
        }
    }

    #[test]
    fn a_glyph_starts_on_its_cell_however_far_the_font_has_advanced() {
        let starts = [0, 1, 2];
        let mut placer = GlyphPlacer::new(&starts);
        // The font's 9.6px advance on a smaller cell would otherwise drift.
        for (column, x) in [0.0, 9.6, 19.2].into_iter().enumerate() {
            assert_eq!(
                placer.place(column, x),
                PlacedGlyph {
                    cluster: column,
                    offset: 0.0
                }
            );
        }
    }

    #[test]
    fn a_combining_mark_stays_in_its_cell_without_moving_the_next() {
        // "e" + U+0301 in one cell, then "x".
        let starts = [0, 3];
        let mut placer = GlyphPlacer::new(&starts);
        assert_eq!(
            placer.place(0, 0.0),
            PlacedGlyph {
                cluster: 0,
                offset: 0.0
            }
        );
        assert_eq!(
            placer.place(1, 0.5),
            PlacedGlyph {
                cluster: 0,
                offset: 0.5
            }
        );
        assert_eq!(
            placer.place(3, 9.6),
            PlacedGlyph {
                cluster: 1,
                offset: 0.0
            }
        );
    }

    #[test]
    fn a_glyph_that_replaced_cells_leaves_the_next_cell_in_place() {
        // A font's `calt` turned "->" into one glyph, then "x" follows it.
        let starts = [0, 1, 2];
        let mut placer = GlyphPlacer::new(&starts);
        assert_eq!(placer.place(0, 0.0).cluster, 0);
        assert_eq!(
            placer.place(2, 9.6),
            PlacedGlyph {
                cluster: 2,
                offset: 0.0
            }
        );
    }

    #[test]
    fn decorations_merge_only_while_their_style_runs_on() {
        assert_eq!(
            merged_spans([Some('a'), Some('a'), None, Some('a'), Some('b'), Some('b')]),
            [(0..2, 'a'), (3..4, 'a'), (4..6, 'b')]
        );
        assert_eq!(merged_spans::<char>([None, None]), []);
    }

    #[test]
    fn only_a_lone_narrow_line_or_block_character_is_drawn_as_a_shape() {
        assert_eq!(
            drawn_character(&TerminalCell::narrow('\u{2500}')),
            Some('\u{2500}')
        );
        assert_eq!(drawn_character(&TerminalCell::narrow('a')), None);
        assert_eq!(drawn_character(&TerminalCell::trailing_spacer()), None);
        assert_eq!(drawn_character(&TerminalCell::wide('\u{2500}')), None);
        let mut combined = TerminalCell::narrow('\u{2500}');
        combined.text = Some("\u{2500}\u{0301}".into());
        assert_eq!(drawn_character(&combined), None);
    }

    #[test]
    fn the_poll_sees_a_change_only_when_a_pane_shows_something_new() {
        let (start, foreground, _) = fake_start_in_foreground("zsh");
        let (mut registry, _) =
            SessionRegistry::start(fake_factory(vec![Ok(start)]), test_geometry()).unwrap();
        let now = std::time::Instant::now();
        registry.sample_foreground(now);
        let shown = registry.polled_state();

        registry.sample_foreground(now + std::time::Duration::from_secs(1));
        assert_eq!(
            registry.polled_state(),
            shown,
            "an idle shell changes nothing"
        );

        *foreground.lock().unwrap() = "vim notes.txt".into();
        registry.sample_foreground(now + std::time::Duration::from_secs(2));
        assert_ne!(
            registry.polled_state(),
            shown,
            "a new program renames the tab"
        );
    }

    #[test]
    fn the_poll_notices_a_cursor_that_changed_without_any_damage() {
        let cursor = Arc::new(Mutex::new(frame_with('a').cursor));
        let (wake_tx, wake_rx) = async_channel::unbounded();
        drop(wake_tx);
        let session = FakeSession {
            frame: frame_with('a'),
            effects: Arc::default(),
            close_error: None,
            foreground: Arc::new(Mutex::new("zsh".into())),
            sent: Arc::default(),
            cursor: Some(Arc::clone(&cursor)),
        };
        let start = SessionStart {
            session: Box::new(session),
            wake_rx,
        };
        let (mut registry, _) =
            SessionRegistry::start(fake_factory(vec![Ok(start)]), test_geometry()).unwrap();
        assert!(!registry.active_cursor_changed(), "nothing has happened");

        cursor.lock().unwrap().visible = Known::Known(false);
        assert!(registry.active_cursor_changed());

        // A draw samples the frame, after which there is nothing left to show.
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        state.frame = state.session.frame();
        assert!(!registry.active_cursor_changed());
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
            cursor: None,
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
            cursor: None,
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
        fake_factory_recording(plans).0
    }

    fn fake_factory_recording(
        plans: Vec<Result<SessionStart, String>>,
    ) -> (Arc<dyn TerminalSessionFactory>, Arc<StartDirectories>) {
        let directories = Arc::new(StartDirectories::default());
        let factory = Arc::new(FakeFactory {
            starts: Mutex::new(plans.into_iter().collect()),
            directories: Arc::clone(&directories),
        });
        (factory, directories)
    }

    fn test_cell() -> CellSize {
        CellSize {
            width: 8,
            height: 16,
        }
    }

    fn test_geometry() -> SessionGeometry {
        SessionGeometry {
            surface: size(px(80.0), px(40.0)),
            scale: ScaleFactor::new(1.0),
            cell: test_cell(),
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
    fn dim_text_fades_towards_the_background_it_is_drawn_on() {
        let mut cell = TerminalCell::narrow('x');
        cell.foreground = Color::Rgb(Rgb(100, 100, 100));
        cell.background = Color::Rgb(Rgb(200, 200, 200));
        cell.style.dim = true;
        assert_eq!(
            colors(&cell, &TerminalTheme::default(), CellLayer::Cell).0,
            Rgb(150, 150, 150)
        );
        // An inverse cell fades towards what is now behind it.
        cell.style.inverse = true;
        assert_eq!(
            colors(&cell, &TerminalTheme::default(), CellLayer::Cell).0,
            Rgb(150, 150, 150)
        );
    }

    /// A cell with no layer above it, on a selection or not.
    fn cell_painted(cell: &TerminalCell, tint: Option<Rgb>) -> PaintedCell<'_> {
        PaintedCell {
            cell,
            layer: CellLayer::Cell,
            tint,
        }
    }

    #[test]
    fn a_search_match_tints_a_cell_as_a_selection_does_and_keeps_its_glyphs_colours() {
        let theme = TerminalTheme::from_config(&TerminalConfig::default());
        let at = |column| CellPosition { row: 0, column };
        let span = |kind, start_column, end_column| CellOverlaySpan {
            row: 0,
            start_column,
            end_column,
            kind,
        };
        let overlays = [
            span(OverlayKind::Selection, 0, 2),
            span(OverlayKind::SearchMatch, 1, 3),
            span(OverlayKind::CurrentSearchMatch, 3, 4),
        ];
        assert_eq!(
            quiet_tint_at(&overlays, at(0), &theme),
            Some(theme.selection)
        );
        assert_eq!(
            quiet_tint_at(&overlays, at(1), &theme),
            Some(theme.search_match),
            "painted last where they overlap"
        );
        assert_eq!(quiet_tint_at(&overlays, at(3), &theme), None);
        assert_eq!(quiet_tint_at(&overlays, at(4), &theme), None);
        let matched_layer = cell_layer(None, &overlays, at(2));
        assert_eq!(matched_layer, CellLayer::Cell);
    }

    #[test]
    fn selected_dim_text_fades_towards_the_selection_behind_it() {
        let theme = TerminalTheme::default();
        let mut cell = TerminalCell::narrow('x');
        cell.style.dim = true;
        let selected = readable_foreground(cell_painted(&cell, Some(theme.selection)), &theme);
        assert_eq!(selected, dimmed(theme.text, theme.selection));
        let unselected = readable_foreground(cell_painted(&cell, None), &theme);
        assert_eq!(unselected, dimmed(theme.text, theme.surface));
        assert!(
            contrast_ratio(selected, theme.selection) > contrast_ratio(unselected, theme.selection),
            "selecting dim text must not make it harder to read"
        );
    }

    fn contrast_theme() -> TerminalTheme {
        TerminalTheme::from_config(&TerminalConfig {
            minimum_contrast: crate::terminal::config::MinimumContrast::new(4.5)
                .expect("a valid ratio"),
            ..TerminalConfig::default()
        })
    }

    #[test]
    fn minimum_contrast_lifts_palette_text_but_not_explicit_colours_or_graphics() {
        let theme = contrast_theme();
        let surface = theme.surface;
        let readable = |cell: &TerminalCell, selection| {
            readable_foreground(cell_painted(cell, selection), &theme)
        };
        let mut palette = TerminalCell::narrow('x');
        // ANSI black sits close to the default surface.
        palette.foreground = Color::Indexed(0);
        let lifted = readable(&palette, None);
        assert!(contrast_ratio(lifted, surface) >= 4.5);

        let mut explicit = palette.clone();
        explicit.foreground = Color::Rgb(Rgb(40, 40, 50));
        assert_eq!(readable(&explicit, None), Rgb(40, 40, 50));

        // Drawn as a shape, and through the font: both have to meet their neighbours.
        for graphic in ["\u{E0B0}", "\u{E0B4}", "\u{2801}", "\u{1FB00}"] {
            let mut cell = palette.clone();
            cell.text = Some(graphic.into());
            assert_eq!(readable(&cell, None), theme.ansi[0], "{graphic}");
        }
    }

    #[test]
    fn minimum_contrast_measures_against_a_selection_beneath_the_cell() {
        let theme = contrast_theme();
        let mut cell = TerminalCell::narrow('x');
        cell.foreground = Color::Indexed(7);
        let plain = readable_foreground(cell_painted(&cell, None), &theme);
        let selected = Rgb(170, 170, 180);
        let on_selection = readable_foreground(cell_painted(&cell, Some(selected)), &theme);
        assert_ne!(plain, on_selection);
        assert!(contrast_ratio(on_selection, selected) >= 4.5);
    }

    #[test]
    fn minimum_contrast_leaves_a_layer_with_its_own_colours_alone() {
        let theme = TerminalTheme {
            cursor_text: Rgb(240, 225, 230),
            ..contrast_theme()
        };
        assert_eq!(
            readable_foreground(
                PaintedCell {
                    cell: &TerminalCell::narrow('x'),
                    layer: CellLayer::BlockCursor,
                    tint: None,
                },
                &theme,
            ),
            theme.cursor_text
        );
    }

    #[test]
    fn bold_cells_take_the_configured_weight() {
        let mut config = TerminalConfig::default();
        config.font.bold_weight = 800;
        let metrics = TerminalMetrics {
            font: gpui::font("Test"),
            font_size: px(16.0),
            bold_weight: FontWeight(f32::from(config.font.bold_weight)),
            cell: size(px(9.0), px(19.0)),
            scale: ScaleFactor::new(1.0),
        };
        let mut cell = TerminalCell::narrow('x');
        assert_eq!(cell_font(&cell, &metrics).weight, FontWeight::NORMAL);
        cell.style.bold = true;
        assert_eq!(cell_font(&cell, &metrics).weight, FontWeight(800.0));
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
            cursor_style_at(
                &frame,
                CursorFocus::Focused,
                CellPosition { row: 0, column: 0 }
            ),
            Some(HostCursorStyle::Block)
        );
        assert_eq!(
            cursor_style_at(
                &frame,
                CursorFocus::Focused,
                CellPosition { row: 0, column: 1 }
            ),
            None
        );
        frame.cursor.visible = Known::Known(false);
        assert_eq!(
            cursor_style_at(
                &frame,
                CursorFocus::Focused,
                CellPosition { row: 0, column: 0 }
            ),
            None
        );
    }

    fn wide_character_row() -> Vec<TerminalCell> {
        vec![
            TerminalCell::narrow('a'),
            TerminalCell::wide('\u{4e16}'),
            TerminalCell::trailing_spacer(),
            TerminalCell::narrow('b'),
            TerminalCell::leading_spacer(),
        ]
    }

    #[test]
    fn a_cursor_covers_both_halves_of_a_wide_character() {
        let cells = wide_character_row();
        assert_eq!(cursor_columns(&cells, 0), 0..1);
        assert_eq!(cursor_columns(&cells, 1), 1..3);
        assert_eq!(cursor_columns(&cells, 2), 1..3);
        assert_eq!(cursor_columns(&cells, 3), 3..4);
        assert_eq!(cursor_columns(&cells, 4), 4..5);
    }

    #[test]
    fn a_cursor_never_covers_a_column_the_row_does_not_have() {
        assert_eq!(cursor_columns(&[TerminalCell::wide('\u{4e16}')], 0), 0..1);
        assert_eq!(cursor_columns(&[], 3), 3..4);
        // A spacer with no wide character before it stays on its own column.
        assert_eq!(cursor_columns(&[TerminalCell::trailing_spacer()], 0), 0..1);
    }

    #[test]
    fn a_wide_character_under_a_cursor_is_the_cursors_on_both_halves() {
        let mut frame = TerminalView::empty_frame();
        frame.rows[0].cells = wide_character_row();
        for cursor_column in [1, 2] {
            frame.cursor.position = Some((0, cursor_column));
            let style_at = |column| {
                cursor_style_at(
                    &frame,
                    CursorFocus::Focused,
                    CellPosition { row: 0, column },
                )
            };
            assert_eq!(style_at(0), None);
            assert_eq!(style_at(1), Some(HostCursorStyle::Block));
            assert_eq!(style_at(2), Some(HostCursorStyle::Block));
            assert_eq!(style_at(3), None);
        }
    }

    #[test]
    fn an_unfocused_cursor_is_an_outline_unless_it_is_hidden() {
        let mut frame = TerminalView::empty_frame();
        frame.cursor.position = Some((0, 0));
        for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
            frame.cursor.shape = Known::Known(shape);
            assert_eq!(
                cursor_style_at(
                    &frame,
                    CursorFocus::Unfocused,
                    CellPosition { row: 0, column: 0 }
                ),
                Some(HostCursorStyle::Hollow)
            );
        }
        assert_eq!(
            cursor_style_at(
                &frame,
                CursorFocus::Unfocused,
                CellPosition { row: 0, column: 1 }
            ),
            None
        );
        frame.cursor.visible = Known::Known(false);
        assert_eq!(
            cursor_style_at(
                &frame,
                CursorFocus::Unfocused,
                CellPosition { row: 0, column: 0 }
            ),
            None
        );
    }

    #[test]
    fn an_outline_leaves_the_glyph_inside_it_its_own_colours() {
        let position = CellPosition { row: 0, column: 0 };
        assert_eq!(
            cell_layer(Some(HostCursorStyle::Hollow), &[], position),
            CellLayer::Cell
        );
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let active = registry.active_pane().unwrap();
        assert_eq!(registry.workspace.workspace().tabs().len(), 1);
        assert_eq!(registry.sessions.len(), 1);
        let error = registry.start_tab(test_geometry()).unwrap_err();
        assert_eq!(error, RegistryError::Session("spawn failed".into()));
        assert_eq!(registry.active_pane(), Some(active));
        assert_eq!(registry.workspace.workspace().tabs().len(), 1);
        assert_eq!(registry.sessions.len(), 1);
    }

    #[test]
    fn restarting_a_broken_pane_gives_it_a_fresh_session_in_the_same_tab() {
        let (initial, old_effects) = fake_start('a', Vec::new(), None);
        let (replacement, new_effects) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Ok(replacement)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let old_generation = registry.sessions[&pane].generation;
        let state = registry.sessions.get_mut(&pane).unwrap();
        state.error = Some("pipe gone".into());
        state.cuenv.presentation.banner = Some("project".into());
        state.cuenv.presentation.rail =
            Some(crate::integrations::cuenv::RailColor(0x89, 0xb4, 0xfa));
        state
            .cuenv
            .presentation
            .terminal
            .terminal_transparency_percent = 40;

        let restarted = registry.restart_pane(pane, test_geometry()).unwrap();

        assert_eq!(registry.active_pane(), Some(pane));
        assert_eq!(registry.workspace.workspace().tabs().len(), 1);
        let state = &registry.sessions[&pane];
        assert_eq!(state.error, None);
        assert_eq!(state.generation, restarted.generation);
        assert_ne!(state.generation, old_generation);
        assert_eq!(
            state
                .cuenv
                .presentation
                .terminal
                .terminal_transparency_percent,
            40,
            "it keeps its look"
        );
        assert_eq!(
            state.cuenv.presentation.banner.as_deref(),
            Some("project"),
            "it stays in that project"
        );
        assert!(state.cuenv.presentation.rail.is_some());

        old_effects
            .lock()
            .unwrap()
            .push(ControlEvent::Title("stale".into()));
        assert!(
            !registry
                .apply_wake(pane, old_generation, Some(pane))
                .accepted,
            "a wake from the old session is ignored"
        );
        new_effects
            .lock()
            .unwrap()
            .push(ControlEvent::Title("fresh".into()));
        assert!(
            registry
                .apply_wake(pane, restarted.generation, Some(pane))
                .accepted
        );
        assert_eq!(registry.sessions[&pane].activity.title(), Some("fresh"));
    }

    #[test]
    fn a_restarted_shell_starts_where_the_broken_one_was_and_keeps_that_directorys_look() {
        let directory = std::env::temp_dir();
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (replacement, _) = fake_start('b', Vec::new(), None);
        let (factory, directories) = fake_factory_recording(vec![Ok(initial), Ok(replacement)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        state.cuenv.observe_cwd(directory.clone());
        state.cuenv.presentation.banner = Some("project".into());

        registry.restart_pane(pane, test_geometry()).unwrap();

        assert_eq!(
            directories.lock().unwrap().as_slice(),
            [None, Some(directory.clone())]
        );
        let state = registry.sessions.get_mut(&pane).unwrap();
        assert!(
            !state.cuenv.observe_cwd(directory),
            "the new shell reporting that directory is no news"
        );
        assert_eq!(state.cuenv.presentation.banner.as_deref(), Some("project"));

        state
            .cuenv
            .observe_cwd(std::path::PathBuf::from("/elsewhere"));
        assert_eq!(
            state.cuenv.presentation.banner, None,
            "somewhere else, the project's look goes"
        );
    }

    #[test]
    fn a_restart_from_a_directory_that_has_gone_starts_at_the_default() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (replacement, _) = fake_start('b', Vec::new(), None);
        let (factory, directories) = fake_factory_recording(vec![Ok(initial), Ok(replacement)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        registry
            .sessions
            .get_mut(&pane)
            .unwrap()
            .cuenv
            .observe_cwd(std::env::temp_dir().join("cuetty-no-such-directory"));

        registry.restart_pane(pane, test_geometry()).unwrap();

        assert_eq!(directories.lock().unwrap().as_slice(), [None, None]);
    }

    #[test]
    fn a_restart_that_cannot_start_a_shell_leaves_the_pane_as_it_was() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Err("no pty".into())]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let generation = registry.sessions[&pane].generation;
        registry.sessions.get_mut(&pane).unwrap().error = Some("pipe gone".into());

        let error = registry.restart_pane(pane, test_geometry()).err();

        assert_eq!(error, Some(RegistryError::Session("no pty".into())));
        let state = &registry.sessions[&pane];
        assert_eq!(state.generation, generation);
        assert_eq!(state.error.as_deref(), Some("pipe gone"));
    }

    #[test]
    fn a_window_that_opened_with_no_shell_can_still_start_one() {
        let (start, _) = fake_start('a', Vec::new(), None);
        let mut registry = SessionRegistry::empty(fake_factory(vec![Ok(start)]));
        registry.notice = Some("Could not start a shell: no pty".into());
        assert_eq!(registry.active_pane(), None);

        let (pane, _, _) = registry.start_tab(test_geometry()).unwrap();

        assert_eq!(registry.active_pane(), Some(pane));
        assert_eq!(registry.notice, None);
    }

    #[test]
    fn a_broken_pane_offers_no_tasks() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        state.cuenv.tasks = vec![runnable_task("build")];
        assert!(!state.task_catalogue().is_empty());
        state.error = Some("pipe gone".into());
        assert!(state.task_catalogue().is_empty());
    }

    #[test]
    fn only_a_fresh_plain_enter_takes_the_action_on_offer() {
        let plain = gpui::Modifiers::default();
        assert!(takes_offered_action(&key_press("enter", plain)));
        assert!(!takes_offered_action(&key_press("x", plain)));
        assert!(!takes_offered_action(&key_press(
            "enter",
            gpui::Modifiers {
                shift: true,
                ..plain
            }
        )));
        assert!(!takes_offered_action(&gpui::KeyDownEvent {
            is_held: true,
            ..key_press("enter", plain)
        }));
    }

    #[test]
    fn terminal_configuration_follows_the_active_tab() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, _) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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

        let (second_pane, _, _) = registry.start_tab(test_geometry()).unwrap();
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let (second_pane, _, _) = registry.start_tab(test_geometry()).unwrap();
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let _ = registry.start_tab(test_geometry()).unwrap();
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let (second_pane, generation, _) = registry.start_tab(test_geometry()).unwrap();

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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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
            cursor: None,
        };
        let pane = PaneState::new(
            SessionGeneration(1),
            Box::new(session),
            test_geometry().surface,
        );
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
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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

    /// A registry with two tabs, the second of them on screen and the first scripted
    /// to report `effects` on its next wake.
    fn background_tab_with(
        effects: Vec<ControlEvent>,
    ) -> (SessionRegistry, crate::workspace::PaneId, SessionGeneration) {
        let (first, _) = fake_start('z', effects, None);
        let (second, _) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(first), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        let generation = state.generation;
        // What the tab last showed, which its next wake's 'z' differs from.
        state.frame = frame_with('a');
        registry.start_tab(test_geometry()).unwrap();
        registry.note_active_pane();
        (registry, pane, generation)
    }

    #[test]
    fn a_background_tab_reports_new_output_and_a_bell_until_it_is_arrived_at() {
        let (mut registry, pane, generation) = background_tab_with(Vec::new());
        let active = registry.active_pane();
        registry.apply_wake(pane, generation, active);
        assert_eq!(
            registry.sessions[&pane].status.indicator(),
            Some(TabIndicator::Unread)
        );

        let (mut registry, pane, generation) = background_tab_with(vec![ControlEvent::Bell]);
        let active = registry.active_pane();
        registry.apply_wake(pane, generation, active);
        assert_eq!(
            registry.sessions[&pane].status.indicator(),
            Some(TabIndicator::Bell)
        );

        let tab = registry
            .workspace
            .workspace()
            .tabs()
            .iter()
            .find(|tab| tab.focused == pane)
            .map(|tab| tab.id)
            .unwrap();
        registry
            .workspace
            .apply(ShellAction::ActivateTab(tab))
            .unwrap();
        registry.note_active_pane();
        assert_eq!(registry.sessions[&pane].status.indicator(), None);
    }

    #[test]
    fn the_tab_on_screen_collects_neither_output_nor_a_bell() {
        let (initial, _) = fake_start('z', vec![ControlEvent::Bell], None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        let generation = state.generation;
        state.frame = frame_with('a');
        registry.apply_wake(pane, generation, Some(pane));
        assert_eq!(registry.sessions[&pane].status.indicator(), None);
    }

    #[test]
    fn the_tab_on_screen_reports_what_happens_while_its_window_is_in_the_background() {
        let (initial, _) = fake_start('z', vec![ControlEvent::Bell], None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        let generation = state.generation;
        state.frame = frame_with('a');
        registry.note_active_pane();

        registry.window_presence = WindowPresence::Inactive;
        registry.note_active_pane();
        registry.apply_wake(pane, generation, Some(pane));
        assert_eq!(
            registry.sessions[&pane].status.indicator(),
            Some(TabIndicator::Bell)
        );
        // Redraws while the user is still elsewhere are not them coming back.
        registry.note_active_pane();
        assert_eq!(
            registry.sessions[&pane].status.indicator(),
            Some(TabIndicator::Bell)
        );

        registry.window_presence = WindowPresence::Active;
        registry.note_active_pane();
        assert_eq!(registry.sessions[&pane].status.indicator(), None);
    }

    #[test]
    fn a_wake_that_changes_no_text_is_not_new_output() {
        let (mut registry, pane, generation) = background_tab_with(Vec::new());
        let unchanged = registry.sessions.get_mut(&pane).unwrap().session.frame();
        registry.sessions.get_mut(&pane).unwrap().frame = unchanged;
        let active = registry.active_pane();
        registry.apply_wake(pane, generation, active);
        assert_eq!(registry.sessions[&pane].status.indicator(), None);
    }

    #[test]
    fn output_arriving_under_a_scrolled_back_view_is_new_output() {
        let earlier = TerminalFrame {
            viewport_offset: Some(40),
            ..frame_with('a')
        };
        let later = TerminalFrame {
            viewport_offset: Some(41),
            ..frame_with('a')
        };
        assert!(shows_new_output(&earlier, &later));
        assert!(!shows_new_output(&earlier, &earlier));
    }

    #[test]
    fn a_launched_task_reports_running_then_finished_until_the_user_moves_on() {
        let (mut state, _) = bracketed_pane("nu");
        let now = std::time::Instant::now();
        state.sample_foreground(now);
        state
            .launch_task("cuenv task build\n", &runnable_task("build"))
            .unwrap();
        assert_eq!(state.status.indicator(), Some(TabIndicator::Running));

        // The task's program has come and gone, which the foreground only shows
        // after the grace period for a task that has yet to appear.
        state.sample_foreground(now + std::time::Duration::from_secs(60));
        assert_eq!(state.status.indicator(), Some(TabIndicator::Finished));

        state
            .input(pressed(KeyInput::Character('x'), InputModifiers::default()))
            .unwrap();
        assert_eq!(state.status.indicator(), None);
    }

    fn pane_with_a_task_awaiting_arguments() -> (PaneState, FakeForeground) {
        let session = FakeSession {
            frame: TerminalView::empty_frame(),
            sent: Arc::default(),
            foreground: Arc::new(Mutex::new("nu".to_owned())),
            effects: Arc::default(),
            close_error: None,
            cursor: None,
        };
        let foreground = Arc::clone(&session.foreground);
        let mut state = PaneState::new(
            SessionGeneration(1),
            Box::new(session),
            test_geometry().surface,
        );
        state.sample_foreground(std::time::Instant::now());
        let mut task = runnable_task("build");
        task.requires_parameters = true;
        state.launch_task("cuenv task build ", &task).unwrap();
        (state, foreground)
    }

    #[test]
    fn a_task_given_its_arguments_reports_running_from_the_enter_that_runs_it() {
        let (mut state, _) = pane_with_a_task_awaiting_arguments();
        let plain = InputModifiers::default();
        state
            .input(pressed(KeyInput::Character('x'), plain))
            .unwrap();
        assert_eq!(state.status.indicator(), None);
        state.input(pressed(KeyInput::Enter, plain)).unwrap();
        assert_eq!(state.status.indicator(), Some(TabIndicator::Running));
        assert_eq!(state.activity.task_name(), Some("build"));
    }

    #[test]
    fn a_staged_task_abandoned_or_overtaken_never_reports_running() {
        let plain = InputModifiers::default();
        let (mut abandoned, _) = pane_with_a_task_awaiting_arguments();
        abandoned
            .input(pressed(KeyInput::Character('c'), InputModifiers::CONTROL))
            .unwrap();
        abandoned.input(pressed(KeyInput::Enter, plain)).unwrap();
        assert_eq!(abandoned.status.indicator(), None);

        // Something else took the foreground, so this Enter is that program's.
        let (mut overtaken, foreground) = pane_with_a_task_awaiting_arguments();
        *foreground.lock().unwrap() = "vim".to_owned();
        overtaken.input(pressed(KeyInput::Enter, plain)).unwrap();
        assert_eq!(overtaken.status.indicator(), None);
    }

    #[test]
    fn a_task_left_waiting_for_arguments_reports_nothing() {
        let (mut state, _) = bracketed_pane("nu");
        state.sample_foreground(std::time::Instant::now());
        let mut task = runnable_task("build");
        task.requires_parameters = true;
        state.launch_task("cuenv task build", &task).unwrap();
        assert_eq!(state.status.indicator(), None);
    }

    fn wake_with(effects: Vec<ControlEvent>) -> (SessionRegistry, crate::workspace::PaneId, bool) {
        let (initial, _) = fake_start('z', effects, None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let generation = registry.sessions[&pane].generation;
        let outcome = registry.apply_wake(pane, generation, Some(pane));
        (registry, pane, outcome.should_close)
    }

    #[test]
    fn a_shell_that_exits_cleanly_or_unknowably_takes_its_tab_with_it() {
        for status in [Some(0), None] {
            let (registry, pane, should_close) =
                wake_with(vec![ControlEvent::ChildExited(status), ControlEvent::Close]);
            assert!(should_close, "status {status:?}");
            assert_eq!(registry.sessions[&pane].status.indicator(), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_fails_leaves_its_tab_and_its_output_until_it_is_closed() {
        let (mut registry, pane, should_close) = wake_with(vec![
            ControlEvent::ChildExited(Some(1 << 8)),
            ControlEvent::Close,
        ]);
        assert!(!should_close);
        assert_eq!(
            registry.sessions[&pane].status.indicator(),
            Some(TabIndicator::Failed)
        );
        assert_eq!(
            registry.sessions[&pane].frame.rows[0].cells[0]
                .text
                .as_deref(),
            Some("z")
        );
        registry.close_pane(pane).unwrap();
        assert!(registry.sessions.is_empty());
        assert!(registry.close_window_requested);
    }

    #[cfg(unix)]
    #[test]
    fn a_close_arriving_on_a_later_wake_still_leaves_a_failed_shells_tab() {
        let (initial, _, effects) = fake_start_in_foreground("zsh");
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let generation = registry.sessions[&pane].generation;
        effects
            .lock()
            .unwrap()
            .push(ControlEvent::ChildExited(Some(9)));
        registry.apply_wake(pane, generation, Some(pane));
        effects.lock().unwrap().push(ControlEvent::Close);
        assert!(
            !registry
                .apply_wake(pane, generation, Some(pane))
                .should_close
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_shell_shows_its_output_even_if_something_hit_the_closed_session_first() {
        let (initial, _, effects) = fake_start_in_foreground("zsh");
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let pane = registry.active_pane().unwrap();
        let state = registry.sessions.get_mut(&pane).unwrap();
        let generation = state.generation;
        state.error = Some("terminal session is closed or its child has exited".into());
        effects
            .lock()
            .unwrap()
            .push(ControlEvent::ChildExited(Some(1 << 8)));
        registry.apply_wake(pane, generation, Some(pane));
        assert_eq!(registry.sessions[&pane].error, None);
    }

    #[test]
    fn only_a_failed_shell_has_its_grid_refit_without_being_told() {
        let (mut state, _) = bracketed_pane("zsh");
        let cell = CellSize {
            width: 8,
            height: 16,
        };
        let rows = state.session.frame().rows.len();
        state.fit_grid(800, 0, cell);
        assert_eq!(state.session.frame().rows.len(), rows);
        state.status.apply(StatusEvent::SessionFailed(
            crate::terminal::tab_status::ExitFailure::Code(1),
        ));
        state.fit_grid(800, 0, cell);
        assert_eq!(state.session.frame().rows.len(), 0);
        assert_eq!(state.error, None);
    }

    #[test]
    fn a_failed_shells_palette_chord_goes_back_to_selecting_tabs() {
        let tabs = shell_with_tabs(1);
        let held = tab_number_modifiers_for_test();
        let (mut state, _) = bracketed_pane("zsh");
        state.cuenv.tasks = vec![runnable_task("build")];
        state.cuenv.presentation.keybindings.palette = KeyChord {
            key: "1".into(),
            modifiers: tab_number_modifiers(),
        };
        let route = |state: &PaneState| {
            route_chord(
                "1",
                held,
                &ChordTargets {
                    palette: state.palette_chord(),
                    tabs: &tabs,
                },
            )
        };
        assert_eq!(route(&state), ChordRoute::OpenPalette);
        state.status.apply(StatusEvent::SessionFailed(
            crate::terminal::tab_status::ExitFailure::Code(1),
        ));
        assert_eq!(
            route(&state),
            ChordRoute::SelectTab(tabs.active_tab().unwrap()),
        );
    }

    #[test]
    fn a_failed_shell_offers_no_tasks() {
        let (mut state, _) = bracketed_pane("zsh");
        state.cuenv.tasks = vec![runnable_task("build")];
        assert_eq!(state.task_catalogue().len(), 1);
        state.status.apply(StatusEvent::SessionFailed(
            crate::terminal::tab_status::ExitFailure::Code(1),
        ));
        assert!(state.task_catalogue().is_empty());
    }

    #[test]
    fn enter_or_ctrl_d_alone_dismiss_an_exited_tab() {
        let plain = gpui::Modifiers::default();
        let control = gpui::Modifiers {
            control: true,
            ..plain
        };
        assert!(closes_exited_tab(&key_press("enter", plain)));
        assert!(closes_exited_tab(&key_press("d", control)));
        assert!(!closes_exited_tab(&key_press("d", plain)));
        assert!(!closes_exited_tab(&key_press("enter", control)));
        assert!(!closes_exited_tab(&key_press("x", plain)));
        let shifted = gpui::Modifiers {
            shift: true,
            ..control
        };
        assert!(!closes_exited_tab(&key_press("d", shifted)));
    }

    #[test]
    fn an_inactive_window_fades_colours_towards_the_background_on_either_theme() {
        let (text, dark, light) = (Rgb(205, 214, 244), Rgb(30, 30, 46), Rgb(239, 241, 245));
        assert_eq!(window_ink(text, dark, WindowPresence::Active), text);
        for background in [dark, light] {
            let faded = window_ink(text, background, WindowPresence::Inactive);
            assert!(contrast_ratio(faded, background) < contrast_ratio(text, background));
        }
        assert_eq!(window_ink(dark, dark, WindowPresence::Inactive), dark);
    }

    #[test]
    fn a_key_still_held_from_before_the_shell_died_leaves_its_tab_open() {
        let control = gpui::Modifiers {
            control: true,
            ..gpui::Modifiers::default()
        };
        for press in [
            key_press("enter", gpui::Modifiers::default()),
            key_press("d", control),
        ] {
            let repeat = gpui::KeyDownEvent {
                is_held: true,
                ..press
            };
            assert!(!closes_exited_tab(&repeat));
        }
    }

    #[test]
    fn a_new_tab_is_measured_without_the_exit_notice_its_sibling_gives_up_room_to() {
        let (mut state, _) = bracketed_pane("zsh");
        let measured = size(px(800.0), px(600.0));
        *state.measured_bounds.lock().unwrap() = measured;
        assert_eq!(state.surface_for_sibling(), measured);
        state.status.apply(StatusEvent::SessionFailed(
            crate::terminal::tab_status::ExitFailure::Code(1),
        ));
        let surface = state.surface_for_sibling();
        assert_eq!(surface.width, measured.width);
        assert!(surface.height > measured.height);
    }

    #[test]
    fn close_wake_samples_the_final_frame_before_requesting_pane_removal() {
        let (initial, _) = fake_start('z', vec![ControlEvent::Close], None);
        let factory = fake_factory(vec![Ok(initial)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
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
        // The UI may now remove the pane; this is an ordering guarantee.
    }

    #[test]
    fn interaction_state_is_owned_by_the_tab_that_owns_the_session() {
        let (initial, _) = fake_start('a', Vec::new(), None);
        let (second, _) = fake_start('b', Vec::new(), None);
        let factory = fake_factory(vec![Ok(initial), Ok(second)]);
        let (mut registry, _) = SessionRegistry::start(factory, test_geometry()).unwrap();
        let first_pane = registry.active_pane().unwrap();
        let (second_pane, _, _) = registry.start_tab(test_geometry()).unwrap();
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
