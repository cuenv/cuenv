//! Backend-neutral transcript storage and viewport bookkeeping.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackLine {
    pub text: String,
    /// True when the next physical row is a continuation of this line.
    pub soft_wrapped: bool,
}

impl ScrollbackLine {
    pub fn new(text: impl Into<String>, soft_wrapped: bool) -> Self {
        Self {
            text: text.into(),
            soft_wrapped,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackBuffer {
    lines: Vec<ScrollbackLine>,
    capacity: usize,
    viewport_rows: usize,
    /// Number of rows upward from the bottom. Zero means the viewport is at
    /// the bottom and follows newly appended output.
    viewport_offset: usize,
    follow_output: bool,
}

impl ScrollbackBuffer {
    pub fn new(capacity: usize, viewport_rows: usize) -> Self {
        Self {
            lines: Vec::new(),
            capacity,
            viewport_rows: viewport_rows.max(1),
            viewport_offset: 0,
            follow_output: true,
        }
    }

    pub fn lines(&self) -> &[ScrollbackLine] {
        &self.lines
    }
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    pub fn viewport_rows(&self) -> usize {
        self.viewport_rows
    }
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }
    pub fn follows_output(&self) -> bool {
        self.follow_output
    }

    pub fn append(&mut self, line: ScrollbackLine) {
        if self.capacity == 0 {
            return;
        }
        self.lines.push(line);
        let excess = self.lines.len().saturating_sub(self.capacity);
        if excess != 0 {
            self.lines.drain(..excess);
        }
        if self.follow_output {
            self.scroll_to_bottom();
        } else {
            self.clamp_offset();
        }
    }

    pub fn append_text(&mut self, text: impl Into<String>, soft_wrapped: bool) {
        self.append(ScrollbackLine::new(text, soft_wrapped));
    }

    pub fn set_follow_output(&mut self, follow: bool) {
        self.follow_output = follow;
        if follow {
            self.scroll_to_bottom();
        }
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.follow_output = false;
        self.viewport_offset = self.viewport_offset.saturating_add(rows);
        self.clamp_offset();
    }
    pub fn scroll_down(&mut self, rows: usize) {
        self.viewport_offset = self.viewport_offset.saturating_sub(rows);
        if self.viewport_offset == 0 {
            self.follow_output = true;
        }
    }
    pub fn scroll_to_top(&mut self) {
        self.follow_output = false;
        self.viewport_offset = self.max_offset();
    }
    pub fn scroll_to_bottom(&mut self) {
        self.viewport_offset = 0;
        self.follow_output = true;
    }

    /// Resize while keeping the same logical top line under the viewport.
    /// This makes resize/reflow behavior deterministic for callers that map
    /// logical lines to physical rows independently.
    pub fn resize_viewport(&mut self, rows: usize) {
        let old_top = self.top_line();
        self.viewport_rows = rows.max(1);
        if self.follow_output {
            self.scroll_to_bottom();
        } else {
            self.viewport_offset = self
                .lines
                .len()
                .saturating_sub(self.viewport_rows)
                .saturating_sub(old_top);
            self.clamp_offset();
        }
    }

    pub fn top_line(&self) -> usize {
        self.lines
            .len()
            .saturating_sub(self.viewport_rows)
            .saturating_sub(self.viewport_offset)
    }
    pub fn visible_lines(&self) -> &[ScrollbackLine] {
        let start = self.top_line();
        let end = (start + self.viewport_rows).min(self.lines.len());
        &self.lines[start..end]
    }

    fn max_offset(&self) -> usize {
        self.lines.len().saturating_sub(self.viewport_rows)
    }
    fn clamp_offset(&mut self) {
        self.viewport_offset = self.viewport_offset.min(self.max_offset());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_follow_output_are_stable() {
        let mut b = ScrollbackBuffer::new(3, 2);
        for n in 0..5 {
            b.append_text(n.to_string(), false);
        }
        assert_eq!(
            b.lines()
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>(),
            vec!["2", "3", "4"]
        );
        b.scroll_up(99);
        assert_eq!(b.top_line(), 0);
        assert!(!b.follows_output());
        b.append_text("5", false);
        assert_eq!(b.top_line(), 0);
        b.scroll_to_bottom();
        b.append_text("6", false);
        assert_eq!(b.visible_lines()[1].text, "6");
    }
    #[test]
    fn resize_preserves_top_line_when_not_following() {
        let mut b = ScrollbackBuffer::new(20, 2);
        for n in 0..8 {
            b.append_text(n.to_string(), false);
        }
        b.scroll_up(2);
        let top = b.top_line();
        b.resize_viewport(4);
        assert_eq!(b.top_line(), top);
    }
}
