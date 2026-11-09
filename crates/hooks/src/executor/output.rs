//! Bounded process output capture and direct-child cleanup.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::{Error, Result};

pub(super) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct OutputReaders {
    stdout: JoinHandle<Result<Vec<u8>>>,
    stderr: JoinHandle<Result<Vec<u8>>>,
    failures: tokio::sync::mpsc::Receiver<()>,
}

impl OutputReaders {
    pub(super) fn new(child: &mut Child) -> Result<Self> {
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
            stdout: spawn_reader(stdout, budget.clone(), sender.clone()),
            stderr: spawn_reader(stderr, budget, sender),
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

fn spawn_reader(
    mut input: impl AsyncRead + Unpin + Send + 'static,
    budget: Arc<AtomicUsize>,
    failures: tokio::sync::mpsc::Sender<()>,
) -> JoinHandle<Result<Vec<u8>>> {
    tokio::spawn(async move {
        let result = async {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 8192];
            loop {
                let count = input
                    .read(&mut buffer)
                    .await
                    .map_err(|_| Error::process("Cannot read hook output"))?;
                if count == 0 {
                    return Ok(output);
                }
                budget
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                        used.checked_add(count)
                            .filter(|total| *total <= MAX_OUTPUT_BYTES)
                    })
                    .map_err(|_| Error::process("Hook output exceeds the 16 MiB capture limit"))?;
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
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Option<std::process::Output>> {
    use std::process::Stdio;

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
    let mut readers = OutputReaders::new(&mut child)?;
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
