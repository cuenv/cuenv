//! The confirmation prompt shown before changes are applied.

use std::future::Future;
use std::io::IsTerminal;
use std::pin::Pin;

use cuenv_infrastructure::{PlanMode, strip_control_characters};

use super::action::{ConfirmationPolicy, InfrastructureAction};
use super::dispatch::CommandContext;
use super::invocation::Invocation;
use super::output;
use super::output::Output;
use crate::cli::{CliError, InfrastructureFailureKind};

/// What a run's confirmation prompt depends on, known before anything is
/// evaluated.
pub(super) struct PromptSetting<'setting> {
    pub(super) output: &'setting Output,
    pub(super) answers: &'setting dyn Answers,
    pub(super) invocation: &'setting Invocation,
}

/// Refuse a prompt that cannot be answered before evaluating anything.
pub(super) fn refuse_unconfirmable(
    action: &InfrastructureAction,
    prompt: &PromptSetting<'_>,
) -> Result<(), CliError> {
    let PromptSetting {
        output,
        answers,
        invocation,
    } = prompt;
    if action.confirmation() == ConfirmationPolicy::AssumeYes {
        return Ok(());
    }
    if output.is_json() {
        return Err(CliError::config_with_help(
            "--json needs --yes for apply, destroy and state restore: the confirmation prompt \
             cannot share standard output with the JSON result",
            format!(
                "Review `{} --json` first, then run with --yes.",
                invocation.command("plan")
            ),
        ));
    }
    if !answers.is_interactive() {
        return Err(CliError::config_with_help(
            "refusing to change infrastructure without confirmation: standard input is not a \
             terminal",
            "Pass --yes to apply without the prompt.",
        ));
    }
    Ok(())
}

/// A line read at the confirmation prompt: `None` at the end of input.
pub(super) type AnswerFuture<'answers> =
    Pin<Box<dyn Future<Output = std::io::Result<Option<String>>> + Send + 'answers>>;

/// Where the answer to the confirmation prompt comes from.
pub(super) trait Answers: Send + Sync {
    /// Whether anyone can answer (standard input is a terminal).
    fn is_interactive(&self) -> bool;

    /// Read one answer without blocking the runtime.
    fn read(&self) -> AnswerFuture<'_>;
}

/// Answers typed on the terminal.
#[derive(Debug)]
pub(super) struct TerminalAnswers;

impl Answers for TerminalAnswers {
    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn read(&self) -> AnswerFuture<'_> {
        Box::pin(async {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // A detached thread, so an interrupt at the prompt never waits
            // on a blocking read; nothing joins it.
            std::thread::Builder::new()
                .name("cuenv-infrastructure-prompt".to_string())
                .spawn(move || {
                    let mut line = String::new();
                    let read = std::io::stdin()
                        .read_line(&mut line)
                        .map(|bytes| (bytes > 0).then_some(line));
                    // The prompt is abandoned after an interrupt; the
                    // answer is meant to be dropped then.
                    let _ = sender.send(read);
                })?;
            receiver
                .await
                .map_err(|_| std::io::Error::other("the prompt reader stopped"))?
        })
    }
}

/// Identifier of the confirmation prompt in prompt events.
pub(super) const PROMPT_IDENTIFIER: &str = "infrastructure-confirmation";

/// The error for an operation that was not confirmed.
pub(super) fn cancelled_operation(operation: &str, reason: &str) -> CliError {
    CliError::infrastructure(
        format!("{operation} cancelled: {reason}; nothing was applied"),
        Some(
            "Answer 'yes' at the prompt to go ahead, or pass --yes to skip the prompt.".to_string(),
        ),
        InfrastructureFailureKind::Cancelled,
    )
}

/// Ask for confirmation of the plan shown, while the lock is held. Only
/// `yes` confirms; any other answer, the end of input, or an unreadable
/// answer cancels, and an interrupt stops the run.
pub(super) async fn confirm(mode: PlanMode, context: &CommandContext<'_>) -> Result<(), CliError> {
    confirm_operation(
        context,
        output::operation_name(mode),
        match mode {
            PlanMode::Apply => "Type 'yes' to apply these changes:",
            PlanMode::Destroy => "Type 'yes' to destroy every resource listed above:",
        },
    )
    .await
}

/// Ask `question`; only `yes` confirms `operation`.
pub(super) async fn confirm_operation(
    context: &CommandContext<'_>,
    operation: &str,
    question: &str,
) -> Result<(), CliError> {
    cuenv_events::emit_prompt_requested!(PROMPT_IDENTIFIER, question, Vec::<String>::new());
    let answer = context
        .interrupts
        .until_interrupted(context.answers.read())
        .await?;
    match answer {
        Ok(Some(line)) => {
            let answer = strip_control_characters(line.trim());
            cuenv_events::emit_prompt_resolved!(PROMPT_IDENTIFIER, answer.as_str());
            if answer == "yes" {
                Ok(())
            } else {
                Err(cancelled_operation(operation, "the answer was not 'yes'"))
            }
        }
        Ok(None) => Err(cancelled_operation(
            operation,
            "standard input ended without an answer",
        )),
        Err(error) => Err(cancelled_operation(
            operation,
            &format!("the answer could not be read ({error})"),
        )),
    }
}
