pub mod version;

use crate::events::{Event, EventSender};
use cuenv_core::Result;
use tokio::time::{sleep, Duration};
use tracing::{event, Level};

#[derive(Debug, Clone)]
pub enum Command {
    Version,
}

pub struct CommandExecutor {
    event_sender: EventSender,
}

impl CommandExecutor {
    pub fn new(event_sender: EventSender) -> Self {
        Self { event_sender }
    }

    pub async fn execute(&self, command: Command) -> Result<()> {
        match command {
            Command::Version => {
                self.execute_version().await
            }
        }
    }

    async fn execute_version(&self) -> Result<()> {
        let command_name = "version";
        
        // Send command start event
        self.send_event(Event::CommandStart {
            command: command_name.to_string(),
        });

        // Simulate some work with progress updates
        for i in 0..=5 {
            let progress = i as f32 / 5.0;
            let message = match i {
                0 => "Initializing...".to_string(),
                1 => "Loading version info...".to_string(),
                2 => "Checking build metadata...".to_string(),
                3 => "Gathering system info...".to_string(),
                4 => "Formatting output...".to_string(),
                5 => "Complete".to_string(),
                _ => "Processing...".to_string(),
            };
            
            self.send_event(Event::CommandProgress {
                command: command_name.to_string(),
                progress,
                message,
            });
            
            if i < 5 {
                sleep(Duration::from_millis(200)).await;
            }
        }

        // Get version information
        let version_info = version::get_version_info();
        
        // Send completion event
        self.send_event(Event::CommandComplete {
            command: command_name.to_string(),
            success: true,
            output: version_info,
        });

        Ok(())
    }

    fn send_event(&self, event: Event) {
        if let Err(e) = self.event_sender.send(event) {
            event!(Level::ERROR, "Failed to send event: {}", e);
        }
    }
}