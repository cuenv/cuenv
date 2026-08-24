#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyInput {
    Character(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputModifiers(u8);
impl InputModifiers {
    pub const CONTROL: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const SHIFT: Self = Self(4);
    pub const SUPER: Self = Self(8);
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalKeyEvent {
    pub key: KeyInput,
    pub modifiers: InputModifiers,
    pub repeat: bool,
}

#[allow(dead_code)]
pub fn paste_bytes(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_enter_and_control() {
        let event = TerminalKeyEvent {
            key: KeyInput::Enter,
            modifiers: InputModifiers::CONTROL,
            repeat: false,
        };
        assert_eq!(event.key, KeyInput::Enter);
        assert!(event.modifiers.contains(InputModifiers::CONTROL));
    }
}
