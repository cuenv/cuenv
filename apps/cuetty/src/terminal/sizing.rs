#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub pixels_width: u32,
    pub pixels_height: u32,
}

pub fn terminal_size(width: u32, height: u32, cell: CellSize) -> TerminalSize {
    let width = width.max(1);
    let height = height.max(1);
    let cell_width = cell.width.max(1);
    let cell_height = cell.height.max(1);
    let cols = (width / cell_width).clamp(1, u16::MAX as u32) as u16;
    let rows = (height / cell_height).clamp(1, u16::MAX as u32) as u16;
    TerminalSize {
        cols,
        rows,
        pixels_width: cols as u32 * cell_width,
        pixels_height: rows as u32 * cell_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_to_one_cell() {
        assert_eq!(
            terminal_size(
                0,
                0,
                CellSize {
                    width: 8,
                    height: 16
                }
            ),
            TerminalSize {
                cols: 1,
                rows: 1,
                pixels_width: 8,
                pixels_height: 16
            }
        );
    }

    #[test]
    fn uses_physical_dimensions_without_duplicate_rounding() {
        assert_eq!(
            terminal_size(
                801,
                321,
                CellSize {
                    width: 8,
                    height: 16
                }
            ),
            TerminalSize {
                cols: 100,
                rows: 20,
                pixels_width: 800,
                pixels_height: 320
            }
        );
    }
}
