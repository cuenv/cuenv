//! What a tab is called. Everything here is pure, so the naming rules can be
//! tested without a window or a terminal session.

use super::foreground::{ForegroundProcess, ShellRun};
use std::borrow::Cow;
use std::path::Path;
use std::time::{Duration, Instant};

/// The name of a tab nothing else can name.
const FALLBACK_LABEL: &str = "Terminal";

/// Foreground processes that may be sitting at a prompt, which their arguments
/// settle. A nested shell at a prompt says nothing about the tab that its title and
/// directory do not say better.
const SHELLS: &[&str] = &[
    "bash", "dash", "elvish", "fish", "ksh", "mksh", "nu", "pwsh", "sh", "tcsh", "xonsh", "zsh",
];

/// What a task launched from Cuetty shows up as in the foreground: the first word
/// of the command line it pastes.
const TASK_PROGRAM: &str = "cuenv";

/// The suffixes Nix's wrappers give the binaries they wrap, as in `.bat-wrapped`.
const WRAPPER_SUFFIXES: &[&str] = &["-wrapped", "-unwrapped"];

/// Bytes of a process name the Linux kernel keeps in `/proc/<pid>/comm`.
const COMM_LIMIT: usize = 15;

/// How long a launched task may go without a program appearing in the foreground
/// before it is assumed to have finished between two samples.
const TASK_START_GRACE: Duration = Duration::from_secs(3);

/// Everything a tab can be named after, in no particular order.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TabLabelSources<'label> {
    pub(super) custom_name: Option<&'label str>,
    pub(super) running_task: Option<&'label str>,
    pub(super) foreground_program: Option<&'label str>,
    pub(super) title: Option<&'label str>,
    pub(super) directory: Option<&'label Path>,
}

/// What a terminal title turned out to describe.
enum TitleName<'title> {
    /// A program chose this title, so it says more than the process name would.
    Program(&'title str),
    /// A shell prompt's directory, reduced to its last component. It only stands in
    /// for the working directory, which is exact, while that is not known.
    Directory(&'title str),
}

impl<'title> TitleName<'title> {
    fn parse(title: &'title str) -> Option<Self> {
        let title = title.trim();
        // A shell naming itself says nothing, and must not outrank what it runs.
        if title.is_empty() || is_shell(title) {
            return None;
        }
        Some(title_directory(title).map_or(Self::Program(title), |path| {
            Self::Directory(path_basename(path))
        }))
    }
}

/// The directory a shell's title names. The path has to lead the title, past any
/// `user@host:` prefix, because a program naming a file further along is not saying
/// where it is. It runs to the end of the title or to the `>` of the `path> command`
/// a shell sets while a command runs, so it may contain spaces.
fn title_directory(title: &str) -> Option<&str> {
    let is_path = |text: &&str| text.starts_with('/') || text.starts_with('~');
    let path = Some(title).filter(is_path).or_else(|| {
        title
            .split_once(':')
            .filter(|(prefix, _)| prefix.contains('@') && !prefix.contains(char::is_whitespace))
            .map(|(_, path)| path.trim_start())
            .filter(is_path)
    })?;
    Some(
        path.split_once('>')
            .map_or(path, |(path, _)| path)
            .trim_end(),
    )
}

fn path_basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn directory_name(directory: &Path) -> Cow<'_, str> {
    directory.file_name().map_or_else(
        || directory.to_string_lossy(),
        |name| name.to_string_lossy(),
    )
}

/// Names a tab, preferring what the user chose, then what the tab is doing, then
/// where it is.
pub(super) fn resolve_tab_label(sources: TabLabelSources<'_>) -> String {
    let title = sources.title.and_then(TitleName::parse);
    let program_title = match title {
        Some(TitleName::Program(title)) => Some(title),
        _ => None,
    };
    let named_directory = match title {
        Some(TitleName::Directory(directory)) => Some(directory),
        _ => None,
    };
    let directory = sources.directory.map(directory_name);
    nonblank(sources.custom_name)
        .or(nonblank(sources.running_task))
        .or(program_title)
        .or(nonblank(sources.foreground_program))
        .or(nonblank(directory.as_deref()))
        .or(named_directory)
        .unwrap_or(FALLBACK_LABEL)
        .to_string()
}

/// What a collapsed tab's mark is taken from, in no particular order.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TabMarkSources<'mark> {
    pub(super) custom_name: Option<&'mark str>,
    pub(super) banner: Option<&'mark str>,
    pub(super) directory: Option<&'mark Path>,
    pub(super) label: &'mark str,
}

/// The mark of a collapsed tab. A tab's label follows whatever is running, so the
/// mark comes from what the tab is for: its name or its project. That keeps the mark
/// still and tells two tabs running the same tool apart.
pub(super) fn tab_mark(sources: TabMarkSources<'_>) -> String {
    let directory = sources.directory.map(directory_name);
    tab_initial(
        nonblank(sources.custom_name)
            .or(nonblank(sources.banner))
            .or(nonblank(directory.as_deref()))
            .unwrap_or(sources.label),
    )
}

/// A one-character stand-in for `label` where only a short mark fits.
fn tab_initial(label: &str) -> String {
    label
        .chars()
        .find(|character| character.is_alphanumeric())
        .or_else(|| label.chars().next())
        .map_or_else(String::new, |initial| {
            initial.to_uppercase().take(1).collect()
        })
}

/// A tab's full name, with the project's banner when it adds anything.
pub(super) fn tab_tooltip(label: &str, banner: Option<&str>) -> String {
    match banner {
        Some(banner) if banner != label => format!("{label} · {banner}"),
        _ => label.to_string(),
    }
}

/// A tab's second line: where it is, then which branch that is on, leaving out the
/// directory when the tab is already named after it. Empty when neither is known, so
/// nothing stands in for what cannot be told.
pub(super) fn tab_subtitle(directory: Option<&Path>, branch: Option<&str>, label: &str) -> String {
    let directory = directory.map(directory_name);
    [
        nonblank(directory.as_deref()).filter(|directory| *directory != label),
        nonblank(branch),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// The name a person would know a binary by. Nix's wrappers run the real binary as
/// `.name-wrapped`, and `exec -a` does not change the process name the kernel
/// reports, which is cut short when it is long.
fn unwrapped_name(name: &str) -> &str {
    let bare = name.trim_start_matches('.');
    let truncated = bare.len() != name.len() && name.len() == COMM_LIMIT;
    WRAPPER_SUFFIXES
        .iter()
        .find_map(|suffix| bare.strip_suffix(suffix))
        .or_else(|| {
            truncated.then_some(())?;
            WRAPPER_SUFFIXES.iter().find_map(|suffix| {
                (1..suffix.len())
                    .rev()
                    .find_map(|kept| bare.strip_suffix(&suffix[..kept]))
            })
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or(if bare.is_empty() { name } else { bare })
}

/// Whether a name is a shell's. Login shells report themselves as `-zsh`.
pub(super) fn is_shell(name: &str) -> bool {
    SHELLS.contains(&unwrapped_name(name.strip_prefix('-').unwrap_or(name)))
}

/// The program in a sampled foreground process name: `None` while a shell is
/// waiting at its prompt.
fn foreground_program(process: &str) -> Option<&str> {
    let process = process.trim();
    let name = process.strip_prefix('-').unwrap_or(process);
    let unwrapped = unwrapped_name(name);
    (!unwrapped.is_empty() && !is_shell(process)).then_some(if unwrapped == name {
        process
    } else {
        unwrapped
    })
}

/// What a sampled foreground process comes to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Foreground<'process> {
    /// A shell reading from the terminal, as far as can be told. A builtin such as
    /// `read`, or a subshell, runs in a process just like it.
    Prompt,
    Program(&'process str),
    /// Nothing could be read, which is not the same as a prompt.
    Unknown,
}

impl<'process> From<&'process ForegroundProcess> for Foreground<'process> {
    fn from(process: &'process ForegroundProcess) -> Self {
        let name = process.name.trim();
        if !is_shell(name) {
            return foreground_program(name).map_or(Self::Unknown, Self::Program);
        }
        match process
            .arguments
            .as_deref()
            .map(super::foreground::shell_run)
        {
            None => Self::Unknown,
            Some(ShellRun::Interactive) => Self::Prompt,
            Some(ShellRun::Script(script)) => Self::Program(path_basename(script)),
            Some(ShellRun::Commands) => {
                Self::Program(unwrapped_name(name.strip_prefix('-').unwrap_or(name)))
            }
        }
    }
}

/// What a pane's terminal is doing, as far as naming its tab goes.
///
/// A title only speaks for the program that set it, so it goes when the foreground
/// changes. Otherwise it would go on naming the tab long after that program exited.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PaneActivity {
    title: Option<String>,
    program: Option<String>,
    /// The last sample could not name the foreground, so `program` is only what was
    /// last known, and a prompt cannot be assumed from it being empty.
    foreground_unknown: bool,
    task: Option<RunningTask>,
    /// What a task pasted here but not run is waiting for.
    staged_hint: Option<String>,
    /// The task whose command is at the prompt waiting for its arguments.
    staged_task: Option<String>,
}

impl PaneActivity {
    pub(super) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The foreground program as of the last sample; `None` at a shell prompt.
    pub(super) fn program(&self) -> Option<&str> {
        self.program.as_deref()
    }

    pub(super) fn task_name(&self) -> Option<&str> {
        self.task.as_ref().map(RunningTask::name)
    }

    pub(super) fn staged_hint(&self) -> Option<&str> {
        self.staged_hint.as_deref()
    }

    /// Replaces the hint about a pasted task, which goes when the line is acted on
    /// or the foreground changes hands.
    pub(super) fn set_staged_hint(&mut self, hint: Option<String>) {
        self.staged_hint = hint;
    }

    /// Records the task an Enter would run once its arguments are typed, which goes on
    /// the same terms as the hint about it.
    pub(super) fn set_staged_task(&mut self, task: Option<String>) {
        self.staged_task = task;
    }

    pub(super) fn has_staged_task(&self) -> bool {
        self.staged_task.is_some()
    }

    pub(super) fn take_staged_task(&mut self) -> Option<String> {
        self.staged_task.take()
    }

    /// Whether the last sample found an interactive shell, as opposed to a program, a
    /// shell running a script, or nothing it could read. A builtin such as `read`, or a
    /// subshell, cannot be told from a prompt without help from the shell.
    pub(super) fn at_prompt(&self) -> bool {
        self.program.is_none() && !self.foreground_unknown
    }

    /// Records the foreground process, which also settles whether a title still
    /// applies and whether a task launched here is still going.
    ///
    /// A missing or blank name is not a prompt. A pipeline's group outlives its
    /// leader, whose name is all the platform offers, so `git log | less` reads as
    /// nothing while `less` runs. What was known stands until a name comes back.
    pub(super) fn sample(&mut self, process: Option<&ForegroundProcess>, now: Instant) {
        let foreground = process.map_or(Foreground::Unknown, Foreground::from);
        self.foreground_unknown = foreground == Foreground::Unknown;
        let program = match foreground {
            Foreground::Unknown => self.program.clone(),
            Foreground::Prompt => None,
            Foreground::Program(program) => Some(program.to_owned()),
        };
        if program != self.program {
            self.title = None;
            self.staged_hint = None;
            self.staged_task = None;
            self.program = program;
        }
        self.task = self
            .task
            .take()
            .and_then(|task| task.observe(self.program.as_deref(), now));
    }

    /// Records a title for the program in the foreground, so sample first: a
    /// program that sets its title as it starts is usually not sampled yet.
    pub(super) fn set_title(&mut self, title: String) {
        self.title = Some(title).filter(|title| !title.trim().is_empty());
    }

    /// Records a task pasted at this pane's prompt. A paste into a busy pane goes
    /// to that program instead of starting anything.
    pub(super) fn launch_task(&mut self, name: impl Into<String>, now: Instant) {
        if self.at_prompt() {
            self.task = Some(RunningTask::launched(name, now));
        }
    }
}

/// A Cuenv task launched from Cuetty, which names its tab until it finishes.
///
/// The task is pasted into the shell, so Cuetty only ever sees it from outside:
/// a program takes over the foreground, and later hands it back to the shell.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RunningTask {
    name: String,
    launched_at: Instant,
    program_seen: bool,
}

impl RunningTask {
    fn launched(name: impl Into<String>, launched_at: Instant) -> Self {
        Self {
            name: name.into(),
            launched_at,
            program_seen: false,
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    /// The task after a foreground sample, or `None` once it has finished. Any other
    /// program means it finished between samples and the user has moved on.
    fn observe(self, foreground_program: Option<&str>, now: Instant) -> Option<Self> {
        match foreground_program {
            Some(TASK_PROGRAM) => Some(Self {
                program_seen: true,
                ..self
            }),
            Some(_) => None,
            None if self.program_seen => None,
            None => (now.duration_since(self.launched_at) < TASK_START_GRACE).then_some(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl PaneActivity {
        /// A sample of a process with `name` and no arguments, which for a shell is
        /// an interactive one.
        fn sample_name(&mut self, name: Option<&str>, now: Instant) {
            let process = name.map(|name| ForegroundProcess {
                name: name.to_owned(),
                arguments: Some(Vec::new()),
            });
            self.sample(process.as_ref(), now);
        }

        fn sample_shell(&mut self, name: &str, arguments: Option<&[&str]>) {
            let process = ForegroundProcess {
                name: name.to_owned(),
                arguments: arguments
                    .map(|arguments| arguments.iter().map(ToString::to_string).collect()),
            };
            self.sample(Some(&process), Instant::now());
        }
    }

    #[test]
    fn a_subtitle_says_where_a_tab_is_and_which_branch_that_is_on() {
        let directory = Path::new("/home/me/Code/cuenv");
        assert_eq!(
            tab_subtitle(Some(directory), Some("main"), "claude"),
            "cuenv · main"
        );
        assert_eq!(tab_subtitle(Some(directory), None, "claude"), "cuenv");
        assert_eq!(tab_subtitle(None, Some("main"), "claude"), "main");
    }

    #[test]
    fn a_subtitle_does_not_repeat_a_label_named_after_the_directory() {
        let directory = Path::new("/home/me/Code/cuenv");
        assert_eq!(tab_subtitle(Some(directory), Some("main"), "cuenv"), "main");
        assert_eq!(tab_subtitle(Some(directory), None, "cuenv"), "");
    }

    #[test]
    fn a_subtitle_is_empty_rather_than_a_placeholder_when_nothing_is_known() {
        assert_eq!(tab_subtitle(None, None, "Terminal"), "");
        assert_eq!(tab_subtitle(None, Some("  "), "Terminal"), "");
    }

    #[test]
    fn each_source_outranks_every_source_after_it() {
        let mut sources = TabLabelSources {
            custom_name: Some("deploys"),
            running_task: Some("checks.clippy"),
            foreground_program: Some("cargo"),
            title: Some("deploy logs"),
            directory: Some(Path::new("/home/me/directory")),
        };
        assert_eq!(resolve_tab_label(sources), "deploys");
        sources.custom_name = None;
        assert_eq!(resolve_tab_label(sources), "checks.clippy");
        sources.running_task = None;
        assert_eq!(resolve_tab_label(sources), "deploy logs");
        sources.title = None;
        assert_eq!(resolve_tab_label(sources), "cargo");
        sources.foreground_program = None;
        sources.title = Some("~/Code/title");
        assert_eq!(resolve_tab_label(sources), "directory");
        sources.directory = None;
        assert_eq!(resolve_tab_label(sources), "title");
        sources.title = None;
        assert_eq!(resolve_tab_label(sources), "Terminal");
    }

    #[test]
    fn the_working_directory_outranks_the_one_a_title_names() {
        // Nu's own titles, as it sets them at the prompt and while a command runs.
        for title in ["~/Code/cuenv> sleep", "~/My Pro", "~/My Pro> sleep"] {
            let sources = TabLabelSources {
                title: Some(title),
                directory: Some(Path::new("/home/me/My Project")),
                ..TabLabelSources::default()
            };
            assert_eq!(resolve_tab_label(sources), "My Project");
        }
    }

    #[test]
    fn a_title_a_program_chose_outranks_its_process_name() {
        let sources = TabLabelSources {
            foreground_program: Some("node"),
            title: Some("Claude Code"),
            ..TabLabelSources::default()
        };
        assert_eq!(resolve_tab_label(sources), "Claude Code");
    }

    #[test]
    fn a_blank_source_gives_way_to_the_next_one() {
        let sources = TabLabelSources {
            custom_name: Some("  "),
            running_task: Some(""),
            title: Some("   "),
            directory: Some(Path::new("/work/cuenv")),
            ..TabLabelSources::default()
        };
        assert_eq!(resolve_tab_label(sources), "cuenv");
    }

    #[test]
    fn titles_reduce_to_the_directory_they_name() {
        let label = |title| {
            resolve_tab_label(TabLabelSources {
                title: Some(title),
                ..TabLabelSources::default()
            })
        };
        assert_eq!(label("~/Code/src/github.com/cuenv/cuenv"), "cuenv");
        assert_eq!(label("~/Code/cuenv/"), "cuenv");
        assert_eq!(label("/"), "/");
        assert_eq!(label("~"), "~");
        assert_eq!(label("me@host:~/Code/cuenv"), "cuenv");
        assert_eq!(label("me@host: /srv/www"), "www");
        assert_eq!(label("~/Code/cuenv > cargo test -p /tmp/x"), "cuenv");
        assert_eq!(label("~/Code/cuenv> sleep"), "cuenv");
        assert_eq!(label("~/Code/cuenv>"), "cuenv");
        assert_eq!(label("~/My Project"), "My Project");
        assert_eq!(label("~/My Project> sleep"), "My Project");
        assert_eq!(label("/srv/a:b"), "a:b");
    }

    #[test]
    fn a_program_title_keeps_its_slashes() {
        let sources = TabLabelSources {
            title: Some("vim src/main.rs"),
            ..TabLabelSources::default()
        };
        assert_eq!(resolve_tab_label(sources), "vim src/main.rs");
    }

    #[test]
    fn a_path_further_along_a_title_is_not_where_the_program_is() {
        for title in ["vim /etc/hosts", "editing: /etc/hosts"] {
            let sources = TabLabelSources {
                title: Some(title),
                directory: Some(Path::new("/work/cuenv")),
                ..TabLabelSources::default()
            };
            assert_eq!(resolve_tab_label(sources), title);
        }
    }

    #[test]
    fn the_filesystem_root_is_still_a_name() {
        let sources = TabLabelSources {
            directory: Some(Path::new("/")),
            ..TabLabelSources::default()
        };
        assert_eq!(resolve_tab_label(sources), "/");
    }

    #[test]
    fn initials_skip_punctuation_and_stay_one_character() {
        assert_eq!(tab_initial("cuenv"), "C");
        assert_eq!(tab_initial(".config"), "C");
        assert_eq!(tab_initial("~"), "~");
        assert_eq!(tab_initial("/"), "/");
        assert_eq!(tab_initial("ßeta"), "S");
        assert_eq!(tab_initial(""), "");
    }

    #[test]
    fn a_mark_prefers_the_custom_name_then_the_project() {
        let mut sources = TabMarkSources {
            custom_name: Some("deploys"),
            banner: Some("academy"),
            directory: Some(Path::new("/work/website")),
            label: "cargo",
        };
        assert_eq!(tab_mark(sources), "D");
        sources.custom_name = Some("  ");
        assert_eq!(tab_mark(sources), "A");
        sources.banner = None;
        assert_eq!(tab_mark(sources), "W");
        sources.directory = None;
        assert_eq!(tab_mark(sources), "C");
    }

    #[test]
    fn a_mark_does_not_follow_what_is_running() {
        let mark = |label| {
            tab_mark(TabMarkSources {
                directory: Some(Path::new("/work/cuenv")),
                label,
                ..TabMarkSources::default()
            })
        };
        assert_eq!(mark("cargo"), "C");
        assert_eq!(mark("nvim"), "C");
    }

    #[test]
    fn a_tooltip_adds_the_banner_only_when_it_differs() {
        assert_eq!(tab_tooltip("cargo", None), "cargo");
        assert_eq!(tab_tooltip("cuenv", Some("cuenv")), "cuenv");
        assert_eq!(tab_tooltip("cargo", Some("cuenv")), "cargo · cuenv");
    }

    #[test]
    fn shells_are_not_foreground_programs() {
        assert_eq!(foreground_program("zsh"), None);
        assert_eq!(foreground_program("-zsh"), None);
        assert_eq!(foreground_program("nu"), None);
        assert_eq!(foreground_program(""), None);
        assert_eq!(foreground_program("cargo"), Some("cargo"));
        assert_eq!(foreground_program("-custom"), Some("-custom"));
    }

    #[test]
    fn nix_wrappers_are_named_after_what_they_wrap() {
        assert_eq!(foreground_program(".claude-wrapped"), Some("claude"));
        assert_eq!(foreground_program(".bat-wrapped"), Some("bat"));
        assert_eq!(foreground_program("neovim-unwrapped"), Some("neovim"));
        assert_eq!(foreground_program(".hidden-tool"), Some("hidden-tool"));
        // Their shells are still shells.
        assert_eq!(foreground_program(".fish-wrapped"), None);
        assert_eq!(foreground_program("-.zsh-wrapped"), None);
    }

    #[test]
    fn a_wrapper_suffix_cut_off_by_the_kernel_is_still_removed() {
        assert_eq!(foreground_program(".kubectl-wrappe"), Some("kubectl"));
        assert_eq!(foreground_program(".terraform-wrap"), Some("terraform"));
        assert_eq!(
            foreground_program(".some-long-tool"),
            Some("some-long-tool")
        );
        // Short of the limit, the name is whole, and these are not suffixes.
        assert_eq!(foreground_program(".tool-wrap"), Some("tool-wrap"));
        assert_eq!(foreground_program("."), Some("."));
    }

    #[test]
    fn a_title_naming_a_shell_does_not_name_the_tab() {
        for title in ["bash", "-zsh", ".fish-wrapped"] {
            let sources = TabLabelSources {
                foreground_program: Some("cargo"),
                title: Some(title),
                ..TabLabelSources::default()
            };
            assert_eq!(resolve_tab_label(sources), "cargo");
        }
    }

    fn label_of(activity: &PaneActivity) -> String {
        resolve_tab_label(TabLabelSources {
            running_task: activity.task_name(),
            foreground_program: activity.program(),
            title: activity.title(),
            directory: Some(Path::new("/work/other")),
            ..TabLabelSources::default()
        })
    }

    #[test]
    fn a_title_goes_with_the_program_that_set_it() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.sample_name(Some("deploy"), now);
        activity.set_title("deploy logs".into());
        assert_eq!(label_of(&activity), "deploy logs");
        activity.sample_name(Some("deploy"), now);
        assert_eq!(label_of(&activity), "deploy logs");

        activity.sample_name(Some("cargo"), now);
        assert_eq!(label_of(&activity), "cargo");
        activity.sample_name(Some("zsh"), now);
        assert_eq!(label_of(&activity), "other");
    }

    #[test]
    fn a_title_set_at_the_prompt_lasts_until_a_program_runs() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.sample_name(Some("zsh"), now);
        activity.set_title("deploy logs".into());
        activity.sample_name(Some("zsh"), now);
        assert_eq!(label_of(&activity), "deploy logs");

        activity.sample_name(Some("cargo"), now);
        assert_eq!(label_of(&activity), "cargo");
        activity.sample_name(Some("zsh"), now);
        assert_eq!(label_of(&activity), "other");
    }

    #[test]
    fn a_staged_hint_goes_when_the_foreground_changes_hands() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.sample_name(Some("vim"), now);
        activity.set_staged_hint(Some("press Enter".into()));
        activity.sample_name(Some("vim"), now);
        activity.sample_name(Some(""), now);
        assert_eq!(activity.staged_hint(), Some("press Enter"));
        activity.sample_name(Some("zsh"), now);
        assert_eq!(activity.staged_hint(), None);
    }

    #[test]
    fn a_blank_title_clears_the_current_one() {
        let mut activity = PaneActivity::default();
        activity.set_title("vim".into());
        activity.set_title("  ".into());
        assert_eq!(activity.title(), None);
    }

    #[test]
    fn a_busy_pane_never_gains_a_task() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.sample_name(Some("vim"), now);
        activity.launch_task("build", now);
        assert_eq!(activity.task_name(), None);
        activity.sample_name(Some("zsh"), now);
        activity.launch_task("build", now);
        assert_eq!(activity.task_name(), Some("build"));
    }

    #[test]
    fn an_unreadable_foreground_keeps_the_program_and_its_title() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.sample_name(Some("less"), now);
        activity.set_title("README.md".into());
        for unreadable in [Some(""), Some("  "), None] {
            activity.sample_name(unreadable, now);
            assert_eq!(activity.program(), Some("less"));
            assert_eq!(label_of(&activity), "README.md");
        }
        // The same program coming back into view is no change of program.
        activity.sample_name(Some("less"), now);
        assert_eq!(label_of(&activity), "README.md");
        activity.sample_name(Some("zsh"), now);
        assert_eq!(label_of(&activity), "other");
    }

    #[test]
    fn an_unreadable_foreground_is_not_a_prompt() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        // A pipeline whose leader was gone before it was ever sampled.
        activity.sample_name(Some("zsh"), now);
        assert!(activity.at_prompt());
        activity.sample_name(Some(""), now);
        assert!(!activity.at_prompt());
        activity.launch_task("build", now);
        assert_eq!(activity.task_name(), None);

        activity.sample_name(Some("zsh"), now);
        activity.launch_task("build", now);
        assert_eq!(activity.task_name(), Some("build"));
    }

    #[test]
    fn a_shell_running_a_script_is_a_program_named_after_the_script() {
        let mut activity = PaneActivity::default();
        activity.sample_shell("bash", Some(&["-e", "./ops/deploy.sh", "--force"]));
        assert!(!activity.at_prompt());
        assert_eq!(activity.program(), Some("deploy.sh"));
        activity.launch_task("build", Instant::now());
        assert_eq!(activity.task_name(), None);
    }

    #[test]
    fn a_shell_given_commands_is_a_program_named_after_the_shell() {
        let mut activity = PaneActivity::default();
        activity.sample_shell(".bash-wrapped", Some(&["-c", "make"]));
        assert!(!activity.at_prompt());
        assert_eq!(activity.program(), Some("bash"));
    }

    #[test]
    fn an_interactive_shell_with_options_is_at_its_prompt() {
        let mut activity = PaneActivity::default();
        activity.sample_shell("bash", Some(&["--rcfile", "/tmp/nix-shell.abc"]));
        assert!(activity.at_prompt());
        activity.sample_shell("-zsh", Some(&[]));
        assert!(activity.at_prompt());
    }

    #[test]
    fn a_shell_whose_arguments_cannot_be_read_is_not_a_prompt() {
        let mut activity = PaneActivity::default();
        activity.sample_shell("deploy", Some(&[]));
        activity.sample_shell("bash", None);
        assert!(!activity.at_prompt());
        // What was known stands, as with any foreground that cannot be read.
        assert_eq!(activity.program(), Some("deploy"));
    }

    /// The limit of telling from outside the shell: `read -p` typed at a prompt, or a
    /// subshell, runs under the very arguments the prompt does.
    #[test]
    fn a_builtin_waiting_for_input_cannot_be_told_from_a_prompt() {
        let mut activity = PaneActivity::default();
        activity.sample_shell("bash", Some(&[]));
        assert!(activity.at_prompt());
    }

    #[test]
    fn an_unreadable_foreground_neither_ends_a_running_task_nor_extends_a_grace_period() {
        let start = Instant::now();
        let mut activity = PaneActivity::default();
        activity.launch_task("build", start);
        activity.sample_name(Some("cuenv"), start);
        let later = start + TASK_START_GRACE * 10;
        activity.sample_name(Some(""), later);
        assert_eq!(activity.task_name(), Some("build"));

        let mut unseen = PaneActivity::default();
        unseen.launch_task("build", start);
        unseen.sample_name(Some(""), start);
        assert_eq!(unseen.task_name(), Some("build"));
        unseen.sample_name(Some(""), later);
        assert_eq!(unseen.task_name(), None);
    }

    #[test]
    fn an_unrelated_program_ends_a_task_that_finished_between_samples() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.launch_task("build", now);
        activity.sample_name(Some("vim"), now + TASK_START_GRACE / 2);
        assert_eq!(activity.task_name(), None);
        assert_eq!(label_of(&activity), "vim");
    }

    #[test]
    fn a_program_following_the_task_ends_it_too() {
        let now = Instant::now();
        let mut activity = PaneActivity::default();
        activity.launch_task("build", now);
        activity.sample_name(Some(".cuenv-wrapped"), now);
        assert_eq!(activity.task_name(), Some("build"));
        activity.sample_name(Some("vim"), now);
        assert_eq!(activity.task_name(), None);
    }

    #[test]
    fn a_task_runs_until_its_program_hands_back_the_foreground() {
        let start = Instant::now();
        let task = RunningTask::launched("build", start);
        // The shell still holds the foreground just after the paste.
        let task = task.observe(None, start).unwrap();
        let task = task.observe(Some("cuenv"), start).unwrap();
        let later = start + TASK_START_GRACE * 10;
        let task = task.observe(Some("cuenv"), later).unwrap();
        assert_eq!(task.name(), "build");
        assert_eq!(task.observe(None, later), None);
    }

    #[test]
    fn a_task_too_quick_to_sample_ends_after_the_grace_period() {
        let start = Instant::now();
        let task = RunningTask::launched("build", start);
        let task = task.observe(None, start + TASK_START_GRACE / 2).unwrap();
        assert_eq!(task.observe(None, start + TASK_START_GRACE), None);
    }
}
