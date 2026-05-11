use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::{
    AnyElement, AppContext, Context as GpuiContext, Entity, FocusHandle, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px, rgb,
};
use gpui_ghostty_terminal::view::{TerminalInput, TerminalView};
use gpui_ghostty_terminal::{
    TerminalConfig, TerminalSession, default_terminal_font, default_terminal_font_features,
};

use crate::pty::{GridMetrics, PtyGridSize, TerminalProcess, TerminalProcessOptions};
use crate::terminal_responses::TerminalResponseScanner;

pub struct RootView {
    terminal: Option<Entity<TerminalView>>,
    error: Option<String>,
    focus_handle: FocusHandle,
}

impl RootView {
    pub fn new(
        window: &mut Window,
        process_options: TerminalProcessOptions,
        cx: &mut GpuiContext<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let launch_options = TerminalLaunchOptions {
            process_options,
            focus_handle: focus_handle.clone(),
        };

        match launch_terminal(window, launch_options, cx) {
            Ok(terminal) => Self {
                terminal: Some(terminal),
                error: None,
                focus_handle,
            },
            Err(error) => {
                tracing::error!(%error, "failed to launch terminal");
                Self {
                    terminal: None,
                    error: Some(error.to_string()),
                    focus_handle,
                }
            }
        }
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, _cx: &mut GpuiContext<Self>) -> impl IntoElement {
        let content = match &self.terminal {
            Some(terminal) => div().size_full().child(terminal.clone()).into_any_element(),
            None => self.render_error().into_any_element(),
        };

        div()
            .id("cuetty-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(rgb(0x090d12))
            .text_color(rgb(0xe6edf3))
            .child(content)
    }
}

impl RootView {
    fn render_error(&self) -> AnyElement {
        let message = self
            .error
            .as_deref()
            .unwrap_or("Cuetty could not launch the terminal process.");

        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_3()
            .px(px(24.0))
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Cuetty could not start"),
            )
            .child(
                div()
                    .max_w(px(640.0))
                    .text_center()
                    .text_color(rgb(0x9fb0c0))
                    .child(message.to_string()),
            )
            .into_any_element()
    }
}

struct TerminalLaunchOptions {
    process_options: TerminalProcessOptions,
    focus_handle: FocusHandle,
}

fn launch_terminal(
    window: &mut Window,
    options: TerminalLaunchOptions,
    cx: &mut GpuiContext<RootView>,
) -> Result<Entity<TerminalView>> {
    let config = TerminalConfig::default();
    let process = TerminalProcess::spawn(TerminalProcessOptions {
        grid_size: PtyGridSize::new(config.cols, config.rows),
        ..options.process_options
    })?;

    let session = TerminalSession::new(config).context("failed to initialize Ghostty VT")?;
    let stdin_tx = process.stdin_tx.clone();
    let input = TerminalInput::new(move |bytes| {
        if stdin_tx.send(bytes.to_vec()).is_err() {
            tracing::debug!("terminal input channel is closed");
        }
    });
    let view = cx.new(|_| TerminalView::new_with_input(session, options.focus_handle, input));

    install_resize_observer(
        window,
        ResizeObserverInput {
            view: view.clone(),
            master: process.master.clone(),
        },
        cx,
    );
    start_output_pump(
        window,
        OutputPumpInput {
            view: view.clone(),
            process,
        },
        cx,
    );

    Ok(view)
}

struct ResizeObserverInput {
    view: Entity<TerminalView>,
    master: std::sync::Arc<dyn portable_pty::MasterPty + Send>,
}

fn install_resize_observer(
    window: &mut Window,
    input: ResizeObserverInput,
    cx: &mut GpuiContext<RootView>,
) {
    let subscription = input.view.update(cx, |_, cx| {
        let master = input.master.clone();
        cx.observe_window_bounds(window, move |terminal, window, cx| {
            let Some(grid_size) = grid_size_for_window(window) else {
                return;
            };

            if let Err(error) = master.resize(grid_size.to_pty_size()) {
                tracing::debug!(%error, "failed to resize PTY");
            }
            terminal.resize_terminal(grid_size.cols, grid_size.rows, cx);
        })
    });
    subscription.detach();
}

struct OutputPumpInput {
    view: Entity<TerminalView>,
    process: TerminalProcess,
}

fn start_output_pump(window: &mut Window, input: OutputPumpInput, cx: &mut GpuiContext<RootView>) {
    let view_for_task = input.view;
    let stdin_tx = input.process.stdin_tx;
    let stdout_rx = input.process.stdout_rx;
    window
        .spawn(cx, async move |cx| {
            let mut response_scanner = TerminalResponseScanner::default();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;

                let mut batch = Vec::new();
                while let Ok(chunk) = stdout_rx.try_recv() {
                    response_scanner.scan(&chunk, |response| {
                        if stdin_tx.send(response.to_vec()).is_err() {
                            tracing::debug!("terminal response channel is closed");
                        }
                    });
                    batch.extend_from_slice(&chunk);
                }
                if batch.is_empty() {
                    continue;
                }

                let _ = cx.update(|_, cx| {
                    view_for_task.update(cx, |terminal, cx| {
                        terminal.queue_output_bytes(&batch, cx);
                    });
                });
            }
        })
        .detach();
}

fn grid_size_for_window(window: &mut Window) -> Option<PtyGridSize> {
    let size = window.viewport_size();
    let width = f32::from(size.width);
    let height = f32::from(size.height);

    let mut style = window.text_style();
    let font = default_terminal_font();
    style.font_family = font.family.clone();
    style.font_features = default_terminal_font_features();
    style.font_fallbacks = font.fallbacks.clone();

    let rem_size = window.rem_size();
    let font_size = style.font_size.to_pixels(rem_size);
    let line_height = style.line_height.to_pixels(style.font_size, rem_size);

    let run = style.to_run(1);
    let lines = window
        .text_system()
        .shape_text(SharedString::from("M"), font_size, &[run], None, Some(1))
        .ok()?;
    let line = lines.first()?;
    let cell_width = f32::from(line.width()).max(1.0);
    let cell_height = f32::from(line_height).max(1.0);

    Some(PtyGridSize::from_metrics(GridMetrics {
        pixel_width: width,
        pixel_height: height,
        cell_width,
        cell_height,
    }))
}
