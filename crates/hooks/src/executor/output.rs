//! Bounded process output capture and direct-child cleanup.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::{Error, Result};

pub(super) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// A callback that receives each line a hook writes to stderr, as it is
/// written, while the output is still captured in full.
///
/// Runtime activation uses it to show what `nix print-dev-env` is doing.
pub type StderrLines = Arc<dyn Fn(&str) + Send + Sync>;

/// How [`run_command_with`] runs a command.
pub(super) struct RunCommandOptions<'run> {
    /// Bytes written to the command's stdin.
    pub(super) input: Option<&'run [u8]>,
    /// Time limit for the command and its output.
    pub(super) timeout: Duration,
    /// Receives each stderr line as it is written.
    pub(super) stderr_lines: Option<StderrLines>,
}

pub(super) struct OutputReaders {
    stdout: JoinHandle<Result<Vec<u8>>>,
    stderr: JoinHandle<Result<Vec<u8>>>,
    failures: tokio::sync::mpsc::Receiver<()>,
}

impl OutputReaders {
    pub(super) fn new(child: &mut Child, stderr_lines: Option<StderrLines>) -> Result<Self> {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::process("Hook stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::process("Hook stderr was not captured"))?;
        let budget = Arc::new(AtomicUsize::new(0));
        let (sender, failures) = tokio::sync::mpsc::channel(2);
        Ok(Self {
            stdout: spawn_reader(stdout, ReaderLimits::new(&budget, &sender), None),
            stderr: spawn_reader(stderr, ReaderLimits::new(&budget, &sender), stderr_lines),
            failures,
        })
    }

    pub(super) async fn failed(&mut self) -> Option<()> {
        self.failures.recv().await
    }

    pub(super) async fn collect(&mut self) -> Result<(Vec<u8>, Vec<u8>)> {
        let stdout = (&mut self.stdout)
            .await
            .map_err(|_| Error::process("Hook stdout task failed"))??;
        let stderr = (&mut self.stderr)
            .await
            .map_err(|_| Error::process("Hook stderr task failed"))??;
        Ok((stdout, stderr))
    }

    pub(super) fn abort(&self) {
        self.stdout.abort();
        self.stderr.abort();
    }

    pub(super) fn abort_handles(&self) -> (tokio::task::AbortHandle, tokio::task::AbortHandle) {
        (self.stdout.abort_handle(), self.stderr.abort_handle())
    }
}

impl Drop for OutputReaders {
    fn drop(&mut self) {
        self.abort();
    }
}

/// The capture budget shared by a command's readers, and where a reader
/// reports that it failed.
struct ReaderLimits {
    budget: Arc<AtomicUsize>,
    failures: tokio::sync::mpsc::Sender<()>,
}

impl ReaderLimits {
    fn new(budget: &Arc<AtomicUsize>, failures: &tokio::sync::mpsc::Sender<()>) -> Self {
        Self {
            budget: budget.clone(),
            failures: failures.clone(),
        }
    }
}

/// Splits captured bytes into lines for a [`StderrLines`] callback.
struct LineSplitter {
    lines: StderrLines,
    pending: Vec<u8>,
}

impl LineSplitter {
    const fn new(lines: StderrLines) -> Self {
        Self {
            lines,
            pending: Vec::new(),
        }
    }

    /// Feed bytes; each completed line (ended by `\n` or `\r`) is passed on.
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                self.flush();
            } else {
                self.pending.push(byte);
            }
        }
    }

    /// Pass on the final line when it has no line ending.
    fn flush(&mut self) {
        if !self.pending.is_empty() {
            (self.lines)(&String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
    }
}

fn spawn_reader(
    mut input: impl AsyncRead + Unpin + Send + 'static,
    limits: ReaderLimits,
    lines: Option<StderrLines>,
) -> JoinHandle<Result<Vec<u8>>> {
    let ReaderLimits { budget, failures } = limits;
    tokio::spawn(async move {
        let mut splitter = lines.map(LineSplitter::new);
        let result = async {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let count = input
                    .read(&mut buffer)
                    .await
                    .map_err(|_| Error::process("Cannot read hook output"))?;
                if count == 0 {
                    if let Some(splitter) = splitter.as_mut() {
                        splitter.flush();
                    }
                    return Ok(output);
                }
                budget
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                        used.checked_add(count)
                            .filter(|total| *total <= MAX_OUTPUT_BYTES)
                    })
                    .map_err(|_| Error::process("Hook output exceeds the 16 MiB capture limit"))?;
                if let Some(splitter) = splitter.as_mut() {
                    splitter.feed(&buffer[..count]);
                }
                output.extend_from_slice(&buffer[..count]);
            }
        }
        .await;
        if result.is_err() {
            let _ = failures.send(()).await;
        }
        result
    })
}

pub(super) async fn stop_child(child: &mut Child) -> Result<()> {
    if child
        .try_wait()
        .map_err(|error| Error::process(format!("Failed to inspect hook child: {error}")))?
        .is_none()
        && let Err(error) = child.kill().await
        && child
            .try_wait()
            .map_err(|wait_error| {
                Error::process(format!("Failed to inspect hook child: {wait_error}"))
            })?
            .is_none()
    {
        return Err(Error::process(format!(
            "Failed to stop hook child: {error}"
        )));
    }
    child
        .wait()
        .await
        .map_err(|error| Error::process(format!("Failed to reap hook child: {error}")))?;
    Ok(())
}

pub(super) async fn run_command(
    command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Option<std::process::Output>> {
    run_command_with(
        command,
        RunCommandOptions {
            input,
            timeout,
            stderr_lines: None,
        },
    )
    .await
}

pub(super) async fn run_command_with(
    mut command: Command,
    options: RunCommandOptions<'_>,
) -> Result<Option<std::process::Output>> {
    use std::process::Stdio;

    let RunCommandOptions {
        input,
        timeout,
        stderr_lines,
    } = options;

    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().map_err(|_| Error::ProcessStart)?;
    let input_task = if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::process("Hook input was not captured"))?;
        let input = input.to_vec();
        Some(tokio::spawn(async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await
        }))
    } else {
        None
    };
    let input_abort = input_task.as_ref().map(JoinHandle::abort_handle);
    let mut readers = OutputReaders::new(&mut child, stderr_lines)?;
    let deadline = tokio::time::Instant::now() + timeout;
    let status = tokio::select! {
        status = child.wait() => status.map_err(|_| Error::process("Cannot wait for hook process"))?,
        Some(()) = readers.failed() => {
            stop_child(&mut child).await?;
            if let Some(abort) = input_abort { abort.abort(); }
            return Err(Error::process("Hook output could not be captured within the limit"));
        }
        () = tokio::time::sleep_until(deadline) => {
            stop_child(&mut child).await?;
            if let Some(abort) = input_abort { abort.abort(); }
            return Ok(None);
        }
    };
    let output = async {
        if let Some(input_task) = input_task {
            input_task
                .await
                .map_err(|_| Error::process("Hook input task failed"))?
                .map_err(|_| Error::process("Cannot deliver hook input"))?;
        }
        readers.collect().await
    };
    let (stdout, stderr) = if let Ok(result) = tokio::time::timeout_at(deadline, output).await {
        result?
    } else {
        if let Some(abort) = input_abort {
            abort.abort();
        }
        return Ok(None);
    };
    Ok(Some(std::process::Output {
        status,
        stdout,
        stderr,
    }))
}
