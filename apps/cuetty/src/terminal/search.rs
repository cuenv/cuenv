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
        if self.query.is_empty() {
            return Ok(());
        }
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
                let Some(character) = cell.codepoint else {
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
                for folded_character in self.fold(character) {
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
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<SearchMatch> {
        self.move_by(1)
    }
    pub fn previous(&mut self) -> Option<SearchMatch> {
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
        assert_eq!(q.next().unwrap().start.column, 0);
        assert_eq!(q.previous().unwrap().start.line, 1);
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
        q.next();
        q.next();
        q.next();
        assert!(q.next().is_none());
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
