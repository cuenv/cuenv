use gpui::{Bounds, Pixels, point, px};

#[derive(Clone, Copy)]
pub(super) struct TerminalGridGeometry {
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) cell_width: f32,
    pub(super) cell_height: f32,
}

pub(super) fn window_position_to_local(
    last_bounds: Option<Bounds<Pixels>>,
    position: gpui::Point<Pixels>,
) -> gpui::Point<Pixels> {
    let origin = last_bounds
        .map(|bounds| bounds.origin)
        .unwrap_or_else(|| point(px(0.0), px(0.0)));
    point(position.x - origin.x, position.y - origin.y)
}

pub(super) fn row_index_for_local_position(
    rows: usize,
    cell_height: f32,
    position: gpui::Point<Pixels>,
) -> usize {
    let y = f32::from(position.y);
    let mut row_index = (y / cell_height).floor() as i32;
    if row_index < 0 {
        row_index = 0;
    }
    if row_index >= rows as i32 {
        row_index = rows as i32 - 1;
    }
    row_index as usize
}

pub(super) fn local_position_to_cell(
    geometry: TerminalGridGeometry,
    position: gpui::Point<Pixels>,
) -> (u16, u16) {
    let x = f32::from(position.x);
    let y = f32::from(position.y);

    let mut col = (x / geometry.cell_width).floor() as i32 + 1;
    let mut row = (y / geometry.cell_height).floor() as i32 + 1;

    if col < 1 {
        col = 1;
    }
    if row < 1 {
        row = 1;
    }
    if col > geometry.cols as i32 {
        col = geometry.cols as i32;
    }
    if row > geometry.rows as i32 {
        row = geometry.rows as i32;
    }

    (col as u16, row as u16)
}

pub(crate) fn byte_index_for_column_in_line(line: &str, col: u16) -> usize {
    use unicode_width::UnicodeWidthChar as _;

    let col = col.max(1) as usize;
    if col == 1 {
        return 0;
    }

    let mut current_col = 1usize;
    for (byte_index, ch) in line.char_indices() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            continue;
        }

        if current_col == col {
            return byte_index;
        }

        let next_col = current_col.saturating_add(width);
        if col < next_col {
            return byte_index;
        }

        current_col = next_col;
    }

    line.len()
}

pub(crate) fn cell_metrics(window: &mut gpui::Window, font: &gpui::Font) -> Option<(f32, f32)> {
    let mut style = window.text_style();
    style.font_family = font.family.clone();
    style.font_features = crate::default_terminal_font_features();
    style.font_fallbacks = font.fallbacks.clone();

    let rem_size = window.rem_size();
    let font_size = style.font_size.to_pixels(rem_size);
    let line_height = style.line_height.to_pixels(style.font_size, rem_size);

    let run = style.to_run(1);
    let lines = window
        .text_system()
        .shape_text(
            gpui::SharedString::from("M"),
            font_size,
            &[run],
            None,
            Some(1),
        )
        .ok()?;
    let line = lines.first()?;

    let cell_width = f32::from(line.width()).max(1.0);
    let cell_height = f32::from(line_height).max(1.0);
    Some((cell_width, cell_height))
}

#[cfg(test)]
mod tests {
    use super::{
        TerminalGridGeometry, local_position_to_cell, row_index_for_local_position,
        window_position_to_local,
    };

    #[test]
    fn mouse_position_to_local_accounts_for_bounds_origin() {
        let bounds = Some(gpui::Bounds::new(
            gpui::point(gpui::px(100.0), gpui::px(20.0)),
            gpui::size(gpui::px(200.0), gpui::px(80.0)),
        ));

        let local = window_position_to_local(bounds, gpui::point(gpui::px(110.0), gpui::px(30.0)));
        assert_eq!(local, gpui::point(gpui::px(10.0), gpui::px(10.0)));
    }

    #[test]
    fn local_selection_coordinates_ignore_shell_chrome_origin() {
        let bounds = Some(gpui::Bounds::new(
            gpui::point(gpui::px(220.0), gpui::px(40.0)),
            gpui::size(gpui::px(800.0), gpui::px(600.0)),
        ));
        let local = window_position_to_local(bounds, gpui::point(gpui::px(244.0), gpui::px(70.0)));
        let geometry = TerminalGridGeometry {
            cols: 100,
            rows: 30,
            cell_width: 8.0,
            cell_height: 15.0,
        };

        assert_eq!(row_index_for_local_position(30, 15.0, local), 2);
        assert_eq!(local_position_to_cell(geometry, local), (4, 3));
    }
}
