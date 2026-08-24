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

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportLayout {
    pub width: u32,
    pub height: u32,
    pub title_height: u32,
    pub inset: u32,
}

#[allow(dead_code)]
impl ViewportLayout {
    pub fn terminal_size(self, cell: CellSize) -> TerminalSize {
        let horizontal = self.inset.saturating_mul(2);
        let vertical = self
            .title_height
            .saturating_add(self.inset.saturating_mul(2));
        terminal_size(
            self.width.saturating_sub(horizontal),
            self.height.saturating_sub(vertical),
            cell,
        )
    }
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

    #[test]
    fn subtracts_title_and_inset_before_deriving_grid() {
        let layout = ViewportLayout {
            width: 816,
            height: 417,
            title_height: 33,
            inset: 16,
        };
        assert_eq!(
            layout.terminal_size(CellSize {
                width: 8,
                height: 16
            }),
            TerminalSize {
                cols: 98,
                rows: 22,
                pixels_width: 784,
                pixels_height: 352,
            }
        );
    }

    #[test]
    fn chrome_only_changes_do_not_change_terminal_size() {
        let cell = CellSize {
            width: 8,
            height: 16,
        };
        let first = ViewportLayout {
            width: 816,
            height: 417,
            title_height: 33,
            inset: 16,
        }
        .terminal_size(cell);
        let second = ViewportLayout {
            width: 816,
            height: 424,
            title_height: 40,
            inset: 16,
        }
        .terminal_size(cell);
        assert_eq!(first, second);
    }
}
