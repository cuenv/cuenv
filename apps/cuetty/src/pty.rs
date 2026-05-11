use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context as _, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const DEFAULT_SHELL: &str = "/bin/sh";
const DEFAULT_TERM: &str = "xterm-256color";
const DEFAULT_COLORTERM: &str = "truecolor";
const DEFAULT_TERM_PROGRAM: &str = "cuetty";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtyGridSize {
    pub cols: u16,
    pub rows: u16,
}

impl PtyGridSize {
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    pub fn from_metrics(metrics: GridMetrics) -> Self {
        let cols = (metrics.pixel_width / metrics.cell_width.max(1.0))
            .floor()
            .max(1.0) as u16;
        let rows = (metrics.pixel_height / metrics.cell_height.max(1.0))
            .floor()
            .max(1.0) as u16;
        Self { cols, rows }
    }

    pub const fn to_pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMetrics {
    pub pixel_width: f32,
    pub pixel_height: f32,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl Default for PtyGridSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalEnvironment {
    pub term: String,
    pub colorterm: String,
    pub term_program: String,
}

impl TerminalEnvironment {
    pub fn as_pairs(&self) -> [(&str, &str); 3] {
        [
            ("TERM", self.term.as_str()),
            ("COLORTERM", self.colorterm.as_str()),
            ("TERM_PROGRAM", self.term_program.as_str()),
        ]
    }
}

impl Default for TerminalEnvironment {
    fn default() -> Self {
        Self {
            term: DEFAULT_TERM.to_string(),
            colorterm: DEFAULT_COLORTERM.to_string(),
            term_program: DEFAULT_TERM_PROGRAM.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalProcessOptions {
    pub shell: PathBuf,
    pub grid_size: PtyGridSize,
    pub environment: TerminalEnvironment,
}

impl Default for TerminalProcessOptions {
    fn default() -> Self {
        Self {
            shell: shell_path_from_env(std::env::var_os("SHELL")),
            grid_size: PtyGridSize::default(),
            environment: TerminalEnvironment::default(),
        }
    }
}

pub struct TerminalProcess {
    pub stdin_tx: Sender<Vec<u8>>,
    pub stdout_rx: Receiver<Vec<u8>>,
    pub master: Arc<dyn MasterPty + Send>,
}

impl TerminalProcess {
    pub fn spawn(options: TerminalProcessOptions) -> Result<Self> {
        let pty_system = native_pty_system();
        let pty_pair = pty_system
            .openpty(options.grid_size.to_pty_size())
            .context("failed to open PTY")?;
        let master: Arc<dyn MasterPty + Send> = Arc::from(pty_pair.master);

        let mut child = pty_pair
            .slave
            .spawn_command(build_command(&options))
            .with_context(|| format!("failed to spawn login shell {}", options.shell.display()))?;

        spawn_named_thread("cuetty-child-wait", move || {
            if let Err(error) = child.wait() {
                tracing::debug!(%error, "terminal child wait failed");
            }
        })?;

        let mut pty_reader = master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let mut pty_writer = master.take_writer().context("failed to take PTY writer")?;
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
        let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>();

        spawn_named_thread("cuetty-pty-writer", move || {
            while let Ok(bytes) = stdin_rx.recv() {
                if pty_writer.write_all(&bytes).is_err() {
                    break;
                }
                if pty_writer.flush().is_err() {
                    break;
                }
            }
        })?;

        spawn_named_thread("cuetty-pty-reader", move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match pty_reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => {
                        if stdout_tx.send(buffer[..size].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "PTY reader stopped");
                        break;
                    }
                }
            }
        })?;

        Ok(Self {
            stdin_tx,
            stdout_rx,
            master,
        })
    }
}

pub fn shell_path_from_env(shell: Option<OsString>) -> PathBuf {
    shell
        .and_then(|value| {
            if value.is_empty() {
                None
            } else {
                Some(PathBuf::from(value))
            }
        })
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SHELL))
}

fn build_command(options: &TerminalProcessOptions) -> CommandBuilder {
    let mut command = CommandBuilder::new(options.shell.clone());
    command.arg("-l");
    for (key, value) in options.environment.as_pairs() {
        command.env(key, value);
    }
    command
}

fn spawn_named_thread(name: &str, task: impl FnOnce() + Send + 'static) -> Result<()> {
    let handle = thread::Builder::new()
        .name(name.to_string())
        .spawn(task)
        .with_context(|| format!("failed to spawn {name} thread"))?;
    drop(handle);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_path_from_env_falls_back_when_missing() {
        assert_eq!(shell_path_from_env(None), PathBuf::from(DEFAULT_SHELL));
    }

    #[test]
    fn shell_path_from_env_falls_back_when_empty() {
        assert_eq!(
            shell_path_from_env(Some(OsString::from(""))),
            PathBuf::from(DEFAULT_SHELL)
        );
    }

    #[test]
    fn terminal_environment_uses_cuetty_identity() {
        assert_eq!(
            TerminalEnvironment::default().as_pairs(),
            [
                ("TERM", DEFAULT_TERM),
                ("COLORTERM", DEFAULT_COLORTERM),
                ("TERM_PROGRAM", DEFAULT_TERM_PROGRAM),
            ]
        );
    }

    #[test]
    fn grid_size_never_drops_below_one_cell() {
        assert_eq!(
            PtyGridSize::from_metrics(GridMetrics {
                pixel_width: 0.0,
                pixel_height: 0.0,
                cell_width: 12.0,
                cell_height: 20.0,
            }),
            PtyGridSize::new(1, 1)
        );
    }

    #[test]
    fn grid_size_uses_cell_metrics() {
        assert_eq!(
            PtyGridSize::from_metrics(GridMetrics {
                pixel_width: 120.0,
                pixel_height: 60.0,
                cell_width: 10.0,
                cell_height: 20.0,
            }),
            PtyGridSize::new(12, 3)
        );
    }
}
