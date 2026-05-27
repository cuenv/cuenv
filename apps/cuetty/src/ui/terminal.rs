use std::sync::Arc;

use anyhow::{Context as _, Result};
use flume::Receiver;
use gpui::{AppContext, Context as GpuiContext, Entity, SharedString, Window};
use gpui_ghostty_terminal::view::{TerminalInput, TerminalView};
use gpui_ghostty_terminal::{
    TerminalConfig, TerminalSession, default_terminal_font, default_terminal_font_features,
};

use super::layout::{PaneNode, PixelRegion, TerminalId, split_region};
use super::{RootView, SIDEBAR_WIDTH, TITLEBAR_HEIGHT};
use crate::pty::{GridMetrics, PtyGridSize, TerminalProcess, TerminalProcessOptions};
use crate::terminal_responses::TerminalResponseScanner;

pub(super) struct TerminalPane {
    pub(super) id: TerminalId,
    pub(super) view: Entity<TerminalView>,
    pub(super) master: Arc<dyn portable_pty::MasterPty + Send>,
    pub(super) focus_handle: gpui::FocusHandle,
}

pub(super) struct TerminalLaunchOptions {
    pub(super) terminal_id: TerminalId,
    pub(super) process_options: TerminalProcessOptions,
}

pub(super) fn launch_terminal(
    window: &mut Window,
    options: TerminalLaunchOptions,
    cx: &mut GpuiContext<RootView>,
) -> Result<TerminalPane> {
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
    let focus_handle = cx.focus_handle();
    let view_focus = focus_handle.clone();
    let view = cx.new(|_| TerminalView::new_with_input(session, view_focus, input));
    let master = process.master.clone();

    start_output_pump(
        window,
        OutputPumpInput {
            view: view.clone(),
            stdin_tx: process.stdin_tx.clone(),
            stdout_rx: process.stdout_rx.clone(),
        },
        cx,
    );

    spawn_pane_death_observer(window, options.terminal_id, process.exited_rx.clone(), cx);

    Ok(TerminalPane {
        id: options.terminal_id,
        view,
        master,
        focus_handle,
    })
}

fn spawn_pane_death_observer(
    window: &mut Window,
    terminal_id: TerminalId,
    exited_rx: Receiver<()>,
    cx: &mut GpuiContext<RootView>,
) {
    let root_entity = cx.entity();
    window
        .spawn(cx, async move |cx| {
            let _ = exited_rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                root_entity.update(cx, |root, cx| {
                    root.handle_pane_exit(terminal_id, window, cx);
                });
            });
        })
        .detach();
}

pub(super) fn install_resize_observer(window: &mut Window, cx: &mut GpuiContext<RootView>) {
    let subscription = cx.observe_window_bounds(window, move |root, window, cx| {
        root.resize_active_layout(window, cx);
    });
    subscription.detach();
}

struct OutputPumpInput {
    view: Entity<TerminalView>,
    stdin_tx: flume::Sender<Vec<u8>>,
    stdout_rx: flume::Receiver<Vec<u8>>,
}

fn start_output_pump(window: &mut Window, input: OutputPumpInput, cx: &mut GpuiContext<RootView>) {
    let view_for_task = input.view;
    let stdin_tx = input.stdin_tx;
    let stdout_rx = input.stdout_rx;
    window
        .spawn(cx, async move |cx| {
            let mut response_scanner = TerminalResponseScanner::default();
            let mut respond = |response: &[u8]| {
                if stdin_tx.send(response.to_vec()).is_err() {
                    tracing::debug!("terminal response channel is closed");
                }
            };

            while let Ok(first) = stdout_rx.recv_async().await {
                let mut batch = first;
                response_scanner.scan(&batch, &mut respond);

                while let Ok(chunk) = stdout_rx.try_recv() {
                    response_scanner.scan(&chunk, &mut respond);
                    batch.extend_from_slice(&chunk);
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

pub(super) fn resize_layout(
    layout: &PaneNode,
    panes: &mut [TerminalPane],
    window: &mut Window,
    cx: &mut GpuiContext<RootView>,
) {
    let Some((cell_width, cell_height)) = cell_metrics(window) else {
        return;
    };

    let region = terminal_region(window);
    let mut input = ResizeLayoutInput {
        panes,
        cell_width,
        cell_height,
        cx,
    };
    resize_layout_node(layout, region, &mut input);
}

struct ResizeLayoutInput<'a, 'b> {
    panes: &'a mut [TerminalPane],
    cell_width: f32,
    cell_height: f32,
    cx: &'a mut GpuiContext<'b, RootView>,
}

fn resize_layout_node(node: &PaneNode, region: PixelRegion, input: &mut ResizeLayoutInput<'_, '_>) {
    match node {
        PaneNode::Leaf(id) => resize_pane(*id, region, input),
        PaneNode::Split {
            axis,
            first,
            second,
        } => {
            let (first_region, second_region) = split_region(region, *axis);
            resize_layout_node(first, first_region, input);
            resize_layout_node(second, second_region, input);
        }
    }
}

fn resize_pane(
    terminal_id: TerminalId,
    region: PixelRegion,
    input: &mut ResizeLayoutInput<'_, '_>,
) {
    let Some(pane) = input.panes.iter_mut().find(|pane| pane.id == terminal_id) else {
        return;
    };

    let grid_size = PtyGridSize::from_metrics(GridMetrics {
        pixel_width: region.width,
        pixel_height: region.height,
        cell_width: input.cell_width,
        cell_height: input.cell_height,
    });

    if let Err(error) = pane.master.resize(grid_size.to_pty_size()) {
        tracing::debug!(%error, "failed to resize PTY");
    }
    pane.view.update(input.cx, |terminal, cx| {
        terminal.resize_terminal(grid_size.cols, grid_size.rows, cx);
    });
}

fn terminal_region(window: &mut Window) -> PixelRegion {
    let size = window.viewport_size();
    PixelRegion {
        width: (f32::from(size.width) - SIDEBAR_WIDTH).max(1.0),
        height: (f32::from(size.height) - TITLEBAR_HEIGHT).max(1.0),
    }
}

fn cell_metrics(window: &mut Window) -> Option<(f32, f32)> {
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

    Some((cell_width, cell_height))
}
