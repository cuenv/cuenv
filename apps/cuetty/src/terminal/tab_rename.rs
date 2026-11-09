//! Renaming a tab in place. The edit is kept here, apart from any window, so what each
//! key does to it and what name it comes to can be tested on their own.

use crate::workspace::TabId;

/// What a key means while a tab's name is being edited.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenameKey<'name> {
    Commit,
    Cancel,
    Backspace,
    Text(&'name str),
    /// Taken so it cannot reach the terminal, but does nothing to the name.
    Ignored,
}

/// What Alt means on a platform. macOS's Option composes characters, and it is the only
/// way to type some of them on many layouts, such as `@` on a German one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AltKey {
    Composes,
    Chords,
}

impl AltKey {
    const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Composes
        } else {
            Self::Chords
        }
    }

    /// Whether these modifiers hold a chord down, which types nothing.
    const fn makes_a_chord(self, modifiers: gpui::Modifiers) -> bool {
        modifiers.control || modifiers.platform || (modifiers.alt && matches!(self, Self::Chords))
    }
}

/// Reads a key press for the name being edited. A chord types nothing.
pub(super) fn rename_key<'a>(
    key: &str,
    key_char: Option<&'a str>,
    modifiers: gpui::Modifiers,
) -> RenameKey<'a> {
    match key {
        "enter" => RenameKey::Commit,
        "escape" => RenameKey::Cancel,
        "backspace" => RenameKey::Backspace,
        _ if AltKey::current().makes_a_chord(modifiers) => RenameKey::Ignored,
        _ => key_char.map_or(RenameKey::Ignored, RenameKey::Text),
    }
}

/// A tab's name while it is being edited.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TabRename {
    tab: TabId,
    text: String,
}

impl TabRename {
    /// Starts from the name the user last gave the tab. A derived name is not offered
    /// for editing: it would stop following what the tab is doing if accepted as it is.
    pub(super) fn new(tab: TabId, custom_name: Option<&str>) -> Self {
        Self {
            tab,
            text: custom_name.unwrap_or_default().to_owned(),
        }
    }

    pub(super) const fn tab(&self) -> TabId {
        self.tab
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn insert(&mut self, text: &str) {
        self.text
            .extend(text.chars().filter(|character| !character.is_control()));
    }

    pub(super) fn backspace(&mut self) {
        self.text.pop();
    }

    /// The name this edit comes to. A blank one hands the tab back to its derived name.
    pub(super) fn into_name(self) -> Option<String> {
        let name = self.text.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(text: Option<&str>) -> TabRename {
        TabRename::new(TabId::new(1), text)
    }

    #[test]
    fn the_keys_that_end_or_edit_a_rename_work_with_any_modifier() {
        let control = gpui::Modifiers {
            control: true,
            ..gpui::Modifiers::default()
        };
        assert_eq!(rename_key("enter", Some("\n"), control), RenameKey::Commit);
        assert_eq!(rename_key("escape", None, control), RenameKey::Cancel);
        assert_eq!(rename_key("backspace", None, control), RenameKey::Backspace);
    }

    #[test]
    fn only_an_unchorded_key_types() {
        let plain = gpui::Modifiers::default();
        let shifted = gpui::Modifiers {
            shift: true,
            ..plain
        };
        assert_eq!(rename_key("a", Some("a"), plain), RenameKey::Text("a"));
        assert_eq!(rename_key("a", Some("A"), shifted), RenameKey::Text("A"));
        for modifiers in [
            gpui::Modifiers {
                control: true,
                ..plain
            },
            gpui::Modifiers {
                platform: true,
                ..plain
            },
        ] {
            assert_eq!(rename_key("a", Some("a"), modifiers), RenameKey::Ignored);
        }
        assert_eq!(rename_key("left", None, plain), RenameKey::Ignored);
    }

    #[test]
    fn alt_types_only_where_it_composes_characters() {
        let plain = gpui::Modifiers::default();
        let alt = gpui::Modifiers { alt: true, ..plain };
        assert!(!AltKey::Composes.makes_a_chord(alt));
        assert!(AltKey::Chords.makes_a_chord(alt));
        // Option held with Control or Command is a chord everywhere.
        for held in [
            gpui::Modifiers {
                control: true,
                ..alt
            },
            gpui::Modifiers {
                platform: true,
                ..alt
            },
        ] {
            assert!(AltKey::Composes.makes_a_chord(held));
        }
    }

    #[test]
    fn a_rename_starts_from_the_users_name_and_never_a_derived_one() {
        assert_eq!(rename(Some("deploys")).text(), "deploys");
        assert_eq!(rename(None).text(), "");
    }

    #[test]
    fn typing_and_backspace_edit_whole_characters() {
        let mut edit = rename(None);
        edit.insert("dé");
        edit.insert("p");
        assert_eq!(edit.text(), "dép");
        edit.backspace();
        edit.backspace();
        assert_eq!(edit.text(), "d");
        edit.backspace();
        edit.backspace();
        assert_eq!(edit.text(), "");
    }

    #[test]
    fn control_characters_never_reach_the_name() {
        let mut edit = rename(None);
        edit.insert("a\tb\n\u{7f}c");
        assert_eq!(edit.text(), "abc");
    }

    #[test]
    fn a_name_is_trimmed_and_a_blank_one_goes_back_to_deriving() {
        let mut edit = rename(None);
        edit.insert("  deploys ");
        assert_eq!(edit.into_name().as_deref(), Some("deploys"));
        let mut blank = rename(Some("deploys"));
        for _ in 0.."deploys".len() {
            blank.backspace();
        }
        blank.insert("   ");
        assert_eq!(blank.into_name(), None);
    }
}
