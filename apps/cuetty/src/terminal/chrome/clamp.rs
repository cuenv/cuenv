//! Cutting text to a number of lines before GPUI lays it out.

/// How much text a place in the chrome has room for. GPUI's `line_clamp` only limits soft
/// wraps, so each newline in a message would otherwise add a line it never counts.
#[derive(Clone, Copy, Debug)]
pub(super) struct TextBudget {
    /// Lines there is room for, counting wrapped and newline-separated ones alike.
    pub(super) lines: usize,
    /// Characters that sit comfortably on one of them.
    pub(super) line_chars: usize,
}

impl TextBudget {
    /// Lines `line` takes up once wrapped. An estimate from its length: wrapping happens
    /// on word boundaries in a proportional font.
    pub(super) fn line_cost(self, line: &str) -> usize {
        line.chars().count().div_ceil(self.line_chars).max(1)
    }

    /// Cuts `label` to the budget, ending in an ellipsis when it has to cut.
    pub(super) fn clamp(self, label: &str) -> String {
        let label = label.trim_end();
        let lines: usize = label.lines().map(|line| self.line_cost(line)).sum();
        if lines <= self.lines {
            return label.to_string();
        }
        label
            .lines()
            .scan(self.lines.saturating_sub(1), |remaining, line| {
                let kept = self.line_cost(line).min(*remaining);
                *remaining -= kept;
                (kept > 0).then(|| {
                    line.chars()
                        .take(kept * self.line_chars)
                        .collect::<String>()
                })
            })
            .chain(std::iter::once("…".to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
