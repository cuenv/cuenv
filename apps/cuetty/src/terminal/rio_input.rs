//! Input encoding for the direct Rio session.
//!
//! Keyboard protocol negotiation belongs to Rio's session state. Cuetty
//! implements the subset representable by its host event: key presses/repeats,
//! modifiers, characters, and the named keys below. The host event cannot yet
//! distinguish keypad keys, key releases, alternate key codes, or committed IME
//! text. Application keypad mode must therefore never change an ordinary digit
//! into a keypad key.

use super::input::{InputModifiers, KeyInput, TerminalKeyEvent};
use rio_vt::crosswords::Mode;

/// Encode the currently supported host keys using live terminal modes.
/// Legacy repeats match the initial press; negotiated event reporting marks
/// repeats explicitly.
pub fn encode_key(
    event: TerminalKeyEvent,
    mode: Mode,
    modify_other_keys: Option<u8>,
) -> Option<Vec<u8>> {
    let modifiers = event.modifiers;
    if modifiers.contains(InputModifiers::SUPER) {
        // An unhandled macOS command shortcut must not type into the shell.
        return None;
    }

    if should_encode_kitty(event, mode) {
        return Some(encode_kitty_key(event, mode));
    }
    if should_encode_modify_other_keys(event, modify_other_keys) {
        return Some(encode_modify_other_key(event));
    }

    let final_byte = match event.key {
        KeyInput::Up => Some('A'),
        KeyInput::Down => Some('B'),
        KeyInput::Right => Some('C'),
        KeyInput::Left => Some('D'),
        KeyInput::Home => Some('H'),
        KeyInput::End => Some('F'),
        _ => None,
    };
    let parameter = modifier_parameter(modifiers);
    if let Some(final_byte) = final_byte {
        let sequence = match parameter {
            1 if mode.contains(Mode::APP_CURSOR) => format!("\x1bO{final_byte}"),
            1 => format!("\x1b[{final_byte}"),
            _ => format!("\x1b[1;{parameter}{final_byte}"),
        };
        return Some(sequence.into_bytes());
    }
    if event.key == KeyInput::Delete {
        return Some(if parameter == 1 {
            b"\x1b[3~".to_vec()
        } else {
            format!("\x1b[3;{parameter}~").into_bytes()
        });
    }

    let bytes = match event.key {
        KeyInput::Character(character) if modifiers.contains(InputModifiers::CONTROL) => {
            vec![control_byte(character)?]
        }
        KeyInput::Character(character) => character.to_string().into_bytes(),
        KeyInput::Enter => vec![b'\r'],
        KeyInput::Tab if modifiers.contains(InputModifiers::SHIFT) => {
            return Some(b"\x1b[Z".to_vec());
        }
        KeyInput::Tab => vec![b'\t'],
        KeyInput::Backspace => vec![0x7f],
        KeyInput::Escape => vec![0x1b],
        KeyInput::Up
        | KeyInput::Down
        | KeyInput::Left
        | KeyInput::Right
        | KeyInput::Home
        | KeyInput::End
        | KeyInput::Delete => return None,
    };
    if modifiers.contains(InputModifiers::ALT) {
        Some(std::iter::once(0x1b).chain(bytes).collect())
    } else {
        Some(bytes)
    }
}

fn should_encode_kitty(event: TerminalKeyEvent, mode: Mode) -> bool {
    if mode.contains(Mode::REPORT_ALL_KEYS_AS_ESC) {
        return true;
    }
    if !mode.intersects(Mode::DISAMBIGUATE_ESC_CODES | Mode::REPORT_EVENT_TYPES) {
        return false;
    }
    match event.key {
        KeyInput::Character(_) => {
            mode.contains(Mode::DISAMBIGUATE_ESC_CODES)
                && (event.modifiers.contains(InputModifiers::CONTROL)
                    || event.modifiers.contains(InputModifiers::ALT))
        }
        KeyInput::Enter | KeyInput::Tab | KeyInput::Backspace | KeyInput::Escape => true,
        KeyInput::Up
        | KeyInput::Down
        | KeyInput::Left
        | KeyInput::Right
        | KeyInput::Home
        | KeyInput::End
        | KeyInput::Delete => true,
    }
}

fn encode_kitty_key(event: TerminalKeyEvent, mode: Mode) -> Vec<u8> {
    let modifiers = kitty_modifier_parameter(event.modifiers);
    let repeat = mode.contains(Mode::REPORT_EVENT_TYPES) && event.repeat;
    let suffix = if repeat {
        format!(";{modifiers}:2")
    } else if modifiers > 1 {
        format!(";{modifiers}")
    } else {
        String::new()
    };
    let navigation_base = if suffix.is_empty() { "" } else { "1" };
    let sequence = match event.key {
        KeyInput::Up => format!("\x1b[{navigation_base}{suffix}A"),
        KeyInput::Down => format!("\x1b[{navigation_base}{suffix}B"),
        KeyInput::Right => format!("\x1b[{navigation_base}{suffix}C"),
        KeyInput::Left => format!("\x1b[{navigation_base}{suffix}D"),
        KeyInput::Home => format!("\x1b[{navigation_base}{suffix}H"),
        KeyInput::End => format!("\x1b[{navigation_base}{suffix}F"),
        KeyInput::Delete => format!("\x1b[3{suffix}~"),
        KeyInput::Character(character) => {
            format!(
                "\x1b[{}{suffix}u",
                kitty_character_code(character, event.modifiers)
            )
        }
        KeyInput::Enter => format!("\x1b[13{suffix}u"),
        KeyInput::Tab => format!("\x1b[9{suffix}u"),
        KeyInput::Backspace => format!("\x1b[127{suffix}u"),
        KeyInput::Escape => format!("\x1b[27{suffix}u"),
    };
    sequence.into_bytes()
}

fn should_encode_modify_other_keys(event: TerminalKeyEvent, level: Option<u8>) -> bool {
    level.is_some_and(|level| level > 0)
        && !matches!(
            event.key,
            KeyInput::Up
                | KeyInput::Down
                | KeyInput::Left
                | KeyInput::Right
                | KeyInput::Home
                | KeyInput::End
                | KeyInput::Delete
        )
        && (event.modifiers.contains(InputModifiers::CONTROL)
            || event.modifiers.contains(InputModifiers::ALT)
            || event.modifiers.contains(InputModifiers::SHIFT))
}

fn encode_modify_other_key(event: TerminalKeyEvent) -> Vec<u8> {
    let codepoint = match event.key {
        KeyInput::Character(character) => kitty_character_code(character, event.modifiers),
        KeyInput::Enter => 13,
        KeyInput::Tab => 9,
        KeyInput::Backspace => 127,
        KeyInput::Escape => 27,
        KeyInput::Up
        | KeyInput::Down
        | KeyInput::Left
        | KeyInput::Right
        | KeyInput::Home
        | KeyInput::End
        | KeyInput::Delete => unreachable!("navigation keys use their ordinary CSI encoding"),
    };
    format!(
        "\x1b[27;{};{codepoint}~",
        kitty_modifier_parameter(event.modifiers)
    )
    .into_bytes()
}

fn kitty_character_code(character: char, modifiers: InputModifiers) -> u32 {
    if modifiers.contains(InputModifiers::SHIFT) {
        character
            .to_lowercase()
            .next()
            .map(u32::from)
            .unwrap_or(character.into())
    } else {
        character.into()
    }
}

fn kitty_modifier_parameter(modifiers: InputModifiers) -> u8 {
    1 + u8::from(modifiers.contains(InputModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(InputModifiers::ALT))
        + 4 * u8::from(modifiers.contains(InputModifiers::CONTROL))
        + 8 * u8::from(modifiers.contains(InputModifiers::SUPER))
}

fn modifier_parameter(modifiers: InputModifiers) -> u8 {
    1 + u8::from(modifiers.contains(InputModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(InputModifiers::ALT))
        + 4 * u8::from(modifiers.contains(InputModifiers::CONTROL))
}

fn control_byte(character: char) -> Option<u8> {
    match character {
        'a'..='z' | 'A'..='Z' => Some(character as u8 & 0x1f),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Paste is distinct from typing: preserve newlines inside brackets, otherwise
/// normalize them to the CR emitted by Enter. Remove bracket terminators and
/// interrupt controls from bracketed payloads so pasted content cannot escape.
pub fn encode_paste(text: &str, mode: Mode) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    if !mode.contains(Mode::BRACKETED_PASTE) {
        return text.replace("\r\n", "\r").replace('\n', "\r").into_bytes();
    }
    let mut bytes = b"\x1b[200~".to_vec();
    for character in text.chars() {
        if !matches!(character, '\x1b' | '\x03' | '\u{9b}') {
            bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: KeyInput, modifiers: InputModifiers) -> TerminalKeyEvent {
        TerminalKeyEvent {
            key,
            modifiers,
            repeat: false,
        }
    }

    #[test]
    fn printable_characters_preserve_utf8_and_shifted_host_text() {
        for character in ['a', 'A', 'é', '界', '🦀'] {
            assert_eq!(
                encode_key(
                    press(KeyInput::Character(character), InputModifiers::SHIFT),
                    Mode::NONE,
                    None,
                ),
                Some(character.to_string().into_bytes()),
            );
        }
    }

    #[test]
    fn ascii_controls_and_numeric_aliases_are_encoded() {
        for (character, expected) in [
            ('a', 1),
            ('Z', 26),
            (' ', 0),
            ('@', 0),
            ('2', 0),
            ('[', 27),
            ('3', 27),
            ('\\', 28),
            ('4', 28),
            (']', 29),
            ('5', 29),
            ('^', 30),
            ('6', 30),
            ('_', 31),
            ('/', 31),
            ('7', 31),
            ('?', 127),
            ('8', 127),
        ] {
            assert_eq!(
                encode_key(
                    press(KeyInput::Character(character), InputModifiers::CONTROL),
                    Mode::NONE,
                    None,
                ),
                Some(vec![expected]),
            );
        }
        assert_eq!(
            encode_key(
                press(KeyInput::Character('界'), InputModifiers::CONTROL),
                Mode::NONE,
                None,
            ),
            None,
        );
    }

    #[test]
    fn navigation_respects_application_cursor_mode() {
        for (key, final_byte) in [
            (KeyInput::Up, 'A'),
            (KeyInput::Down, 'B'),
            (KeyInput::Right, 'C'),
            (KeyInput::Left, 'D'),
            (KeyInput::Home, 'H'),
            (KeyInput::End, 'F'),
        ] {
            let event = press(key, InputModifiers::default());
            assert_eq!(
                encode_key(event, Mode::NONE, None),
                Some(format!("\x1b[{final_byte}").into_bytes()),
            );
            assert_eq!(
                encode_key(event, Mode::APP_CURSOR, None),
                Some(format!("\x1bO{final_byte}").into_bytes()),
            );
        }
    }

    #[test]
    fn modified_navigation_always_uses_parameterized_csi() {
        for (modifiers, parameter) in [
            (InputModifiers::SHIFT, 2),
            (InputModifiers::ALT, 3),
            (InputModifiers::CONTROL, 5),
            (InputModifiers::from_bits(7), 8),
        ] {
            assert_eq!(
                encode_key(press(KeyInput::Left, modifiers), Mode::APP_CURSOR, None),
                Some(format!("\x1b[1;{parameter}D").into_bytes()),
            );
        }
    }

    #[test]
    fn special_keys_and_delete_keep_legacy_sequences() {
        for (key, expected) in [
            (KeyInput::Enter, b"\r".as_slice()),
            (KeyInput::Tab, b"\t".as_slice()),
            (KeyInput::Backspace, b"\x7f".as_slice()),
            (KeyInput::Escape, b"\x1b".as_slice()),
            (KeyInput::Delete, b"\x1b[3~".as_slice()),
        ] {
            assert_eq!(
                encode_key(press(key, InputModifiers::default()), Mode::NONE, None),
                Some(expected.to_vec()),
            );
        }
        assert_eq!(
            encode_key(
                press(KeyInput::Tab, InputModifiers::SHIFT),
                Mode::NONE,
                None
            ),
            Some(b"\x1b[Z".to_vec()),
        );
        assert_eq!(
            encode_key(
                press(KeyInput::Delete, InputModifiers::CONTROL),
                Mode::NONE,
                None
            ),
            Some(b"\x1b[3;5~".to_vec()),
        );
    }

    #[test]
    fn alt_prefixes_text_and_control_bytes() {
        assert_eq!(
            encode_key(
                press(KeyInput::Character('d'), InputModifiers::ALT),
                Mode::NONE,
                None,
            ),
            Some(b"\x1bd".to_vec()),
        );
        let modifiers =
            InputModifiers::from_bits(InputModifiers::ALT.bits() | InputModifiers::CONTROL.bits());
        assert_eq!(
            encode_key(press(KeyInput::Character('c'), modifiers), Mode::NONE, None),
            Some(vec![0x1b, 3]),
        );
    }

    #[test]
    fn command_shortcuts_never_leak_to_the_pty() {
        for key in [KeyInput::Character('c'), KeyInput::Enter, KeyInput::Left] {
            assert_eq!(
                encode_key(press(key, InputModifiers::SUPER), Mode::APP_CURSOR, None),
                None,
            );
        }
    }

    #[test]
    fn repeat_matches_press_and_keypad_mode_does_not_reinterpret_digits() {
        let mut event = press(KeyInput::Character('1'), InputModifiers::default());
        let initial = encode_key(event, Mode::NONE, None);
        event.repeat = true;
        assert_eq!(encode_key(event, Mode::APP_KEYPAD, None), initial);
        assert_eq!(initial, Some(b"1".to_vec()));
    }

    #[test]
    fn kitty_disambiguation_keeps_plain_text_and_encodes_ambiguous_keys() {
        let mode = Mode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(
            encode_key(
                press(KeyInput::Character('a'), InputModifiers::default()),
                mode,
                None,
            ),
            Some(b"a".to_vec()),
        );
        assert_eq!(
            encode_key(
                press(KeyInput::Character('c'), InputModifiers::CONTROL),
                mode,
                None,
            ),
            Some(b"\x1b[99;5u".to_vec()),
        );
        assert_eq!(
            encode_key(
                press(KeyInput::Enter, InputModifiers::default()),
                mode,
                None
            ),
            Some(b"\x1b[13u".to_vec()),
        );
        assert_eq!(
            encode_key(press(KeyInput::Up, InputModifiers::default()), mode, None),
            Some(b"\x1b[A".to_vec()),
        );
    }

    #[test]
    fn kitty_report_all_and_repeat_use_csi_u_event_metadata() {
        assert_eq!(
            encode_key(
                press(KeyInput::Character('A'), InputModifiers::SHIFT),
                Mode::REPORT_ALL_KEYS_AS_ESC,
                None,
            ),
            Some(b"\x1b[97;2u".to_vec()),
        );
        let mut event = press(KeyInput::Left, InputModifiers::ALT);
        event.repeat = true;
        assert_eq!(
            encode_key(event, Mode::REPORT_EVENT_TYPES, None),
            Some(b"\x1b[1;3:2D".to_vec()),
        );
    }

    #[test]
    fn modify_other_keys_encodes_modified_text_without_blocking_plain_text() {
        assert_eq!(
            encode_key(
                press(KeyInput::Character('c'), InputModifiers::CONTROL),
                Mode::NONE,
                Some(2),
            ),
            Some(b"\x1b[27;5;99~".to_vec()),
        );
        assert_eq!(
            encode_key(
                press(KeyInput::Character('a'), InputModifiers::default()),
                Mode::NONE,
                Some(2),
            ),
            Some(b"a".to_vec()),
        );
    }

    #[test]
    fn plain_paste_normalizes_newlines_without_doubling_crlf() {
        assert_eq!(encode_paste("a\r\nb\nc\rd", Mode::NONE), b"a\rb\rc\rd");
    }

    #[test]
    fn bracketed_paste_preserves_text_and_cannot_inject_a_terminator() {
        assert_eq!(
            encode_paste("é\n界\r\n", Mode::BRACKETED_PASTE),
            "\x1b[200~é\n界\r\n\x1b[201~".as_bytes(),
        );
        assert_eq!(
            encode_paste("safe\x1b[201~\x03\u{9b}201~", Mode::BRACKETED_PASTE),
            b"\x1b[200~safe[201~201~\x1b[201~",
        );
    }

    #[test]
    fn empty_paste_sends_nothing_in_either_mode() {
        assert!(encode_paste("", Mode::NONE).is_empty());
        assert!(encode_paste("", Mode::BRACKETED_PASTE).is_empty());
    }
}
