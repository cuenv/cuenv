//! Cuenv-backed, presentation-only configuration for Cuetty.
//!
//! CUE is evaluated on a worker thread.  The terminal UI receives only this
//! validated representation, so configuration cannot reach a renderer or a
//! session directly.

use cuengine::{ModuleEvalOptions, UnifySource};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const PACKAGE: &str = "cuetty";
pub const PROJECT_PACKAGE: &str = "cuenv";
/// Directory below the XDG config home that holds the global `cuetty` package.
const GLOBAL_CONFIG_DIR: &str = "cuetty";
const MAX_BANNER_BYTES: usize = 120;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CuettyPresentation {
    pub banner: Option<String>,
    pub rail: Option<RailColor>,
    pub titlebar: TitlebarVisibility,
    pub keybindings: Keybindings,
}

/// Application chords configured through `keybindings` in package `cuetty`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Keybindings {
    pub palette: KeyChord,
}

impl Default for Keybindings {
    fn default() -> Self {
        let macos = cfg!(target_os = "macos");
        Self {
            // Cmd-K on macOS and Ctrl-K elsewhere, matching common
            // command-palette conventions.
            palette: KeyChord {
                key: "k".into(),
                modifiers: ChordModifiers {
                    control: !macos,
                    platform: macos,
                    ..ChordModifiers::default()
                },
            },
        }
    }
}

/// Modifiers held for a [`KeyChord`]; a chord matches only this exact set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChordModifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
}

/// A renderer-neutral chord written as `ctrl-k` or `ctrl-shift-p`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyChord {
    /// Lowercase key name, compared case-insensitively with the pressed key.
    pub key: String,
    pub modifiers: ChordModifiers,
}

impl KeyChord {
    pub fn parse(value: &str) -> Result<Self, String> {
        let invalid = || format!("invalid key chord '{value}'; expected e.g. \"ctrl-k\"");
        let mut segments: Vec<&str> = value.split('-').collect();
        let key = segments
            .pop()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(invalid)?;
        let mut modifiers = ChordModifiers::default();
        for segment in segments {
            let flag = match segment.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut modifiers.control,
                "alt" | "option" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                "cmd" | "super" | "win" => &mut modifiers.platform,
                _ => return Err(invalid()),
            };
            if *flag {
                return Err(invalid());
            }
            *flag = true;
        }
        Ok(Self {
            key: key.to_ascii_lowercase(),
            modifiers,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RailColor(pub u8, pub u8, pub u8);

/// Whether Cuetty draws its title strip above the terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TitlebarVisibility {
    #[default]
    Shown,
    Hidden,
}

impl From<bool> for TitlebarVisibility {
    fn from(shown: bool) -> Self {
        if shown { Self::Shown } else { Self::Hidden }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CuenvTaskKind {
    Task,
    Group,
    Sequence,
}

impl CuenvTaskKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Task => "TASK",
            Self::Group => "GROUP",
            Self::Sequence => "SEQUENCE",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CuenvTask {
    pub name: String,
    pub description: Option<String>,
    pub kind: CuenvTaskKind,
    pub requires_parameters: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskEvaluation {
    Absent,
    Valid(Vec<CuenvTask>),
    Invalid(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CuettyDto {
    #[serde(default)]
    banner: Option<String>,
    #[serde(default)]
    rail: Option<String>,
    #[serde(default)]
    titlebar: Option<bool>,
    #[serde(default)]
    keybindings: Option<KeybindingsDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeybindingsDto {
    #[serde(default)]
    palette: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Evaluation {
    Absent,
    Valid(CuettyPresentation),
    Invalid(String),
}

/// Per-pane presentation state kept independent from the GPUI view. The
/// generation is advanced whenever the shell enters a new directory or a new
/// source binding, so late worker results cannot cross a CWD boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CuenvPaneState {
    pub cwd: Option<PathBuf>,
    pub generation: u64,
    pub presentation: CuettyPresentation,
    pub tasks: Vec<CuenvTask>,
    pub notice: Option<String>,
    pub task_notice: Option<String>,
}

impl CuenvPaneState {
    pub fn observe_cwd(&mut self, cwd: PathBuf) -> bool {
        if self.cwd.as_ref() == Some(&cwd) {
            return false;
        }
        self.cwd = Some(cwd);
        self.generation = self.generation.wrapping_add(1);
        self.presentation = CuettyPresentation::default();
        self.tasks.clear();
        self.notice = None;
        self.task_notice = None;
        true
    }

    pub fn begin_source(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub fn apply(&mut self, generation: u64, update: Evaluation) -> bool {
        if self.generation != generation {
            return false;
        }
        match update {
            Evaluation::Valid(presentation) => {
                self.presentation = presentation;
                self.notice = None;
            }
            Evaluation::Absent => {
                self.presentation = CuettyPresentation::default();
                self.notice = None;
            }
            Evaluation::Invalid(error) => self.notice = Some(error),
        }
        true
    }

    pub fn apply_tasks(&mut self, generation: u64, update: TaskEvaluation) -> bool {
        if self.generation != generation {
            return false;
        }
        match update {
            TaskEvaluation::Valid(tasks) => {
                self.tasks = tasks;
                self.task_notice = None;
            }
            TaskEvaluation::Absent => {
                self.tasks.clear();
                self.task_notice = None;
            }
            TaskEvaluation::Invalid(error) => self.task_notice = Some(error),
        }
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigSource {
    pub module_root: Option<PathBuf>,
    pub target_dir: PathBuf,
    /// Directory whose `package cuetty` is unified beneath every
    /// directory's configuration.
    pub global_dir: Option<PathBuf>,
}

impl ConfigSource {
    /// Select configuration from the shell's current working directory,
    /// unified with the global package under the XDG config home.
    pub fn from_cwd(start: &Path) -> Result<Self, String> {
        Self::in_dir(start).map(|source| source.with_global_dir(global_config_dir()))
    }

    /// Select configuration from exactly one directory, without a global
    /// layer. Any `.cue` file in that directory may provide package `cuetty`.
    pub fn in_dir(start: &Path) -> Result<Self, String> {
        let start = start
            .canonicalize()
            .map_err(|error| format!("cannot resolve terminal working directory: {error}"))?;
        let start = if start.is_file() {
            start
                .parent()
                .ok_or("config path has no parent")?
                .to_path_buf()
        } else {
            start
        };
        Ok(Self {
            module_root: nearest_module_root(&start),
            target_dir: start,
            global_dir: None,
        })
    }

    #[must_use]
    pub fn with_global_dir(mut self, global_dir: Option<PathBuf>) -> Self {
        self.global_dir = global_dir;
        self
    }

    /// The global directory when it currently exists. It is resolved on
    /// every evaluation so creating it later takes effect on the next reload.
    fn existing_global_dir(&self) -> Option<PathBuf> {
        self.global_dir
            .as_deref()
            .filter(|dir| dir.is_dir())
            .and_then(|dir| dir.canonicalize().ok())
    }

    /// Evaluates the directory's `package cuetty` unified with the global
    /// one. CUE unification applies: conflicting concrete values fail, so a
    /// global value meant to be overridden is written as a default
    /// (`rail: *"#89b4fa" | string`). Without a directory package the
    /// global package applies alone.
    pub fn evaluate(&self) -> Evaluation {
        let global = self.existing_global_dir();
        let unify = global
            .iter()
            .map(|dir| UnifySource {
                dir: dir.to_string_lossy().into_owned(),
                module_root: nearest_module_root(dir)
                    .map(|root| root.to_string_lossy().into_owned()),
            })
            .collect();
        let local = evaluate_cuetty_instance(&self.target_dir, self.module_root.as_deref(), unify);
        let value = match (local, &global) {
            (Ok(None), Some(global)) => {
                evaluate_cuetty_instance(global, nearest_module_root(global).as_deref(), Vec::new())
            }
            (local, _) => local,
        };
        match value {
            Ok(Some(value)) => parse_value(value),
            // No `package cuetty` in this directory or the global one.
            Ok(None) => Evaluation::Absent,
            Err(error) => Evaluation::Invalid(error),
        }
    }

    pub fn evaluate_tasks(&self) -> TaskEvaluation {
        let module_root = self.module_root.as_deref().unwrap_or(&self.target_dir);
        let options = ModuleEvalOptions {
            recursive: false,
            package_name: Some(PROJECT_PACKAGE.into()),
            target_dir: Some(self.target_dir.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let module = match cuengine::evaluate_module(module_root, PROJECT_PACKAGE, Some(&options)) {
            Ok(module) => module,
            Err(error) => {
                return TaskEvaluation::Invalid(format!("Cuetty task evaluation failed: {error}"));
            }
        };
        let Some(value) = module
            .instances
            .get(".")
            .or_else(|| module.instances.values().next())
        else {
            return TaskEvaluation::Absent;
        };
        parse_tasks(value)
    }
}

/// Evaluates `package cuetty` in exactly `target_dir`, unified with `unify`.
/// `Ok(None)` means the directory holds no such package.
fn evaluate_cuetty_instance(
    target_dir: &Path,
    module_root: Option<&Path>,
    unify: Vec<UnifySource>,
) -> Result<Option<serde_json::Value>, String> {
    let options = ModuleEvalOptions {
        recursive: false,
        package_name: Some(PACKAGE.into()),
        target_dir: Some(target_dir.to_string_lossy().into_owned()),
        unify,
        ..Default::default()
    };
    let module =
        cuengine::evaluate_module(module_root.unwrap_or(target_dir), PACKAGE, Some(&options))
            .map_err(|error| format!("Cuetty CUE evaluation failed: {error}"))?;
    Ok(module
        .instances
        .get(".")
        .or_else(|| module.instances.values().next())
        .cloned())
}

/// `$XDG_CONFIG_HOME/cuetty`, falling back to `~/.config/cuetty`.
pub fn global_config_dir() -> Option<PathBuf> {
    global_config_dir_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn global_config_dir_from(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    // The XDG spec requires an absolute path; relative values are ignored.
    xdg_config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })
        .map(|config| config.join(GLOBAL_CONFIG_DIR))
}

fn nearest_module_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(path) = current {
        if path.join("cue.mod").join("module.cue").is_file() {
            return path.canonicalize().ok();
        }
        current = path.parent();
    }
    None
}

fn parse_value(value: serde_json::Value) -> Evaluation {
    let dto = match serde_json::from_value::<CuettyDto>(value) {
        Ok(dto) => dto,
        Err(error) => return Evaluation::Invalid(format!("invalid Cuetty CUE schema: {error}")),
    };
    let banner = match dto.banner {
        Some(banner)
            if banner.is_empty()
                || banner.len() > MAX_BANNER_BYTES
                || banner.contains(['\n', '\r']) =>
        {
            return Evaluation::Invalid(
                "banner must be a single line no longer than 120 bytes".into(),
            );
        }
        value => value,
    };
    let rail = match dto.rail {
        Some(value) => match parse_color(&value) {
            Ok(color) => Some(color),
            Err(error) => return Evaluation::Invalid(error),
        },
        None => None,
    };
    let keybindings = match dto.keybindings.and_then(|keys| keys.palette) {
        Some(value) => match KeyChord::parse(&value) {
            Ok(palette) => Keybindings { palette },
            Err(error) => return Evaluation::Invalid(error),
        },
        None => Keybindings::default(),
    };
    Evaluation::Valid(CuettyPresentation {
        banner,
        rail,
        titlebar: dto
            .titlebar
            .map(TitlebarVisibility::from)
            .unwrap_or_default(),
        keybindings,
    })
}

fn parse_color(value: &str) -> Result<RailColor, String> {
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return Err("rail must use quoted #RRGGBB syntax".into());
    }
    let channel = |from| {
        u8::from_str_radix(&value[from..from + 2], 16)
            .map_err(|_| "rail must use quoted #RRGGBB syntax".to_string())
    };
    Ok(RailColor(channel(1)?, channel(3)?, channel(5)?))
}

fn parse_tasks(value: &serde_json::Value) -> TaskEvaluation {
    let Some(tasks) = value.get("tasks") else {
        return TaskEvaluation::Valid(Vec::new());
    };
    let Some(tasks) = tasks.as_object() else {
        return TaskEvaluation::Invalid("Cuenv tasks must evaluate to an object".into());
    };
    let mut catalog = Vec::new();
    for (name, node) in tasks {
        if let Err(error) = collect_task(name, node, &mut catalog) {
            return TaskEvaluation::Invalid(error);
        }
    }
    catalog.sort_by(|left, right| left.name.cmp(&right.name));
    TaskEvaluation::Valid(catalog)
}

fn collect_task(
    name: &str,
    node: &serde_json::Value,
    catalog: &mut Vec<CuenvTask>,
) -> Result<(), String> {
    if node.is_array() {
        catalog.push(CuenvTask {
            name: name.into(),
            description: None,
            kind: CuenvTaskKind::Sequence,
            requires_parameters: false,
        });
        return Ok(());
    }
    let Some(object) = node.as_object() else {
        return Err(format!(
            "Cuenv task '{name}' must evaluate to an object or sequence"
        ));
    };
    let description = object
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(String::from);
    if object.get("type").and_then(serde_json::Value::as_str) == Some("group") {
        catalog.push(CuenvTask {
            name: name.into(),
            description,
            kind: CuenvTaskKind::Group,
            requires_parameters: false,
        });
        for (child_name, child) in object {
            if matches!(
                child_name.as_str(),
                "type" | "dependsOn" | "maxConcurrency" | "description"
            ) || child_name.starts_with('_')
            {
                continue;
            }
            collect_task(&format!("{name}.{child_name}"), child, catalog)?;
        }
        return Ok(());
    }
    if !object.contains_key("command") && !object.contains_key("script") {
        return Err(format!(
            "Cuenv task '{name}' has no command, script, group, or sequence"
        ));
    }
    catalog.push(CuenvTask {
        name: name.into(),
        description,
        kind: CuenvTaskKind::Task,
        requires_parameters: has_required_parameters(object.get("params")),
    });
    Ok(())
}

fn has_required_parameters(params: Option<&serde_json::Value>) -> bool {
    let Some(params) = params.and_then(serde_json::Value::as_object) else {
        return false;
    };
    params.iter().any(|(name, value)| {
        if name == "positional" {
            value.as_array().is_some_and(|values| {
                values.iter().any(|value| {
                    value
                        .get("required")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                })
            })
        } else {
            value
                .get("required")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }
    })
}

pub fn relevant_change(event: &Event) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| {
        path.file_name().is_some_and(|name| name == "module.cue")
            || path.extension().is_some_and(|extension| extension == "cue")
    })
}

/// Native watcher retained by `TerminalView`; dropping it stops the watch.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    error: Arc<Mutex<Option<String>>>,
}

/// Replaceable watcher boundary for the terminal host. The UI only depends on
/// change notifications and health, never on `notify`'s concrete watcher.
pub trait CuenvWatcher: Send {
    fn take_error(&self) -> Option<String>;
}

/// Replaceable Cuenv integration boundary. Tests and future hosts can provide
/// an in-memory evaluator/watcher without constructing a native filesystem
/// watcher or invoking the CUE bridge.
pub trait CuenvProvider: Send + Sync {
    fn evaluate(&self, source: &ConfigSource) -> Evaluation;
    fn evaluate_tasks(&self, source: &ConfigSource) -> TaskEvaluation {
        let _ = source;
        TaskEvaluation::Absent
    }
    fn watch(
        &self,
        source: &ConfigSource,
        notify: async_channel::Sender<()>,
    ) -> Result<Box<dyn CuenvWatcher>, String>;
}

#[derive(Default)]
pub struct NativeCuenvProvider;

impl ConfigWatcher {
    pub fn start(source: &ConfigSource, notify: async_channel::Sender<()>) -> Result<Self, String> {
        let error = Arc::new(Mutex::new(None));
        let callback_error = Arc::clone(&error);
        let mut watcher =
            notify::recommended_watcher(move |event: Result<Event, notify::Error>| match event {
                Ok(event) if relevant_change(&event) => {
                    let _ = notify.try_send(());
                }
                Err(error) => {
                    if let Ok(mut slot) = callback_error.lock() {
                        *slot = Some(error.to_string());
                    }
                }
                _ => {}
            })
            .map_err(|error| format!("cannot watch Cuetty CUE configuration: {error}"))?;
        // Watch the exact CWD (for atomic saves, creates, and renames) and
        // module metadata separately; configuration never leaks from parents.
        watcher
            .watch(&source.target_dir, RecursiveMode::NonRecursive)
            .map_err(|error| format!("cannot watch Cuetty CUE module: {error}"))?;
        if let Some(module_root) = &source.module_root {
            let module_metadata = module_root.join("cue.mod");
            if module_metadata != source.target_dir {
                watcher
                    .watch(&module_metadata, RecursiveMode::NonRecursive)
                    .map_err(|error| format!("cannot watch Cuetty module metadata: {error}"))?;
            }
        }
        if let Some(global_dir) = source
            .existing_global_dir()
            .filter(|dir| *dir != source.target_dir)
        {
            watcher
                .watch(&global_dir, RecursiveMode::NonRecursive)
                .map_err(|error| format!("cannot watch global Cuetty configuration: {error}"))?;
        }
        Ok(Self {
            _watcher: watcher,
            error,
        })
    }

    fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl CuenvWatcher for ConfigWatcher {
    fn take_error(&self) -> Option<String> {
        ConfigWatcher::take_error(self)
    }
}

impl CuenvProvider for NativeCuenvProvider {
    fn evaluate(&self, source: &ConfigSource) -> Evaluation {
        source.evaluate()
    }

    fn evaluate_tasks(&self, source: &ConfigSource) -> TaskEvaluation {
        source.evaluate_tasks()
    }

    fn watch(
        &self,
        source: &ConfigSource,
        notify: async_channel::Sender<()>,
    ) -> Result<Box<dyn CuenvWatcher>, String> {
        ConfigWatcher::start(source, notify)
            .map(|watcher| Box::new(watcher) as Box<dyn CuenvWatcher>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_v1_presentation() {
        assert_eq!(
            parse_value(serde_json::json!({"banner": "Welcome to Cuetty", "rail": "#ff0000"})),
            Evaluation::Valid(CuettyPresentation {
                banner: Some("Welcome to Cuetty".into()),
                rail: Some(RailColor(255, 0, 0)),
                titlebar: TitlebarVisibility::Shown,
                keybindings: Keybindings::default(),
            })
        );
    }

    #[test]
    fn titlebar_defaults_to_shown_and_can_be_hidden() {
        let presentation = |value| match parse_value(value) {
            Evaluation::Valid(presentation) => presentation.titlebar,
            other => panic!("unexpected evaluation: {other:?}"),
        };
        assert_eq!(
            presentation(serde_json::json!({})),
            TitlebarVisibility::Shown
        );
        assert_eq!(
            presentation(serde_json::json!({"titlebar": true})),
            TitlebarVisibility::Shown
        );
        assert_eq!(
            presentation(serde_json::json!({"titlebar": false})),
            TitlebarVisibility::Hidden
        );
        assert!(matches!(
            parse_value(serde_json::json!({"titlebar": "no"})),
            Evaluation::Invalid(_)
        ));
    }

    #[test]
    fn pane_state_clears_on_cwd_changes_and_rejects_stale_results() {
        let mut state = CuenvPaneState::default();
        assert!(state.observe_cwd(PathBuf::from("/a")));
        let generation_a = state.begin_source();
        assert!(state.apply(
            generation_a,
            Evaluation::Valid(CuettyPresentation {
                banner: Some("A".into()),
                rail: Some(RailColor(255, 0, 0)),
                titlebar: TitlebarVisibility::Shown,
                keybindings: Keybindings::default(),
            })
        ));
        assert!(state.observe_cwd(PathBuf::from("/b")));
        assert_eq!(state.presentation, CuettyPresentation::default());
        assert!(!state.apply(
            generation_a,
            Evaluation::Valid(CuettyPresentation {
                banner: Some("late A".into()),
                rail: None,
                titlebar: TitlebarVisibility::Shown,
                keybindings: Keybindings::default(),
            })
        ));
    }

    #[test]
    fn pane_state_keeps_last_good_presentation_for_same_source_failures() {
        let mut state = CuenvPaneState::default();
        state.observe_cwd(PathBuf::from("/a"));
        let generation = state.begin_source();
        let presentation = CuettyPresentation {
            banner: Some("A".into()),
            rail: None,
            titlebar: TitlebarVisibility::Shown,
            keybindings: Keybindings::default(),
        };
        assert!(state.apply(generation, Evaluation::Valid(presentation.clone())));
        assert!(state.apply(generation, Evaluation::Invalid("broken".into())));
        assert_eq!(state.presentation, presentation);
        assert_eq!(state.notice.as_deref(), Some("broken"));
    }

    #[test]
    fn pane_state_keeps_last_good_presentation_when_source_rebinds() {
        let mut state = CuenvPaneState::default();
        state.observe_cwd(PathBuf::from("/a"));
        let first_generation = state.begin_source();
        let presentation = CuettyPresentation {
            banner: Some("A".into()),
            rail: Some(RailColor(255, 0, 0)),
            titlebar: TitlebarVisibility::Shown,
            keybindings: Keybindings::default(),
        };
        assert!(state.apply(first_generation, Evaluation::Valid(presentation.clone())));

        let rebound_generation = state.begin_source();
        assert!(state.apply(
            rebound_generation,
            Evaluation::Invalid("new module is broken".into())
        ));
        assert_eq!(state.presentation, presentation);
        assert_eq!(state.notice.as_deref(), Some("new module is broken"));
    }

    #[test]
    fn task_catalog_flattens_groups_and_marks_required_arguments() {
        let evaluation = parse_tasks(&serde_json::json!({
            "tasks": {
                "build": {
                    "command": "cargo",
                    "description": "Build the workspace",
                    "params": {"profile": {"required": true}}
                },
                "checks": {
                    "type": "group",
                    "description": "Quality gates",
                    "lint": {"command": "cargo"}
                },
                "release": ["build", "checks.lint"]
            }
        }));
        let TaskEvaluation::Valid(tasks) = evaluation else {
            panic!("expected a valid task catalog");
        };
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["build", "checks", "checks.lint", "release"]
        );
        assert!(tasks[0].requires_parameters);
        assert_eq!(tasks[1].kind, CuenvTaskKind::Group);
        assert_eq!(tasks[2].kind, CuenvTaskKind::Task);
        assert_eq!(tasks[3].kind, CuenvTaskKind::Sequence);
    }

    #[test]
    fn pane_state_clears_tasks_on_cwd_change_and_rejects_stale_results() {
        let mut state = CuenvPaneState::default();
        state.observe_cwd(PathBuf::from("/a"));
        let generation = state.begin_source();
        let tasks = vec![CuenvTask {
            name: "build".into(),
            description: None,
            kind: CuenvTaskKind::Task,
            requires_parameters: false,
        }];
        assert!(state.apply_tasks(generation, TaskEvaluation::Valid(tasks)));
        assert_eq!(state.tasks.len(), 1);
        state.observe_cwd(PathBuf::from("/b"));
        assert!(state.tasks.is_empty());
        assert!(!state.apply_tasks(generation, TaskEvaluation::Valid(Vec::new())));
    }

    #[test]
    fn palette_chord_defaults_per_platform_and_is_configurable() {
        let palette = |value| match parse_value(value) {
            Evaluation::Valid(presentation) => presentation.keybindings.palette,
            other => panic!("unexpected evaluation: {other:?}"),
        };
        let default = palette(serde_json::json!({}));
        assert_eq!(default.key, "k");
        assert_eq!(default.modifiers.control, !cfg!(target_os = "macos"));
        assert_eq!(default.modifiers.platform, cfg!(target_os = "macos"));
        assert_eq!(
            palette(serde_json::json!({"keybindings": {"palette": "Ctrl-Shift-P"}})),
            KeyChord {
                key: "p".into(),
                modifiers: ChordModifiers {
                    control: true,
                    shift: true,
                    ..ChordModifiers::default()
                },
            }
        );
        for invalid in ["", "ctrl-", "hyper-k", "ctrl-ctrl-k"] {
            assert!(
                matches!(
                    parse_value(serde_json::json!({"keybindings": {"palette": invalid}})),
                    Evaluation::Invalid(_)
                ),
                "{invalid} should be rejected"
            );
        }
        assert!(matches!(
            parse_value(serde_json::json!({"keybindings": {"copy": "ctrl-c"}})),
            Evaluation::Invalid(_)
        ));
    }

    #[test]
    fn rejects_unknown_fields_and_non_hex_colours() {
        assert!(matches!(
            parse_value(serde_json::json!({"wat": true})),
            Evaluation::Invalid(_)
        ));
        assert!(matches!(
            parse_value(serde_json::json!({"rail": "red"})),
            Evaluation::Invalid(_)
        ));
        // Pane borders were retired in favour of the rail colour.
        assert!(matches!(
            parse_value(serde_json::json!({"border": "#ff0000"})),
            Evaluation::Invalid(_)
        ));
    }

    #[test]
    fn only_cue_files_and_module_metadata_trigger_reload() {
        let event = Event::new(EventKind::Any).add_path(PathBuf::from("x/terminal.cue"));
        assert!(relevant_change(&event));
        let event = Event::new(EventKind::Any).add_path(PathBuf::from("x/readme.md"));
        assert!(!relevant_change(&event));
    }

    #[test]
    fn evaluates_a_real_cue_package_in_the_exact_target_directory() {
        let root = std::env::temp_dir().join(format!("cuetty-cue-test-{}", std::process::id()));
        let target = root.join("target");
        std::fs::create_dir_all(root.join("cue.mod")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(
            target.join("terminal.cue"),
            "package cuetty\nbanner: \"Welcome to Cuetty\"\nrail: \"#ff0000\"\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&target).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Valid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_without_module_evaluates() {
        let root =
            std::env::temp_dir().join(format!("cuetty-cue-no-module-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("anything.cue"),
            "package cuetty\nbanner: \"Hi\"\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Valid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A directory and a global directory, each holding `package cuetty`
    /// sources (`None` leaves that directory without the package).
    fn layered_source(
        name: &str,
        local: Option<&str>,
        global: Option<&str>,
    ) -> (PathBuf, ConfigSource) {
        let root = std::env::temp_dir().join(format!("cuetty-cue-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (local_dir, global_dir) = (root.join("project"), root.join("config/cuetty"));
        for (dir, body) in [(&local_dir, local), (&global_dir, global)] {
            std::fs::create_dir_all(dir).unwrap();
            if let Some(body) = body {
                std::fs::write(
                    dir.join("any-name.cue"),
                    format!("package cuetty\n{body}\n"),
                )
                .unwrap();
            }
        }
        let source = ConfigSource::in_dir(&local_dir)
            .unwrap()
            .with_global_dir(Some(global_dir));
        (root, source)
    }

    #[test]
    fn global_package_unifies_beneath_directory_package() {
        let (root, source) = layered_source(
            "layered",
            Some("rail: \"#ff0000\""),
            Some("rail: *\"#89b4fa\" | string\ntitlebar: false\nbanner: \"global\""),
        );
        assert_eq!(
            source.evaluate(),
            Evaluation::Valid(CuettyPresentation {
                banner: Some("global".into()),
                rail: Some(RailColor(255, 0, 0)),
                titlebar: TitlebarVisibility::Hidden,
                keybindings: Keybindings::default(),
            })
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn global_package_applies_alone_without_a_directory_package() {
        let (root, source) = layered_source("global-only", None, Some("titlebar: false"));
        assert!(matches!(
            source.evaluate(),
            Evaluation::Valid(CuettyPresentation {
                titlebar: TitlebarVisibility::Hidden,
                ..
            })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflicting_global_and_directory_values_are_invalid() {
        let (root, source) = layered_source(
            "conflict",
            Some("rail: \"#ff0000\""),
            Some("rail: \"#89b4fa\""),
        );
        assert!(matches!(source.evaluate(), Evaluation::Invalid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_global_directory_is_neutral() {
        let (root, source) = layered_source("no-global", Some("banner: \"local\""), None);
        let source = source.with_global_dir(Some(root.join("does-not-exist")));
        assert!(matches!(source.evaluate(), Evaluation::Valid(_)));
        let (empty_root, empty) = layered_source("neither", None, None);
        assert_eq!(empty.evaluate(), Evaluation::Absent);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(empty_root).unwrap();
    }

    #[test]
    fn global_config_dir_follows_xdg_with_home_fallback() {
        assert_eq!(
            global_config_dir_from(Some("/xdg".into()), Some("/home/me".into())),
            Some(PathBuf::from("/xdg/cuetty"))
        );
        assert_eq!(
            global_config_dir_from(Some("relative".into()), Some("/home/me".into())),
            Some(PathBuf::from("/home/me/.config/cuetty"))
        );
        assert_eq!(global_config_dir_from(None, None), None);
    }

    #[test]
    fn standalone_unrelated_package_is_absent() {
        let root =
            std::env::temp_dir().join(format!("cuetty-cue-unrelated-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("other.cue"),
            "package unrelated\nvalue: \"not cuetty\"\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        let evaluation = source.evaluate();
        assert!(matches!(evaluation, Evaluation::Absent));
        assert!(matches!(source.evaluate_tasks(), TaskEvaluation::Absent));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_evaluation_does_not_inherit_a_parent_package() {
        let root = std::env::temp_dir().join(format!("cuetty-cue-scope-{}", std::process::id()));
        let parent = root.join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(parent.join("cue.mod")).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            parent.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty-scope\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(
            parent.join("parent.cue"),
            "package cuetty\nbanner: \"Parent\"\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&child).unwrap();
        let evaluation = source.evaluate();
        assert!(matches!(evaluation, Evaluation::Absent));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_matching_package_is_an_explicit_failure() {
        let root = std::env::temp_dir().join(format!("cuetty-cue-invalid-{}", std::process::id()));
        std::fs::create_dir_all(root.join("cue.mod")).unwrap();
        std::fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty-invalid\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(root.join("terminal.cue"), "package cuetty\nrail: #ff0000\n").unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Invalid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn syntax_error_in_matching_package_is_an_explicit_failure() {
        let root =
            std::env::temp_dir().join(format!("cuetty-cue-syntax-invalid-{}", std::process::id()));
        std::fs::create_dir_all(root.join("cue.mod")).unwrap();
        std::fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty-syntax-invalid\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("terminal.cue"),
            "package cuetty\nbanner: \"unterminated\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Invalid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn syntax_error_in_unrelated_package_is_neutral_absence() {
        let root = std::env::temp_dir().join(format!(
            "cuetty-cue-unrelated-syntax-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("cue.mod")).unwrap();
        std::fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty-unrelated-syntax\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("other.cue"),
            "package unrelated\nbanner: \"unterminated\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        let evaluation = source.evaluate();
        assert!(matches!(evaluation, Evaluation::Absent));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn valid_matching_package_survives_invalid_unrelated_file() {
        let root = std::env::temp_dir().join(format!(
            "cuetty-cue-mixed-unrelated-invalid-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("cue.mod")).unwrap();
        std::fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty-mixed-unrelated-invalid\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(root.join("terminal.cue"), "package cuetty\nbanner: \"A\"\n").unwrap();
        std::fs::write(
            root.join("other.cue"),
            "package unrelated\nbanner: \"unterminated\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        assert!(matches!(
            source.evaluate(),
            Evaluation::Valid(CuettyPresentation {
                banner: Some(ref banner),
                ..
            }) if banner == "A"
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_matching_package_is_not_hidden_by_valid_unrelated_file() {
        let root = std::env::temp_dir().join(format!(
            "cuetty-cue-mixed-matching-invalid-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("cue.mod")).unwrap();
        std::fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.test/cuetty-mixed-matching-invalid\"\nlanguage: version: \"v0.14.1\"\n",
        )
        .unwrap();
        std::fs::write(root.join("other.cue"), "package unrelated\nvalue: \"ok\"\n").unwrap();
        std::fs::write(
            root.join("terminal.cue"),
            "package cuetty\nbanner: \"unterminated\n",
        )
        .unwrap();
        let source = ConfigSource::in_dir(&root).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Invalid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_the_repository_cuenv_task_catalog() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("cuetty lives below the repository root");
        let source = ConfigSource::in_dir(root).expect("repository CUE source");
        let TaskEvaluation::Valid(tasks) = source.evaluate_tasks() else {
            panic!("repository package cuenv should evaluate into a task catalog");
        };
        assert!(tasks.iter().any(|task| task.name == "checks.clippy"));
        assert!(tasks.iter().any(|task| task.name == "docs.build"));
    }
}
