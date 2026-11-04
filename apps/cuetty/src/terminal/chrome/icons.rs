//! The chrome's icons: a few Lucide glyphs, bundled so they cannot go missing.
//!
//! GPUI only ever uses an SVG's coverage and tints it, so every icon takes the
//! colour of the text around it.

use super::tokens::size;
use gpui::{AssetSource, Rgba, SharedString, Styled, Svg, svg};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Icon {
    Plus,
    Close,
    Tasks,
    /// Collapses the sidebar towards the window edge.
    SidebarCollapse,
    /// Expands the sidebar away from the window edge.
    SidebarExpand,
    /// A tab rang its bell.
    Bell,
    /// A tab's task is running. Drawn still: an idle window never redraws for it.
    Running,
    /// A tab's task has finished.
    Finished,
    /// A tab's shell failed.
    Failed,
}

impl Icon {
    const ALL: [Self; 9] = [
        Self::Plus,
        Self::Close,
        Self::Tasks,
        Self::SidebarCollapse,
        Self::SidebarExpand,
        Self::Bell,
        Self::Running,
        Self::Finished,
        Self::Failed,
    ];

    const fn path(self) -> &'static str {
        match self {
            Self::Plus => "icons/plus.svg",
            Self::Close => "icons/x.svg",
            Self::Tasks => "icons/list-todo.svg",
            Self::SidebarCollapse => "icons/panel-right-close.svg",
            Self::SidebarExpand => "icons/panel-right-open.svg",
            Self::Bell => "icons/bell.svg",
            Self::Running => "icons/loader-circle.svg",
            Self::Finished => "icons/check.svg",
            Self::Failed => "icons/triangle-alert.svg",
        }
    }

    const fn source(self) -> &'static [u8] {
        match self {
            Self::Plus => include_bytes!("../../../assets/icons/plus.svg"),
            Self::Close => include_bytes!("../../../assets/icons/x.svg"),
            Self::Tasks => include_bytes!("../../../assets/icons/list-todo.svg"),
            Self::SidebarCollapse => include_bytes!("../../../assets/icons/panel-right-close.svg"),
            Self::SidebarExpand => include_bytes!("../../../assets/icons/panel-right-open.svg"),
            Self::Bell => include_bytes!("../../../assets/icons/bell.svg"),
            Self::Running => include_bytes!("../../../assets/icons/loader-circle.svg"),
            Self::Finished => include_bytes!("../../../assets/icons/check.svg"),
            Self::Failed => include_bytes!("../../../assets/icons/triangle-alert.svg"),
        }
    }

    /// This icon at the chrome's one icon size.
    pub(super) fn element(self, color: Rgba) -> Svg {
        svg()
            .path(self.path())
            .size(size::ICON)
            .flex_none()
            .text_color(color)
    }
}

/// Serves the bundled icons to GPUI's SVG renderer.
pub(in crate::terminal) struct ChromeAssets;

impl AssetSource for ChromeAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::ALL
            .into_iter()
            .find(|icon| icon.path() == path)
            .map(|icon| Cow::Borrowed(icon.source())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .into_iter()
            .map(Icon::path)
            .filter(|icon| icon.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_served_at_the_path_its_element_asks_for() {
        for icon in Icon::ALL {
            let loaded = ChromeAssets.load(icon.path()).unwrap().unwrap();
            assert_eq!(loaded.as_ref(), icon.source());
            assert!(loaded.starts_with(b"<svg"), "{icon:?} is not an SVG");
        }
    }

    #[test]
    fn unknown_assets_are_absent_rather_than_an_error() {
        assert!(ChromeAssets.load("icons/nope.svg").unwrap().is_none());
    }

    #[test]
    fn listing_filters_by_prefix() {
        assert_eq!(ChromeAssets.list("icons/").unwrap().len(), Icon::ALL.len());
        assert!(ChromeAssets.list("fonts/").unwrap().is_empty());
    }
}
