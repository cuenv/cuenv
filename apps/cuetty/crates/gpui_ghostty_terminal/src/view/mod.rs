use super::TerminalSession;
use ghostty_vt::{KeyModifiers, StyleRun, encode_key_named};
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, IntoElement, KeyBinding, KeyDownEvent,
    MouseButton, Pixels, Render, SharedString, Window, actions, div, prelude::*,
};
use std::ops::Range;
use std::sync::Once;

mod element;
mod geometry;
mod input_handler;
mod links;
mod mouse;

#[cfg(test)]
pub(crate) use element::box_drawing_mask;
use element::terminal_text_element;
pub(crate) use geometry::{byte_index_for_column_in_line, cell_metrics};
#[cfg(test)]
pub(crate) use mouse::{sgr_mouse_button_value, sgr_mouse_sequence};

actions!(terminal_view, [Copy, Paste, SelectAll, Tab, TabPrev]);

const KEY_CONTEXT: &str = "Terminal";
static KEY_BINDINGS: Once = Once::new();

fn ensure_key_bindings(cx: &mut App) {
    KEY_BINDINGS.call_once(|| {
        cx.bind_keys([
            KeyBinding::new("tab", Tab, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-tab", TabPrev, Some(KEY_CONTEXT)),
        ]);
    });
}

fn split_viewport_lines(viewport: &str) -> Vec<String> {
    let viewport = viewport.strip_suffix('\n').unwrap_or(viewport);
    if viewport.is_empty() {
        return Vec::new();
    }
    viewport.split('\n').map(|line| line.to_string()).collect()
}

pub(crate) fn should_skip_key_down_for_ime(has_input: bool, keystroke: &gpui::Keystroke) -> bool {
    if !has_input || !keystroke.is_ime_in_progress() {
        return false;
    }

    !matches!(
        keystroke.key.as_str(),
        "enter" | "return" | "kp_enter" | "numpad_enter"
    )
}

pub(crate) fn ctrl_byte_for_keystroke(keystroke: &gpui::Keystroke) -> Option<u8> {
    let candidate = keystroke
        .key_char
        .as_deref()
        .or_else(|| (!keystroke.key.is_empty()).then_some(keystroke.key.as_str()))?;

    if candidate == "space" {
        return Some(0x00);
    }

    let bytes = candidate.as_bytes();
    if bytes.len() != 1 {
        return None;
    }

    let b = bytes[0];
    if (b'@'..=b'_').contains(&b) {
        Some(b & 0x1f)
    } else if b.is_ascii_lowercase() {
        Some(b - b'a' + 1)
    } else if b.is_ascii_uppercase() {
        Some(b - b'A' + 1)
    } else {
        None
    }
}

type TerminalSendFn = dyn Fn(&[u8]) + Send + Sync + 'static;

pub struct TerminalInput {
    send: Box<TerminalSendFn>,
}

impl TerminalInput {
    pub fn new(send: impl Fn(&[u8]) + Send + Sync + 'static) -> Self {
        Self {
            send: Box::new(send),
        }
    }

    pub fn send(&self, bytes: &[u8]) {
        (self.send)(bytes);
    }
}

pub struct TerminalView {
    session: TerminalSession,
    viewport_lines: Vec<String>,
    viewport_line_offsets: Vec<usize>,
    viewport_total_len: usize,
    viewport_style_runs: Vec<Vec<StyleRun>>,
    line_layouts: Vec<Option<gpui::ShapedLine>>,
    line_layout_key: Option<(Pixels, Pixels)>,
    last_bounds: Option<Bounds<Pixels>>,
    focus_handle: FocusHandle,
    last_window_title: Option<String>,
    input: Option<TerminalInput>,
    pending_output: Vec<u8>,
    pending_refresh: bool,
    selection: Option<ByteSelection>,
    marked_text: Option<SharedString>,
    marked_selected_range_utf16: Range<usize>,
    font: gpui::Font,
}

#[derive(Clone, Copy, Debug)]
struct ByteSelection {
    anchor: usize,
    active: usize,
}

impl ByteSelection {
    fn range(self) -> Range<usize> {
        if self.anchor <= self.active {
            self.anchor..self.active
        } else {
            self.active..self.anchor
        }
    }
}

impl TerminalView {
    pub fn new(session: TerminalSession, focus_handle: FocusHandle) -> Self {
        Self {
            session,
            viewport_lines: Vec::new(),
            viewport_line_offsets: Vec::new(),
            viewport_total_len: 0,
            viewport_style_runs: Vec::new(),
            line_layouts: Vec::new(),
            line_layout_key: None,
            last_bounds: None,
            focus_handle,
            last_window_title: None,
            input: None,
            pending_output: Vec::new(),
            pending_refresh: false,
            selection: None,
            marked_text: None,
            marked_selected_range_utf16: 0..0,
            font: crate::default_terminal_font(),
        }
        .with_refreshed_viewport()
    }

    fn on_tab(&mut self, _: &Tab, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(false, cx);
    }

    fn on_tab_prev(&mut self, _: &TabPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(true, cx);
    }

    fn send_tab(&mut self, reverse: bool, cx: &mut Context<Self>) {
        if reverse {
            self.send_input_parts(&[b"\x1b[Z"], cx);
        } else {
            self.send_input_parts(&[b"\t"], cx);
        }
    }

    pub fn new_with_input(
        session: TerminalSession,
        focus_handle: FocusHandle,
        input: TerminalInput,
    ) -> Self {
        Self {
            session,
            viewport_lines: Vec::new(),
            viewport_line_offsets: Vec::new(),
            viewport_total_len: 0,
            viewport_style_runs: Vec::new(),
            line_layouts: Vec::new(),
            line_layout_key: None,
            last_bounds: None,
            focus_handle,
            last_window_title: None,
            input: Some(input),
            pending_output: Vec::new(),
            pending_refresh: false,
            selection: None,
            marked_text: None,
            marked_selected_range_utf16: 0..0,
            font: crate::default_terminal_font(),
        }
        .with_refreshed_viewport()
    }

    fn send_input_parts(&mut self, parts: &[&[u8]], cx: &mut Context<Self>) {
        if parts.is_empty() {
            return;
        }

        if let Some(input) = self.input.as_ref() {
            for bytes in parts {
                input.send(bytes);
            }
            return;
        }

        for bytes in parts {
            let _ = self.session.feed(bytes);
        }
        self.apply_side_effects(cx);
        self.schedule_viewport_refresh(cx);
    }

    fn feed_output_bytes_to_session(&mut self, bytes: &[u8]) {
        if let Some(input) = self.input.as_ref() {
            let _ = self
                .session
                .feed_with_pty_responses(bytes, |resp| input.send(resp));
        } else {
            let _ = self.session.feed(bytes);
        }
    }

    fn sync_viewport_scroll_tracking(&mut self) {
        let _ = self.session.take_viewport_scroll_delta();
    }

    fn apply_viewport_scroll_delta(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }

        let rows = self.session.rows() as usize;
        if rows == 0 {
            return;
        }

        if self.viewport_lines.len() != rows || self.viewport_style_runs.len() != rows {
            self.refresh_viewport();
            return;
        }

        let delta_abs: usize = delta.unsigned_abs() as usize;
        if delta_abs == 0 {
            return;
        }
        if delta_abs >= rows {
            self.refresh_viewport();
            return;
        }

        let has_layouts = self.line_layouts.len() == rows;

        if delta > 0 {
            self.viewport_lines.rotate_left(delta_abs);
            self.viewport_style_runs.rotate_left(delta_abs);
            if has_layouts {
                self.line_layouts.rotate_left(delta_abs);
            }

            for idx in rows - delta_abs..rows {
                self.viewport_lines[idx].clear();
                self.viewport_style_runs[idx].clear();
                if has_layouts {
                    self.line_layouts[idx] = None;
                }
            }

            let dirty_rows: Vec<u16> = (rows - delta_abs..rows).map(|row| row as u16).collect();
            let _ = self.apply_dirty_viewport_rows(&dirty_rows);
            return;
        }

        self.viewport_lines.rotate_right(delta_abs);
        self.viewport_style_runs.rotate_right(delta_abs);
        if has_layouts {
            self.line_layouts.rotate_right(delta_abs);
        }

        for idx in 0..delta_abs {
            self.viewport_lines[idx].clear();
            self.viewport_style_runs[idx].clear();
            if has_layouts {
                self.line_layouts[idx] = None;
            }
        }

        let dirty_rows: Vec<u16> = (0..delta_abs).map(|row| row as u16).collect();
        let _ = self.apply_dirty_viewport_rows(&dirty_rows);
    }

    fn reconcile_dirty_viewport_after_output(&mut self) {
        let delta = self.session.take_viewport_scroll_delta();
        self.apply_viewport_scroll_delta(delta);

        let dirty = self.session.take_dirty_viewport_rows();
        if !dirty.is_empty() && !self.apply_dirty_viewport_rows(&dirty) {
            self.pending_refresh = true;
        }
    }

    fn with_refreshed_viewport(mut self) -> Self {
        self.refresh_viewport();
        self
    }

    fn refresh_viewport(&mut self) {
        let viewport = self.session.dump_viewport().unwrap_or_default();
        self.viewport_lines = split_viewport_lines(&viewport);
        self.viewport_line_offsets = Self::compute_viewport_line_offsets(&self.viewport_lines);
        self.viewport_total_len = Self::compute_viewport_total_len(&self.viewport_lines);
        self.viewport_style_runs = (0..self.session.rows())
            .map(|row| {
                self.session
                    .dump_viewport_row_style_runs(row)
                    .unwrap_or_default()
            })
            .collect();
        self.line_layouts.clear();
        self.line_layout_key = None;
        self.selection = None;
    }

    fn compute_viewport_line_offsets(lines: &[String]) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(lines.len());
        let mut offset = 0usize;
        for line in lines {
            offsets.push(offset);
            offset = offset.saturating_add(line.len() + 1);
        }
        offsets
    }

    fn compute_viewport_total_len(lines: &[String]) -> usize {
        lines
            .iter()
            .fold(0usize, |acc, line| acc.saturating_add(line.len() + 1))
    }

    fn viewport_slice(&self, range: Range<usize>) -> String {
        if range.is_empty() || self.viewport_lines.is_empty() {
            return String::new();
        }

        let start = range.start.min(self.viewport_total_len);
        let end = range.end.min(self.viewport_total_len);
        if start >= end {
            return String::new();
        }

        let mut out = String::new();
        let mut i = 0usize;
        while i < self.viewport_lines.len() {
            let line_start = *self.viewport_line_offsets.get(i).unwrap_or(&0);
            let line = &self.viewport_lines[i];
            let line_end = line_start.saturating_add(line.len());
            let newline_pos = line_end;

            let seg_start = start.max(line_start);
            let seg_end = end.min(newline_pos.saturating_add(1));
            if seg_start < seg_end {
                let local_start = seg_start.saturating_sub(line_start);
                let local_end = seg_end.saturating_sub(line_start);
                let local_end = local_end.min(line.len().saturating_add(1));

                if local_start < line.len() {
                    let text_end = local_end.min(line.len());
                    if let Some(seg) = line.get(local_start..text_end) {
                        out.push_str(seg);
                    }
                }
                if local_end > line.len() {
                    out.push('\n');
                }
            }

            i += 1;
        }

        out
    }

    fn apply_dirty_viewport_rows(&mut self, dirty_rows: &[u16]) -> bool {
        if dirty_rows.is_empty() {
            return false;
        }

        let expected_rows = self.session.rows() as usize;
        if self.viewport_lines.len() != expected_rows {
            self.refresh_viewport();
            return true;
        }
        if self.viewport_style_runs.len() != expected_rows {
            self.refresh_viewport();
            return true;
        }

        for &row in dirty_rows {
            let row = row as usize;
            if row >= self.viewport_lines.len() {
                continue;
            }

            let line = match self.session.dump_viewport_row(row as u16) {
                Ok(s) => s,
                Err(_) => {
                    self.refresh_viewport();
                    return true;
                }
            };

            let line = line.strip_suffix('\n').unwrap_or(line.as_str());
            self.viewport_lines[row].clear();
            self.viewport_lines[row].push_str(line);
            self.viewport_style_runs[row] = self
                .session
                .dump_viewport_row_style_runs(row as u16)
                .unwrap_or_default();
            if row < self.line_layouts.len() {
                self.line_layouts[row] = None;
            }
        }

        self.viewport_line_offsets = Self::compute_viewport_line_offsets(&self.viewport_lines);
        self.viewport_total_len = Self::compute_viewport_total_len(&self.viewport_lines);
        self.selection = None;
        true
    }

    fn schedule_viewport_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_refresh = true;
        cx.notify();
    }

    fn apply_side_effects(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.session.take_clipboard_write() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn feed_output_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.feed_output_bytes_to_session(bytes);
        self.refresh_viewport();
        self.apply_side_effects(cx);
        cx.notify();
    }

    pub fn queue_output_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        const MAX_PENDING_OUTPUT_BYTES: usize = 256 * 1024;

        if self.pending_output.len().saturating_add(bytes.len()) <= MAX_PENDING_OUTPUT_BYTES {
            self.pending_output.extend_from_slice(bytes);
            cx.notify();
            return;
        }

        if !self.pending_output.is_empty() {
            let pending = std::mem::take(&mut self.pending_output);
            self.feed_output_bytes_to_session(&pending);
            self.apply_side_effects(cx);
            self.reconcile_dirty_viewport_after_output();
        }

        if bytes.len() > MAX_PENDING_OUTPUT_BYTES {
            let mut offset = 0usize;
            while offset < bytes.len() {
                let end = (offset + MAX_PENDING_OUTPUT_BYTES).min(bytes.len());
                self.feed_output_bytes_to_session(&bytes[offset..end]);
                offset = end;
            }
            self.apply_side_effects(cx);
            self.reconcile_dirty_viewport_after_output();
            cx.notify();
            return;
        }

        self.pending_output.extend_from_slice(bytes);
        cx.notify();
    }

    pub fn resize_terminal(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let _ = self.session.resize(cols, rows);
        self.sync_viewport_scroll_tracking();
        self.pending_refresh = true;
        cx.notify();
    }

    fn on_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };

        if self.session.bracketed_paste_enabled() {
            self.send_input_parts(&[b"\x1b[200~", text.as_bytes(), b"\x1b[201~"], cx);
        } else {
            self.send_input_parts(&[text.as_bytes()], cx);
        }
    }

    fn on_copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let selection = self
            .selection
            .map(|s| s.range())
            .filter(|range| !range.is_empty())
            .map(|range| self.viewport_slice(range))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.viewport_slice(0..self.viewport_total_len));

        let item = ClipboardItem::new_string(selection.to_string());
        cx.write_to_clipboard(item.clone());
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        cx.write_to_primary(item);
    }

    fn on_select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = Some(ByteSelection {
            anchor: 0,
            active: self.viewport_total_len,
        });
        self.on_copy(&Copy, window, cx);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let raw_keystroke = event.keystroke.clone();
        if should_skip_key_down_for_ime(self.input.is_some(), &raw_keystroke) {
            return;
        }
        let keystroke = raw_keystroke.with_simulated_ime();

        if keystroke.modifiers.platform || keystroke.modifiers.function {
            return;
        }

        let scroll_step = (self.session.rows() as i32 / 2).max(1);

        if let Some(input) = self.input.as_ref() {
            if keystroke.modifiers.shift {
                match keystroke.key.as_str() {
                    "home" => {
                        let _ = self.session.scroll_viewport_top();
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "end" => {
                        let _ = self.session.scroll_viewport_bottom();
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "pageup" | "page_up" | "page-up" => {
                        let _ = self.session.scroll_viewport(-scroll_step);
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "pagedown" | "page_down" | "page-down" => {
                        let _ = self.session.scroll_viewport(scroll_step);
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    _ => {}
                }
            }

            if keystroke.modifiers.control
                && let Some(b) = ctrl_byte_for_keystroke(&keystroke)
            {
                input.send(&[b]);
                return;
            }

            if keystroke.modifiers.alt
                && let Some(text) = keystroke.key_char.as_deref()
            {
                input.send(&[0x1b]);
                input.send(text.as_bytes());
                return;
            }

            let modifiers = KeyModifiers {
                shift: keystroke.modifiers.shift,
                control: keystroke.modifiers.control,
                alt: keystroke.modifiers.alt,
                super_key: false,
            };
            if let Some(encoded) = encode_key_named(&keystroke.key, modifiers) {
                input.send(&encoded);
                return;
            }
            return;
        }

        match keystroke.key.as_str() {
            "home" => {
                let _ = self.session.scroll_viewport_top();
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "end" => {
                let _ = self.session.scroll_viewport_bottom();
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "pageup" | "page_up" | "page-up" => {
                let _ = self.session.scroll_viewport(-scroll_step);
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "pagedown" | "page_down" | "page-down" => {
                let _ = self.session.scroll_viewport(scroll_step);
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            _ => {}
        }

        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            super_key: false,
        };
        if let Some(encoded) = encode_key_named(&keystroke.key, modifiers) {
            let _ = self.session.feed(&encoded);
            self.apply_side_effects(cx);
            self.schedule_viewport_refresh(cx);
            return;
        }

        if keystroke.key == "backspace" {
            if let Some(input) = self.input.as_ref() {
                input.send(&[0x7f]);
                return;
            }
            let _ = self.session.feed(&[0x08]);
            self.apply_side_effects(cx);
            self.schedule_viewport_refresh(cx);
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ensure_key_bindings(cx);

        if !self.pending_output.is_empty() {
            let bytes = std::mem::take(&mut self.pending_output);
            self.feed_output_bytes_to_session(&bytes);
            self.apply_side_effects(cx);
            self.reconcile_dirty_viewport_after_output();
        }

        if self.pending_refresh {
            self.refresh_viewport();
            self.pending_refresh = false;
        }

        if self.session.window_title_updates_enabled() {
            let title = self
                .session
                .title()
                .unwrap_or("GPUI Embedded Terminal (Ghostty VT)");

            if self.last_window_title.as_deref() != Some(title) {
                window.set_window_title(title);
                self.last_window_title = Some(title.to_string());
            }
        }

        div()
            .size_full()
            .flex()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_tab_prev))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .bg(gpui::black())
            .text_color(gpui::white())
            .font(self.font.clone())
            .whitespace_nowrap()
            .child(terminal_text_element(cx.entity()))
    }
}
