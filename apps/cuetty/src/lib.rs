mod pty;
mod terminal_responses;
mod theme;
mod ui;

pub use pty::{PtyGridSize, TerminalEnvironment, TerminalProcessOptions, shell_path_from_env};

use gpui::{
    App, AppContext, Application, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowOptions,
    actions,
};
use gpui_ghostty_terminal::view::{Copy, Paste, SelectAll};
use tracing_subscriber::EnvFilter;

use crate::ui::RootView;

const APP_NAME: &str = "Cuetty";
const APP_ID: &str = "com.cuenv.cuetty";

actions!(
    cuetty,
    [CloseTab, FocusNextPane, NewTab, Quit, SplitDown, SplitRight]
);

#[derive(Clone, Debug, Default)]
pub struct CuettyOptions {
    pub terminal: TerminalProcessOptions,
}

pub fn run(options: CuettyOptions) {
    init_logging();

    Application::new().run(move |cx: &mut App| {
        install_app_chrome(cx);
        bind_keys(cx);
        let terminal_options = options.terminal.clone();

        if let Err(error) = cx.open_window(window_options(), move |window, cx| {
            cx.new(|cx| RootView::new(window, terminal_options.clone(), cx))
        }) {
            tracing::error!(%error, "failed to open Cuetty window");
            cx.quit();
        }
    });
}

fn install_app_chrome(cx: &mut App) {
    cx.on_action(quit);
    cx.set_menus(vec![Menu {
        name: APP_NAME.into(),
        items: vec![
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::action("Focus Next Pane", FocusNextPane),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ],
    }]);
}

fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-d", SplitRight, None),
        KeyBinding::new("cmd-shift-d", SplitDown, None),
        KeyBinding::new("cmd-]", FocusNextPane, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("ctrl-shift-t", NewTab, None),
        KeyBinding::new("ctrl-shift-w", CloseTab, None),
        KeyBinding::new("ctrl-shift-d", SplitRight, None),
        KeyBinding::new("ctrl-shift-e", SplitDown, None),
        KeyBinding::new("ctrl-shift-]", FocusNextPane, None),
        KeyBinding::new("ctrl-shift-c", Copy, None),
        KeyBinding::new("ctrl-shift-v", Paste, None),
    ]);
}

fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

fn window_options() -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(APP_NAME.into()),
            appears_transparent: true,
            ..TitlebarOptions::default()
        }),
        app_id: Some(APP_ID.to_string()),
        ..WindowOptions::default()
    }
}

fn init_logging() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("cuetty=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_options_use_cuetty_identity() {
        let options = window_options();

        assert_eq!(options.app_id.as_deref(), Some(APP_ID));
        assert_eq!(
            options
                .titlebar
                .and_then(|titlebar| titlebar.title)
                .map(|title| title.to_string()),
            Some(APP_NAME.to_string())
        );
    }
}
