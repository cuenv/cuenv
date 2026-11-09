//! Verify output through the public asynchronous command executor.

use cuenv::commands::{Command, CommandExecutor};
use cuenv_events::{EventBus, EventCategory, EventSender, OutputEvent};
use std::error::Error;
use std::fs;
use std::time::Duration;

struct RestoreSender(Option<EventSender>);

impl Drop for RestoreSender {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            cuenv_events::set_global_sender(sender);
        } else {
            cuenv_events::clear_global_sender();
        }
    }
}

#[tokio::test]
async fn async_env_print_and_list_emit_their_results() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    fs::create_dir(directory.path().join("cue.mod"))?;
    fs::write(
        directory.path().join("cue.mod/module.cue"),
        "module: \"test.example/env-dispatch\"\nlanguage: version: \"v0.9.0\"\n",
    )?;
    fs::write(
        directory.path().join("env.cue"),
        "package cuenv\nname: \"test\"\nenv: {GREETING: \"hello\", environment: dev: {GREETING: \"dev\"}}\n",
    )?;
    let bus = EventBus::new();
    let mut receiver = bus.subscribe();
    let _restore = RestoreSender(cuenv_events::global_sender());
    cuenv_events::set_global_sender(bus.sender().ok_or("event bus is closed")?);
    let executor = CommandExecutor::new("cuenv".to_string());
    let path = directory.path().to_string_lossy().into_owned();
    executor
        .execute(Command::EnvPrint {
            path: path.clone(),
            package: "cuenv".to_string(),
            format: "env".to_string(),
            environment: None,
        })
        .await?;
    executor
        .execute(Command::EnvList {
            path,
            package: "cuenv".to_string(),
            format: "simple".to_string(),
        })
        .await?;
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        let mut output = Vec::new();
        while output.len() < 2 {
            let event = receiver.recv().await.ok_or("event stream ended")?;
            if let EventCategory::Output(OutputEvent::Stdout { content }) = &event.category {
                output.push(content.clone());
            }
        }
        Ok::<_, Box<dyn Error>>(output)
    })
    .await??;
    assert_eq!(output, ["GREETING=hello", "dev"]);
    Ok(())
}
