//! Searchable commands and Cuenv tasks shown by the command palette.

use crate::integrations::cuenv::CuenvTask;

/// Commands name real actions the host can perform today.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Command {
    NewTab,
    CloseTab,
    RenameTab,
    NextTab,
    PreviousTab,
    ToggleSidebar,
    FindOnScreen,
}

const COMMANDS: &[Command] = &[
    Command::NewTab,
    Command::CloseTab,
    Command::RenameTab,
    Command::NextTab,
    Command::PreviousTab,
    Command::ToggleSidebar,
    Command::FindOnScreen,
];

impl Command {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::NewTab => "New tab",
            Self::CloseTab => "Close tab",
            Self::RenameTab => "Rename tab",
            Self::NextTab => "Next tab",
            Self::PreviousTab => "Previous tab",
            Self::ToggleSidebar => "Toggle sidebar",
            Self::FindOnScreen => "Find on screen",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::NewTab => "Open a new terminal tab",
            Self::CloseTab => "Close the current tab",
            Self::RenameTab => "Change the current tab's name",
            Self::NextTab => "Switch to the next tab",
            Self::PreviousTab => "Switch to the previous tab",
            Self::ToggleSidebar => "Collapse or expand the tab sidebar",
            Self::FindOnScreen => "Search the visible terminal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mode {
    Commands,
    Tasks,
}

impl Mode {
    pub(super) fn from_query(query: &str) -> Self {
        if query.trim_start().starts_with('$') {
            Self::Tasks
        } else {
            Self::Commands
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Item {
    Command(Command),
    Task(CuenvTask),
}

/// The active tab determines which actions can actually be performed.
#[derive(Clone, Copy)]
pub(super) struct Available<'selection> {
    pub(super) tasks: &'selection [CuenvTask],
    pub(super) tab: bool,
    pub(super) screen: bool,
}

/// A leading `$` selects the active pane's tasks; deleting it returns to commands.
pub(super) fn results(query: &str, available: Available<'_>) -> Vec<Item> {
    let query = query.trim();
    match Mode::from_query(query) {
        Mode::Commands => {
            let query = query.to_lowercase();
            COMMANDS
                .iter()
                .copied()
                .filter(|command| {
                    let possible = match command {
                        Command::NewTab | Command::ToggleSidebar => true,
                        Command::FindOnScreen => available.screen,
                        _ => available.tab,
                    };
                    possible
                        && (command.name().to_lowercase().contains(&query)
                            || command.description().to_lowercase().contains(&query))
                })
                .map(Item::Command)
                .collect()
        }
        Mode::Tasks => {
            let query = query
                .strip_prefix('$')
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            available
                .tasks
                .iter()
                .filter(|task| {
                    task.name.to_lowercase().contains(&query)
                        || task
                            .description
                            .as_deref()
                            .is_some_and(|description| description.to_lowercase().contains(&query))
                })
                .cloned()
                .map(Item::Task)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::cuenv::CuenvTaskKind;

    fn task(name: &str, description: &str) -> CuenvTask {
        CuenvTask {
            name: name.into(),
            description: Some(description.into()),
            kind: CuenvTaskKind::Task,
            requires_parameters: false,
        }
    }

    fn available(tasks: &[CuenvTask]) -> Available<'_> {
        Available {
            tasks,
            tab: true,
            screen: true,
        }
    }

    #[test]
    fn commands_are_the_default_and_match_names_or_descriptions() {
        let tasks = [task("build", "Build the website")];
        assert!(
            results("", available(&tasks))
                .iter()
                .all(|item| matches!(item, Item::Command(_)))
        );
        assert_eq!(
            results("NEW TAB", available(&tasks)),
            vec![Item::Command(Command::NewTab)]
        );
        assert_eq!(
            results("visible terminal", available(&tasks)),
            vec![Item::Command(Command::FindOnScreen)]
        );
    }

    #[test]
    fn a_dollar_selects_tasks_and_removing_it_returns_to_commands() {
        let tasks = [
            task("docs.build", "Build the website"),
            task("lint", "Check code"),
        ];
        assert_eq!(Mode::from_query("$"), Mode::Tasks);
        assert_eq!(
            results("$ WEBSITE", available(&tasks)),
            vec![Item::Task(tasks[0].clone())]
        );
        assert_eq!(results("$", available(&tasks)).len(), 2);
        assert_eq!(Mode::from_query(""), Mode::Commands);
        assert!(results("$ missing", available(&tasks)).is_empty());
    }

    #[test]
    fn every_matching_task_remains_reachable() {
        let tasks: Vec<_> = (0..15)
            .map(|index| task(&format!("task-{index}"), ""))
            .collect();
        assert_eq!(results("$", available(&tasks)).len(), 15);
    }

    #[test]
    fn an_empty_window_offers_only_actions_it_can_perform() {
        let available = Available {
            tasks: &[],
            tab: false,
            screen: false,
        };
        assert_eq!(
            results("", available),
            vec![
                Item::Command(Command::NewTab),
                Item::Command(Command::ToggleSidebar)
            ]
        );
        assert!(results("$", available).is_empty());
    }
}
