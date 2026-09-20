//! M0 public-core contracts. These tests deliberately do not launch GPUI or a
//! PTY and do not stand in for host/adapter or Apple Silicon smoke coverage.

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::pos::{Column, Line, Pos};
use rio_vt::crosswords::{Crosswords, CrosswordsSize, Mode};
use rio_vt::event::{VoidListener, WindowId};
use rio_vt::performer::handler::Processor;

fn terminal() -> Crosswords<VoidListener> {
    Crosswords::new(
        CrosswordsSize::new(20, 4),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        100,
    )
}

#[test]
fn public_core_preserves_output_when_resized() {
    let mut term = terminal();
    Processor::default().advance(&mut term, b"M0-OUTPUT");
    term.resize(CrosswordsSize::new(12, 6));
    assert_eq!(term.columns(), 12);
    assert_eq!(term.screen_lines(), 6);
    let rows = term.visible_rows();
    let text: String = (0..9).map(|column| rows[0][Column(column)].c()).collect();
    assert_eq!(text, "M0-OUTPUT");
}

#[test]
fn public_modes_follow_application_cursor_and_bracketed_paste_sequences() {
    let mut term = terminal();
    let mut parser = Processor::default();
    assert!(
        !term
            .mode()
            .intersects(Mode::APP_CURSOR | Mode::BRACKETED_PASTE)
    );
    parser.advance(&mut term, b"\x1b[?1h\x1b[?2004h");
    assert!(
        term.mode()
            .contains(Mode::APP_CURSOR | Mode::BRACKETED_PASTE)
    );
    parser.advance(&mut term, b"\x1b[?1l\x1b[?2004l");
    assert!(
        !term
            .mode()
            .intersects(Mode::APP_CURSOR | Mode::BRACKETED_PASTE)
    );
}

#[test]
fn public_cell_text_preserves_combining_text_and_wide_spacers() {
    let mut term = terminal();
    Processor::default().advance(&mut term, "e\u{301}界".as_bytes());
    let rows = term.visible_rows();
    let base = &rows[0][Column(0)];
    assert_eq!(base.c(), 'e');
    let cluster: String = term.grid.cell_text(Pos::new(Line(0), Column(0))).collect();
    assert_eq!(cluster, "e\u{301}");
    assert_eq!(rows[0][Column(1)].c(), '界');
    assert!(rows[0][Column(1)].is_wide());
    assert!(rows[0][Column(2)].is_spacer());
}

#[test]
fn public_cursor_and_alternate_screen_state_are_observable() {
    let mut term = terminal();
    let mut parser = Processor::default();
    parser.advance(&mut term, b"main\x1b[?1049h\x1b[?25l");
    assert!(term.mode().contains(Mode::ALT_SCREEN));
    assert!(matches!(term.cursor().content, CursorShape::Hidden));
    parser.advance(&mut term, b"\x1b[?1049l\x1b[?25h");
    assert!(!term.mode().contains(Mode::ALT_SCREEN));
    assert!(!matches!(term.cursor().content, CursorShape::Hidden));
    let rows = term.visible_rows();
    let text: String = (0..4).map(|column| rows[0][Column(column)].c()).collect();
    assert_eq!(text, "main");
}
