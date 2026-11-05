//! Deterministic literal search over a replaceable transcript source.

use super::model::CellOccupancy;
use super::selection::{LogicalPosition, TextSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Literal,
    Regex,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub wrap: bool,
    pub mode: SearchMode,
}
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            wrap: true,
            mode: SearchMode::Literal,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Physical cell coordinates in a half-open range `[start, end)`.
    /// `end` is the first cell after the match and may be on a later
    /// soft-wrapped physical line.
    pub start: LogicalPosition,
    pub end: LogicalPosition,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    UnsupportedMode(SearchMode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralSearch {
    query: String,
    options: SearchOptions,
    matches: Vec<SearchMatch>,
    current: Option<usize>,
    /// The text of each line as last searched, to tell how far output has moved it since.
    lines: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct PhysicalSpan {
    start: LogicalPosition,
    end: LogicalPosition,
}

impl LiteralSearch {
    pub fn new(query: impl Into<String>, options: SearchOptions) -> Self {
        Self {
            query: query.into(),
            options,
            matches: Vec::new(),
            current: None,
            lines: Vec::new(),
        }
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn options(&self) -> SearchOptions {
        self.options
    }
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }
    /// The match navigation is on, if it has reached one.
    pub fn current_match(&self) -> Option<SearchMatch> {
        self.current
            .and_then(|index| self.matches.get(index).copied())
    }
    /// Where the current match comes among all of them, counting from zero.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }
    /// Searches the source again as it has changed, keeping navigation on the match it
    /// was on as output scrolls it up the screen, or else on the nearest after it, since
    /// what it was on may have gone.
    pub fn refresh<S: TextSource>(&mut self, source: &S) -> Result<(), SearchError> {
        let held = self.current_match();
        let searched = std::mem::take(&mut self.lines);
        self.rebuild(source)?;
        let scrolled = lines_scrolled(&searched, &self.lines);
        self.current = held.and_then(|held| {
            let wanted = held
                .start
                .line
                .checked_sub(scrolled)
                .map_or(LogicalPosition { line: 0, column: 0 }, |line| {
                    LogicalPosition { line, ..held.start }
                });
            self.matches
                .iter()
                .position(|candidate| candidate.start >= wanted)
                .or_else(|| self.matches.len().checked_sub(1))
        });
        Ok(())
    }
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.current = None;
    }
    pub fn rebuild<S: TextSource>(&mut self, source: &S) -> Result<(), SearchError> {
        if self.options.mode != SearchMode::Literal {
            return Err(SearchError::UnsupportedMode(self.options.mode));
        }
        self.matches.clear();
        self.current = None;
        self.lines.clear();
        if self.query.is_empty() {
            return Ok(());
        }
        self.lines = (0..source.line_count())
            .map(|line| line_text(source, line))
            .collect();
        let needle = self.folded_query();
        let mut first_line = 0;
        while first_line < source.line_count() {
            let (hay, hay_sources, next_line) = self.logical_row(source, first_line);
            let mut from = 0;
            while needle.len() <= hay.len() && from + needle.len() <= hay.len() {
                let Some(at) = hay[from..]
                    .windows(needle.len())
                    .position(|window| window == needle.as_slice())
                else {
                    break;
                };
                let start = from + at;
                let end = start + needle.len();
                let start_span = hay_sources[start];
                let end_span = hay_sources[end - 1];
                self.matches.push(SearchMatch {
                    start: start_span.start,
                    end: end_span.end,
                });
                from = start + needle.len().max(1);
            }
            first_line = next_line;
        }
        Ok(())
    }

    fn folded_query(&self) -> Vec<char> {
        self.query
            .chars()
            .flat_map(|character| self.fold(character))
            .collect()
    }

    fn logical_row<S: TextSource>(
        &self,
        source: &S,
        first_line: usize,
    ) -> (Vec<char>, Vec<PhysicalSpan>, usize) {
        let mut hay = Vec::new();
        let mut hay_sources = Vec::new();
        let mut line = first_line;
        loop {
            for (column, cell) in source.line_cells(line).iter().enumerate() {
                let Some(text) = &cell.text else {
                    continue;
                };
                if !matches!(cell.occupancy, CellOccupancy::Narrow | CellOccupancy::Wide) {
                    continue;
                }
                let span = PhysicalSpan {
                    start: LogicalPosition { line, column },
                    end: LogicalPosition {
                        line,
                        column: column
                            + if cell.occupancy == CellOccupancy::Wide {
                                2
                            } else {
                                1
                            },
                    },
                };
                // Case folding occurs per Unicode scalar value. A fold may expand to
                // multiple scalars; each maps to the whole originating cell, because
                // terminal selection addresses cells rather than grapheme fragments.
                for folded_character in text.chars().flat_map(|character| self.fold(character)) {
                    hay.push(folded_character);
                    hay_sources.push(span);
                }
            }
            line += 1;
            if line == source.line_count() || !source.line_soft_wrapped(line - 1) {
                return (hay, hay_sources, line);
            }
        }
    }

    fn fold(&self, character: char) -> std::vec::IntoIter<char> {
        if self.options.case_sensitive {
            vec![character].into_iter()
        } else {
            character.to_lowercase().collect::<Vec<_>>().into_iter()
        }
    }
    pub fn next_match(&mut self) -> Option<SearchMatch> {
        self.move_by(1)
    }
    pub fn previous_match(&mut self) -> Option<SearchMatch> {
        self.move_by(-1)
    }
    fn move_by(&mut self, delta: i32) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        let candidate = match self.current {
            None => {
                if delta > 0 {
                    0
                } else {
                    self.matches.len() - 1
                }
            }
            Some(i) => {
                let n = i as i32 + delta;
                if n < 0 {
                    if self.options.wrap {
                        self.matches.len() - 1
                    } else {
                        return None;
                    }
                } else if n >= self.matches.len() as i32 {
                    if self.options.wrap {
                        0
                    } else {
                        return None;
                    }
                } else {
                    n as usize
                }
            }
        };
        self.current = Some(candidate);
        Some(self.matches[candidate])
    }
}

/// A line's text, without the blank cells after it.
fn line_text<S: TextSource>(source: &S, line: usize) -> String {
    let text: String = source
        .line_cells(line)
        .iter()
        .flat_map(|cell| cell.text.iter())
        .flat_map(|text| text.chars())
        .collect();
    text.trim_end().to_owned()
}

/// How many lines output has moved the text up between two samples of a screen: whichever
/// distance leaves the most lines of text in agreement, and the shortest of those. Every
/// screen has blank lines, so they say nothing. The line being written at the bottom
/// may have changed as it scrolled, so this asks for the most agreement and not for all.
fn lines_scrolled(before: &[String], after: &[String]) -> usize {
    (0..before.len())
        .max_by_key(|&distance| {
            let agreeing = before[distance..]
                .iter()
                .zip(after)
                .filter(|(was, is)| was == is && !was.is_empty())
                .count();
            (agreeing, std::cmp::Reverse(distance))
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::TerminalCell;
    use crate::terminal::selection::Selection;
    struct S(Vec<Vec<TerminalCell>>);
    impl TextSource for S {
        fn line_count(&self) -> usize {
            self.0.len()
        }
        fn line_cells(&self, l: usize) -> &[TerminalCell] {
            &self.0[l]
        }
        fn line_soft_wrapped(&self, _: usize) -> bool {
            false
        }
    }
    fn source() -> S {
        S(vec![
            "One one".chars().map(TerminalCell::narrow).collect(),
            "one".chars().map(TerminalCell::narrow).collect(),
        ])
    }

    struct WrappedS(Vec<(Vec<TerminalCell>, bool)>);
    impl TextSource for WrappedS {
        fn line_count(&self) -> usize {
            self.0.len()
        }
        fn line_cells(&self, line: usize) -> &[TerminalCell] {
            &self.0[line].0
        }
        fn line_soft_wrapped(&self, line: usize) -> bool {
            self.0[line].1
        }
    }
    #[test]
    fn navigation_and_case_options() {
        let s = source();
        let mut q = LiteralSearch::new(
            "one",
            SearchOptions {
                case_sensitive: false,
                ..Default::default()
            },
        );
        q.rebuild(&s).unwrap();
        assert_eq!(q.matches().len(), 3);
        assert_eq!(q.next_match().unwrap().start.column, 0);
        assert_eq!(q.previous_match().unwrap().start.line, 1);
    }
    #[test]
    fn a_refresh_stays_on_the_match_or_moves_to_the_nearest_after_it() {
        let mut search = LiteralSearch::new("one", SearchOptions::default());
        search.rebuild(&lines(&["one", "one", "one"])).unwrap();
        search.next_match();
        search.next_match();
        assert_eq!(search.current_index(), Some(1));

        search.refresh(&lines(&["one", "one", "one"])).unwrap();
        assert_eq!(search.current_index(), Some(1), "unchanged text keeps it");

        search.refresh(&lines(&["one", "two", "one"])).unwrap();
        assert_eq!(
            search.current_match().map(|matched| matched.start.line),
            Some(2),
            "the next one takes over from one that went"
        );

        search.refresh(&lines(&["one", "two", "two"])).unwrap();
        assert_eq!(
            search.current_match().map(|matched| matched.start.line),
            Some(0),
            "with nothing after it, the last one left takes over"
        );

        search.refresh(&lines(&["two"])).unwrap();
        assert_eq!(search.current_match(), None);
    }

    fn lines(text: &[&str]) -> S {
        S(text
            .iter()
            .map(|line| line.chars().map(TerminalCell::narrow).collect())
            .collect())
    }

    #[test]
    fn a_refresh_keeps_to_the_match_as_output_scrolls_it_up_the_screen() {
        let mut search = LiteralSearch::new("one", SearchOptions::default());
        search
            .rebuild(&lines(&["top", "one", "mid", "one"]))
            .unwrap();
        search.next_match();
        assert_eq!(search.current_index(), Some(0));

        search
            .refresh(&lines(&["one", "mid", "one", "new"]))
            .unwrap();
        assert_eq!(
            search.current_match().map(|matched| matched.start.line),
            Some(0),
            "it moved up with its text, and not onto the other match"
        );
        assert_eq!(search.current_index(), Some(0));
    }

    #[test]
    fn a_refresh_keeps_to_the_match_when_its_screen_has_a_line_still_being_written() {
        let mut search = LiteralSearch::new("one", SearchOptions::default());
        search
            .rebuild(&lines(&["top", "one", "mid", "one", "$ par"]))
            .unwrap();
        search.next_match();
        search.next_match();
        assert_eq!(search.current_index(), Some(1));

        search
            .refresh(&lines(&["one", "mid", "one", "$ partial", "new"]))
            .unwrap();
        assert_eq!(
            search.current_match().map(|matched| matched.start.line),
            Some(2)
        );
    }

    #[test]
    fn a_match_scrolled_off_the_screen_hands_over_to_the_first_one_left() {
        let mut search = LiteralSearch::new("one", SearchOptions::default());
        search
            .rebuild(&lines(&["one", "mid", "low", "one"]))
            .unwrap();
        search.next_match();

        search
            .refresh(&lines(&["mid", "low", "one", "new"]))
            .unwrap();
        assert_eq!(
            search.current_match().map(|matched| matched.start.line),
            Some(2)
        );
    }

    #[test]
    fn blank_lines_are_no_evidence_that_text_has_moved() {
        let blank = String::new();
        let was = [blank.clone(), blank.clone(), "text".to_owned()];
        assert_eq!(lines_scrolled(&was, &was), 0);
        assert_eq!(
            lines_scrolled(&[blank.clone(), blank.clone()], &[blank.clone(), blank]),
            0
        );
        assert_eq!(lines_scrolled(&[], &[]), 0);
    }

    #[test]
    fn a_refresh_does_not_start_navigation_that_had_not_begun() {
        let source = source();
        let mut search = LiteralSearch::new("one", SearchOptions::default());
        search.refresh(&source).unwrap();
        assert_eq!(search.matches().len(), 2);
        assert_eq!(search.current_match(), None);
    }

    #[test]
    fn no_wrap_stops_at_edges() {
        let s = source();
        let mut q = LiteralSearch::new(
            "one",
            SearchOptions {
                case_sensitive: false,
                wrap: false,
                ..Default::default()
            },
        );
        q.rebuild(&s).unwrap();
        q.next_match();
        q.next_match();
        q.next_match();
        assert!(q.next_match().is_none());
    }
    #[test]
    fn regex_is_explicitly_unsupported() {
        let s = source();
        let mut q = LiteralSearch::new(
            ".",
            SearchOptions {
                mode: SearchMode::Regex,
                ..Default::default()
            },
        );
        assert_eq!(
            q.rebuild(&s),
            Err(SearchError::UnsupportedMode(SearchMode::Regex))
        );
    }

    #[test]
    fn match_coordinates_preserve_wide_cell_physical_columns() {
        let s = S(vec![vec![
            TerminalCell::narrow('a'),
            TerminalCell::wide('界'),
            TerminalCell::trailing_spacer(),
            TerminalCell::narrow('b'),
        ]]);
        let mut wide = LiteralSearch::new("界", SearchOptions::default());
        wide.rebuild(&s).unwrap();
        assert_eq!(
            wide.matches(),
            &[SearchMatch {
                start: LogicalPosition { line: 0, column: 1 },
                end: LogicalPosition { line: 0, column: 3 },
            }]
        );

        let mut trailing = LiteralSearch::new("b", SearchOptions::default());
        trailing.rebuild(&s).unwrap();
        assert_eq!(trailing.matches()[0].start.column, 3);
    }

    #[test]
    fn combining_cluster_search_and_copy_keep_physical_cell_coordinates() {
        let cluster = TerminalCell {
            text: Some("e\u{301}".into()),
            ..TerminalCell::narrow('e')
        };
        let source = S(vec![vec![cluster, TerminalCell::narrow('x')]]);
        let mut search = LiteralSearch::new("e\u{301}x", SearchOptions::default());
        search.rebuild(&source).unwrap();
        assert_eq!(search.matches().len(), 1);
        let matched = search.matches()[0];
        assert_eq!(matched.start, LogicalPosition { line: 0, column: 0 });
        assert_eq!(matched.end, LogicalPosition { line: 0, column: 2 });
        assert_eq!(
            Selection::new(matched.start, matched.end).copy(&source),
            "e\u{301}x"
        );

        // A match within a cluster selects the complete originating cell.
        search.set_query("\u{301}");
        search.rebuild(&source).unwrap();
        let matched = search.matches()[0];
        assert_eq!(matched.end.column, 1);
        assert_eq!(
            Selection::new(matched.start, matched.end).copy(&source),
            "e\u{301}"
        );
    }

    #[test]
    fn literal_match_can_cross_a_soft_wrap_and_copy_exactly() {
        let s = WrappedS(vec![
            ("hel".chars().map(TerminalCell::narrow).collect(), true),
            ("lo!".chars().map(TerminalCell::narrow).collect(), false),
        ]);
        let mut search = LiteralSearch::new("hello", SearchOptions::default());
        search.rebuild(&s).unwrap();
        let matched = search.matches()[0];
        assert_eq!(
            matched,
            SearchMatch {
                start: LogicalPosition { line: 0, column: 0 },
                end: LogicalPosition { line: 1, column: 2 },
            }
        );
        assert_eq!(Selection::new(matched.start, matched.end).copy(&s), "hello");
    }

    #[test]
    fn hard_lines_are_separate_logical_search_rows() {
        let s = WrappedS(vec![
            ("hel".chars().map(TerminalCell::narrow).collect(), false),
            ("lo".chars().map(TerminalCell::narrow).collect(), false),
        ]);
        let mut search = LiteralSearch::new("hello", SearchOptions::default());
        search.rebuild(&s).unwrap();
        assert!(search.matches().is_empty());
    }

    #[test]
    fn case_insensitive_search_maps_scalar_expansions_to_the_source_cell() {
        let s = S(vec![vec![TerminalCell::narrow('\u{0130}')]]);
        let mut search = LiteralSearch::new(
            "i\u{307}",
            SearchOptions {
                case_sensitive: false,
                ..Default::default()
            },
        );
        search.rebuild(&s).unwrap();
        assert_eq!(
            search.matches(),
            &[SearchMatch {
                start: LogicalPosition { line: 0, column: 0 },
                end: LogicalPosition { line: 0, column: 1 },
            }]
        );
    }
}
