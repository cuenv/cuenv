//! Inline responder for VT queries that `gpui_ghostty_terminal` does not yet handle.
//!
//! Upstream `TerminalSession::feed_with_pty_responses` (at the pinned rev) answers
//! DSR (`CSI 5/6 n`) and OSC color (`OSC 10/11`) queries but not Primary or Secondary
//! Device Attributes (`CSI c`, `CSI 0 c`, `CSI > c`, `CSI > 0 c`). Shells like fish
//! block their prompt on a DA1 reply at startup, so we scan the PTY output stream
//! ourselves and inject the answer. Delete this module once upstream lands DA
//! support in `feed_with_pty_responses`.

const ESC: u8 = 0x1b;
const PRIMARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[?62;22c";
const SECONDARY_DEVICE_ATTRIBUTES_RESPONSE: &[u8] = b"\x1b[>1;10;0c";

#[derive(Debug, Default)]
pub(crate) struct TerminalResponseScanner {
    state: DeviceAttributesState,
}

impl TerminalResponseScanner {
    pub(crate) fn scan(&mut self, bytes: &[u8], mut respond: impl FnMut(&[u8])) {
        for byte in bytes {
            if let Some(response) = self.advance(*byte) {
                respond(response);
            }
        }
    }

    fn advance(&mut self, byte: u8) -> Option<&'static [u8]> {
        match self.state.next(byte) {
            Transition::Continue(state) => {
                self.state = state;
                None
            }
            Transition::Respond {
                response,
                next_state,
            } => {
                self.state = next_state;
                Some(response)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DeviceAttributesState {
    #[default]
    Ground,
    Escape,
    Csi,
    PrimaryZero,
    Secondary,
    SecondaryZero,
}

impl DeviceAttributesState {
    fn next(self, byte: u8) -> Transition {
        use DeviceAttributesState::{Csi, Escape, Ground, PrimaryZero, Secondary, SecondaryZero};

        match (self, byte) {
            (Ground, ESC) => Transition::continue_with(Escape),
            (Escape, b'[') => Transition::continue_with(Csi),
            (Escape, ESC) => Transition::continue_with(Escape),

            (Csi, b'c') => Transition::respond(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE),
            (Csi, b'0') => Transition::continue_with(PrimaryZero),
            (Csi, b'>') => Transition::continue_with(Secondary),
            (Csi, ESC) => Transition::continue_with(Escape),

            (PrimaryZero, b'c') => Transition::respond(PRIMARY_DEVICE_ATTRIBUTES_RESPONSE),
            (PrimaryZero, ESC) => Transition::continue_with(Escape),

            (Secondary, b'c') => Transition::respond(SECONDARY_DEVICE_ATTRIBUTES_RESPONSE),
            (Secondary, b'0') => Transition::continue_with(SecondaryZero),
            (Secondary, ESC) => Transition::continue_with(Escape),

            (SecondaryZero, b'c') => Transition::respond(SECONDARY_DEVICE_ATTRIBUTES_RESPONSE),
            (SecondaryZero, ESC) => Transition::continue_with(Escape),

            _ => Transition::continue_with(Ground),
        }
    }
}

enum Transition {
    Continue(DeviceAttributesState),
    Respond {
        response: &'static [u8],
        next_state: DeviceAttributesState,
    },
}

impl Transition {
    const fn continue_with(state: DeviceAttributesState) -> Self {
        Self::Continue(state)
    }

    const fn respond(response: &'static [u8]) -> Self {
        Self::Respond {
            response,
            next_state: DeviceAttributesState::Ground,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responds_to_primary_device_attributes_query() {
        assert_eq!(
            responses_for([b"\x1b[c".as_slice()]),
            vec![PRIMARY_DEVICE_ATTRIBUTES_RESPONSE.to_vec()]
        );
    }

    #[test]
    fn responds_to_primary_device_attributes_zero_query() {
        assert_eq!(
            responses_for([b"\x1b[0c".as_slice()]),
            vec![PRIMARY_DEVICE_ATTRIBUTES_RESPONSE.to_vec()]
        );
    }

    #[test]
    fn responds_to_split_primary_device_attributes_query() {
        assert_eq!(
            responses_for([b"\x1b[".as_slice(), b"0".as_slice(), b"c".as_slice()]),
            vec![PRIMARY_DEVICE_ATTRIBUTES_RESPONSE.to_vec()]
        );
    }

    #[test]
    fn responds_to_secondary_device_attributes_query() {
        assert_eq!(
            responses_for([b"\x1b[>c".as_slice()]),
            vec![SECONDARY_DEVICE_ATTRIBUTES_RESPONSE.to_vec()]
        );
    }

    #[test]
    fn ignores_device_attributes_responses() {
        assert!(responses_for([b"\x1b[?62;22c".as_slice()]).is_empty());
        assert!(responses_for([b"\x1b[>1;10;0c".as_slice()]).is_empty());
    }

    fn responses_for<const N: usize>(chunks: [&[u8]; N]) -> Vec<Vec<u8>> {
        let mut scanner = TerminalResponseScanner::default();
        let mut responses = Vec::new();
        for chunk in chunks {
            scanner.scan(chunk, |response| responses.push(response.to_vec()));
        }
        responses
    }
}
