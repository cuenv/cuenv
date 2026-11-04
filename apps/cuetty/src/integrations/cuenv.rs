//! Cuenv-backed, presentation-only configuration for Cuetty.
//!
//! CUE is evaluated on a worker thread.  The terminal UI receives only this
//! validated representation, so configuration cannot reach a renderer or a
//! session directly.

use crate::terminal::config::{Rgb, TerminalConfig};
use cuengine::{ModuleEvalOptions, UnifySource};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PACKAGE: &str = "cuetty";
pub const PROJECT_PACKAGE: &str = "cuenv";
/// Directory below the XDG config home that holds the global `cuetty` package.
const GLOBAL_CONFIG_DIR: &str = "cuetty";
const MAX_BANNER_BYTES: usize = 120;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CuettyPresentation {
    pub banner: Option<String>,
    pub rail: Option<RailColor>,
    pub titlebar: TitlebarVisibility,
    pub sidebar: SidebarMode,
    pub keybindings: Keybindings,
    pub terminal: Box<TerminalConfig>,
}

impl CuettyPresentation {
    /// What a pane shows while its own evaluation is on the way: the same look,
    /// so a `cd` or a new tab does not flash the defaults and resize the grid, but
    /// nothing that identifies another project.
    pub fn carried_over(&self) -> Self {
        Self {
            banner: None,
            rail: None,
            ..self.clone()
        }
    }
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

    /// The chord as this platform writes it in UI hints.
    pub fn label(&self) -> String {
        if cfg!(target_os = "macos") {
            self.glyph_label()
        } else {
            self.word_label()
        }
    }

    /// Modifiers in the order macOS menus show them, for example `⇧⌘P`.
    fn glyph_label(&self) -> String {
        let ChordModifiers {
            control,
            alt,
            shift,
            platform,
        } = self.modifiers;
        [(control, "⌃"), (alt, "⌥"), (shift, "⇧"), (platform, "⌘")]
            .into_iter()
            .filter_map(|(held, glyph)| held.then_some(glyph))
            .chain([self.key_label().as_str()])
            .collect()
    }

    /// Conventional Linux spelling, for example `Ctrl+Shift+P`.
    fn word_label(&self) -> String {
        let ChordModifiers {
            control,
            alt,
            shift,
            platform,
        } = self.modifiers;
        [
            (control, "Ctrl"),
            (alt, "Alt"),
            (shift, "Shift"),
            (platform, "Super"),
        ]
        .into_iter()
        .filter_map(|(held, word)| held.then_some(word))
        .chain([self.key_label().as_str()])
        .collect::<Vec<_>>()
        .join("+")
    }

    fn key_label(&self) -> String {
        let mut characters = self.key.chars();
        characters.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(characters).collect()
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

/// How much room the tab sidebar takes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SidebarMode {
    /// Tabs are listed by name.
    #[default]
    Expanded,
    /// A narrow strip of initials, with names in tooltips.
    Collapsed,
}

/// The sidebar of one window. `sidebar` says how it starts, so only the first
/// configuration the window evaluates is read, and the toggle replaces even that.
/// Following the configuration any longer would resize the grid on every `cd`.
///
/// That evaluation lands after the first frame, so until then the window goes by
/// what the global package says, which it nearly always agrees with.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WindowSidebar {
    settled: Option<SidebarMode>,
    expected: SidebarMode,
}

impl WindowSidebar {
    pub fn mode(self) -> SidebarMode {
        self.settled.unwrap_or(self.expected)
    }

    /// A sidebar opening on what `global` says, until something settles it.
    pub fn expecting(global: &Evaluation) -> Self {
        Self {
            settled: None,
            expected: match global {
                Evaluation::Valid(presentation) => presentation.sidebar,
                Evaluation::Absent | Evaluation::Invalid(_) => SidebarMode::default(),
            },
        }
    }

    /// Settles the sidebar from an evaluation, unless something already has. An
    /// invalid configuration says nothing, so the next valid one still counts.
    pub fn observe(&mut self, evaluation: &Evaluation) {
        let configured = match evaluation {
            Evaluation::Valid(presentation) => presentation.sidebar,
            Evaluation::Absent => SidebarMode::default(),
            Evaluation::Invalid(_) => return,
        };
        self.settled.get_or_insert(configured);
    }

    pub fn toggle(&mut self) {
        self.settled = Some(self.mode().toggled());
    }
}

impl SidebarMode {
    pub const fn toggled(self) -> Self {
        match self {
            Self::Expanded => Self::Collapsed,
            Self::Collapsed => Self::Expanded,
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "expanded" => Ok(Self::Expanded),
            "collapsed" => Ok(Self::Collapsed),
            _ => Err(format!(
                "sidebar must be \"expanded\" or \"collapsed\", got \"{value}\""
            )),
        }
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
    accent: Option<String>,
    #[serde(default)]
    titlebar: Option<bool>,
    #[serde(default)]
    sidebar: Option<String>,
    #[serde(default)]
    keybindings: Option<KeybindingsDto>,
    #[serde(default)]
    font: Option<FontDto>,
    #[serde(default)]
    padding: Option<PaddingDto>,
    #[serde(default)]
    transparency: Option<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FontDto {
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    size: Option<f32>,
    #[serde(default)]
    line_height: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PaddingDto {
    #[serde(default)]
    top: Option<u16>,
    #[serde(default)]
    right: Option<u16>,
    #[serde(default)]
    bottom: Option<u16>,
    #[serde(default)]
    left: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeybindingsDto {
    #[serde(default)]
    palette: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Evaluation {
    Absent,
    Valid(CuettyPresentation),
    Invalid(String),
}

/// Per-pane presentation state kept independent from the GPUI view. The
/// generation is advanced whenever the shell enters a new directory or a new
/// source binding, so late worker results cannot cross a CWD boundary.
#[derive(Clone, Debug, Default, PartialEq)]
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
        self.set_cwd(Some(cwd))
    }

    pub fn forget_cwd(&mut self) -> bool {
        self.set_cwd(None)
    }

    fn set_cwd(&mut self, cwd: Option<PathBuf>) -> bool {
        if self.cwd == cwd {
            return false;
        }
        self.cwd = cwd;
        self.generation = self.generation.wrapping_add(1);
        self.presentation = self.presentation.carried_over();
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
    /// Use the exact shell directory when known; otherwise load only the
    /// global package until the terminal reports a working directory.
    pub fn from_working_directory(cwd: Option<&Path>) -> Result<Option<Self>, String> {
        Self::from_directories(cwd, global_config_dir())
    }

    fn from_directories(
        cwd: Option<&Path>,
        global: Option<PathBuf>,
    ) -> Result<Option<Self>, String> {
        match cwd {
            Some(cwd) => Self::in_dir(cwd).map(|source| Some(source.with_global_dir(global))),
            None => global
                .filter(|dir| dir.is_dir())
                .map(|dir| Self::in_dir(&dir))
                .transpose(),
        }
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
        Some(value) => match parse_color("rail", &value) {
            Ok(color) => Some(color),
            Err(error) => return Evaluation::Invalid(error),
        },
        None => None,
    };
    let accent = match dto
        .accent
        .as_deref()
        .map(|value| parse_color("accent", value))
    {
        Some(Ok(RailColor(red, green, blue))) => Some(Rgb(red, green, blue)),
        Some(Err(error)) => return Evaluation::Invalid(error),
        None => None,
    };
    let sidebar = match dto.sidebar.as_deref().map(SidebarMode::parse).transpose() {
        Ok(sidebar) => sidebar.unwrap_or_default(),
        Err(error) => return Evaluation::Invalid(error),
    };
    let keybindings = match dto.keybindings.and_then(|keys| keys.palette) {
        Some(value) => match KeyChord::parse(&value) {
            Ok(palette) => Keybindings { palette },
            Err(error) => return Evaluation::Invalid(error),
        },
        None => Keybindings::default(),
    };
    // Apply built-in defaults only after global and directory CUE packages
    // have unified, so any individual setting can be overridden locally.
    let mut terminal = TerminalConfig::default();
    if let Some(font) = dto.font {
        if let Some(family) = font.family {
            terminal.font.family = family;
        }
        if let Some(size) = font.size {
            terminal.font.size_px = size;
        }
        if let Some(line_height) = font.line_height {
            terminal.font.line_height_multiplier = line_height;
        }
    }
    if let Some(padding) = dto.padding {
        let edges = &mut terminal.terminal_padding;
        if let Some(top) = padding.top {
            edges.top = top;
        }
        if let Some(right) = padding.right {
            edges.right = right;
        }
        if let Some(bottom) = padding.bottom {
            edges.bottom = bottom;
        }
        if let Some(left) = padding.left {
            edges.left = left;
        }
    }
    if let Some(accent) = accent {
        terminal.theme.accent = accent;
    }
    if let Some(transparency) = dto.transparency {
        terminal.terminal_transparency_percent = transparency;
    }
    if let Err(error) = terminal.validate() {
        return Evaluation::Invalid(format!("invalid Cuetty terminal configuration: {error}"));
    }
    Evaluation::Valid(CuettyPresentation {
        banner,
        rail,
        titlebar: dto
            .titlebar
            .map(TitlebarVisibility::from)
            .unwrap_or_default(),
        sidebar,
        keybindings,
        terminal: Box::new(terminal),
    })
}

fn parse_color(field: &str, value: &str) -> Result<RailColor, String> {
    let invalid = || format!("{field} must use quoted #RRGGBB syntax");
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid());
    }
    let channel = |from| u8::from_str_radix(&value[from..from + 2], 16).map_err(|_| invalid());
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

/// Evaluates `source` if that takes no longer than `wait`. For configuration that
/// should be in place before something is shown, but must never hold it up for long:
/// a slow evaluation is left to finish on its own, and its result dropped.
pub fn evaluate_within(
    provider: Arc<dyn CuenvProvider>,
    source: ConfigSource,
    wait: Duration,
) -> Option<Evaluation> {
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The receiver has gone if this took longer than it would wait.
        let _ = result_tx.send(provider.evaluate(&source));
    });
    result_rx.recv_timeout(wait).ok()
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
                sidebar: SidebarMode::Expanded,
                keybindings: Keybindings::default(),
                terminal: Box::default(),
            })
        );
    }

    #[test]
    fn terminal_presentation_supports_partial_configuration_and_defaults() {
        let Evaluation::Valid(presentation) = parse_value(serde_json::json!({
            "font": {"family": "Monaspace Neon", "size": 21, "lineHeight": 1.5},
            "padding": {"left": 2, "top": 12},
            "transparency": 40
        })) else {
            panic!("expected valid terminal presentation");
        };
        let terminal = presentation.terminal;
        assert_eq!(terminal.font.family, "Monaspace Neon");
        assert_eq!(terminal.font.size_px, 21.0);
        assert_eq!(terminal.font.line_height_multiplier, 1.5);
        assert_eq!(terminal.terminal_padding.top, 12);
        assert_eq!(terminal.terminal_padding.right, 14);
        assert_eq!(terminal.terminal_padding.bottom, 14);
        assert_eq!(terminal.terminal_padding.left, 2);
        assert_eq!(terminal.terminal_transparency_percent, 40);
    }

    #[test]
    fn terminal_presentation_rejects_invalid_fields_and_values() {
        for value in [
            serde_json::json!({"font": {"size": 0}}),
            serde_json::json!({"font": {"size": 129}}),
            serde_json::json!({"font": {"lineHeight": 0.49}}),
            serde_json::json!({"font": {"lineHeight": 3.01}}),
            serde_json::json!({"font": {"family": ""}}),
            serde_json::json!({"font": {"family": "   "}}),
            serde_json::json!({"font": {"unknown": "no"}}),
            serde_json::json!({"padding": {"top": -1}}),
            serde_json::json!({"padding": {"right": 129}}),
            serde_json::json!({"padding": {"left": 1.5}}),
            serde_json::json!({"padding": {"horizontal": 2}}),
            serde_json::json!({"transparency": 101}),
            serde_json::json!({"transparency": -1}),
        ] {
            assert!(
                matches!(parse_value(value.clone()), Evaluation::Invalid(_)),
                "{value}"
            );
        }
        for value in [
            serde_json::json!({"font": {"size": 1, "lineHeight": 0.5}, "padding": {"top": 0}, "transparency": 0}),
            serde_json::json!({"font": {"size": 128, "lineHeight": 3}, "padding": {"top": 128}, "transparency": 100}),
        ] {
            assert!(
                matches!(parse_value(value.clone()), Evaluation::Valid(_)),
                "{value}"
            );
        }
    }

    #[test]
    fn invalid_terminal_reload_keeps_the_previous_presentation() {
        let mut state = CuenvPaneState::default();
        state.observe_cwd(PathBuf::from("/a"));
        let generation = state.begin_source();
        let valid = parse_value(serde_json::json!({"padding": {"left": 15}}));
        assert!(state.apply(generation, valid));
        assert!(state.apply(
            generation,
            parse_value(serde_json::json!({"padding": {"left": 129}}))
        ));
        assert_eq!(state.presentation.terminal.terminal_padding.left, 15);
        assert!(state.notice.is_some());
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
    fn the_accent_sets_the_theme_and_leaves_the_project_rail_alone() {
        let Evaluation::Valid(presentation) =
            parse_value(serde_json::json!({"accent": "#a6e3a1", "rail": "#ff0000"}))
        else {
            panic!("expected a valid presentation");
        };
        assert_eq!(presentation.terminal.theme.accent, Rgb(166, 227, 161));
        assert_eq!(presentation.rail, Some(RailColor(255, 0, 0)));

        let Evaluation::Valid(presentation) = parse_value(serde_json::json!({"rail": "#ff0000"}))
        else {
            panic!("expected a valid presentation");
        };
        assert_eq!(
            presentation.terminal.theme.accent,
            TerminalConfig::default().theme.accent
        );
    }

    #[test]
    fn colour_errors_name_the_field_at_fault() {
        for (value, field) in [
            (serde_json::json!({"accent": "green"}), "accent"),
            (serde_json::json!({"accent": "#aéabc"}), "accent"),
            (serde_json::json!({"rail": "red"}), "rail"),
        ] {
            let Evaluation::Invalid(error) = parse_value(value) else {
                panic!("expected an invalid presentation");
            };
            assert!(error.starts_with(field), "{error}");
        }
    }

    #[test]
    fn a_global_accent_coexists_with_a_directory_rail() {
        let (root, source) = layered_source(
            "accent-layered",
            Some("rail: \"#ff0000\""),
            Some("accent: \"#a6e3a1\""),
        );
        let Evaluation::Valid(presentation) = source.evaluate() else {
            panic!("expected a unified presentation");
        };
        assert_eq!(presentation.terminal.theme.accent, Rgb(166, 227, 161));
        assert_eq!(presentation.rail, Some(RailColor(255, 0, 0)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sidebar_defaults_to_expanded_and_can_start_collapsed() {
        let sidebar = |value| match parse_value(value) {
            Evaluation::Valid(presentation) => presentation.sidebar,
            other => panic!("unexpected evaluation: {other:?}"),
        };
        assert_eq!(sidebar(serde_json::json!({})), SidebarMode::Expanded);
        assert_eq!(
            sidebar(serde_json::json!({"sidebar": "expanded"})),
            SidebarMode::Expanded
        );
        assert_eq!(
            sidebar(serde_json::json!({"sidebar": "collapsed"})),
            SidebarMode::Collapsed
        );
        for invalid in [
            serde_json::json!({"sidebar": "Collapsed"}),
            serde_json::json!({"sidebar": "hidden"}),
            serde_json::json!({"sidebar": false}),
        ] {
            assert!(
                matches!(parse_value(invalid.clone()), Evaluation::Invalid(_)),
                "{invalid}"
            );
        }
    }

    fn evaluation_with_sidebar(sidebar: SidebarMode) -> Evaluation {
        Evaluation::Valid(CuettyPresentation {
            sidebar,
            ..CuettyPresentation::default()
        })
    }

    #[test]
    fn only_the_first_evaluation_a_window_sees_sets_its_sidebar() {
        let mut sidebar = WindowSidebar::default();
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
        sidebar.observe(&Evaluation::Invalid("broken".into()));
        sidebar.observe(&evaluation_with_sidebar(SidebarMode::Collapsed));
        assert_eq!(sidebar.mode(), SidebarMode::Collapsed);

        // Neither another directory's setting nor having none moves it.
        sidebar.observe(&evaluation_with_sidebar(SidebarMode::Expanded));
        sidebar.observe(&Evaluation::Absent);
        assert_eq!(sidebar.mode(), SidebarMode::Collapsed);
    }

    #[test]
    fn a_window_opens_on_the_global_sidebar_until_an_evaluation_settles_it() {
        let mut sidebar =
            WindowSidebar::expecting(&evaluation_with_sidebar(SidebarMode::Collapsed));
        assert_eq!(sidebar.mode(), SidebarMode::Collapsed);
        // Saying nothing leaves it where it opened, not on the default.
        sidebar.observe(&Evaluation::Invalid("broken".into()));
        assert_eq!(sidebar.mode(), SidebarMode::Collapsed);
        // The directory it opened in has the last word.
        sidebar.observe(&evaluation_with_sidebar(SidebarMode::Expanded));
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
    }

    #[test]
    fn a_broken_global_package_opens_on_the_default_sidebar() {
        let sidebar = WindowSidebar::expecting(&Evaluation::Invalid("broken".into()));
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
        assert_eq!(
            WindowSidebar::expecting(&Evaluation::Absent).mode(),
            SidebarMode::Expanded
        );
    }

    #[test]
    fn the_toggle_works_from_where_the_window_opened() {
        let mut sidebar =
            WindowSidebar::expecting(&evaluation_with_sidebar(SidebarMode::Collapsed));
        sidebar.toggle();
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
        sidebar.observe(&evaluation_with_sidebar(SidebarMode::Collapsed));
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
    }

    /// Evaluates once `release` says to, and to nothing in particular.
    struct GatedProvider {
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl CuenvProvider for GatedProvider {
        fn evaluate(&self, _source: &ConfigSource) -> Evaluation {
            let _ = self.release.lock().unwrap().recv();
            Evaluation::Absent
        }

        fn watch(
            &self,
            _source: &ConfigSource,
            _notify: async_channel::Sender<()>,
        ) -> Result<Box<dyn CuenvWatcher>, String> {
            Err("not watched".into())
        }
    }

    fn gated_provider() -> (Arc<dyn CuenvProvider>, std::sync::mpsc::Sender<()>) {
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let provider = GatedProvider {
            release: Mutex::new(release_rx),
        };
        (Arc::new(provider), release_tx)
    }

    fn any_source() -> ConfigSource {
        ConfigSource {
            module_root: None,
            target_dir: PathBuf::from("/nowhere"),
            global_dir: None,
        }
    }

    #[test]
    fn an_evaluation_in_time_is_returned() {
        let (provider, release) = gated_provider();
        release.send(()).unwrap();
        let evaluation = evaluate_within(provider, any_source(), Duration::from_secs(30));
        assert_eq!(evaluation, Some(Evaluation::Absent));
    }

    #[test]
    fn an_evaluation_that_takes_too_long_is_given_up_on() {
        // Never released, so it is still going when the wait runs out.
        let (provider, _release) = gated_provider();
        let evaluation = evaluate_within(provider, any_source(), Duration::from_millis(1));
        assert_eq!(evaluation, None);
    }

    #[test]
    fn a_directory_without_configuration_settles_the_sidebar_on_its_default() {
        let mut sidebar = WindowSidebar::default();
        sidebar.observe(&Evaluation::Absent);
        sidebar.observe(&evaluation_with_sidebar(SidebarMode::Collapsed));
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
    }

    #[test]
    fn the_toggle_outranks_an_evaluation_still_on_its_way() {
        let mut sidebar = WindowSidebar::default();
        sidebar.toggle();
        assert_eq!(sidebar.mode(), SidebarMode::Collapsed);
        sidebar.observe(&evaluation_with_sidebar(SidebarMode::Expanded));
        assert_eq!(sidebar.mode(), SidebarMode::Collapsed);
        sidebar.toggle();
        assert_eq!(sidebar.mode(), SidebarMode::Expanded);
    }

    #[test]
    fn a_new_directory_keeps_the_look_but_not_the_project_until_it_evaluates() {
        let mut state = CuenvPaneState::default();
        state.observe_cwd(PathBuf::from("/a"));
        let generation = state.begin_source();
        let Evaluation::Valid(mut presentation) = parse_value(serde_json::json!({
            "banner": "A",
            "rail": "#ff0000",
            "accent": "#00ff00",
            "titlebar": false,
            "padding": {"left": 4},
        })) else {
            panic!("the configuration is valid");
        };
        presentation.sidebar = SidebarMode::Collapsed;
        assert!(state.apply(generation, Evaluation::Valid(presentation.clone())));

        assert!(state.observe_cwd(PathBuf::from("/b")));
        assert_eq!(state.presentation.banner, None);
        assert_eq!(state.presentation.rail, None);
        assert_eq!(state.presentation.titlebar, presentation.titlebar);
        assert_eq!(state.presentation.sidebar, SidebarMode::Collapsed);
        assert_eq!(state.presentation.terminal, presentation.terminal);

        // The new directory's own evaluation still decides in the end.
        let generation = state.begin_source();
        assert!(state.apply(generation, Evaluation::Absent));
        assert_eq!(state.presentation, CuettyPresentation::default());
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
                sidebar: SidebarMode::Expanded,
                keybindings: Keybindings::default(),
                terminal: Box::default(),
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
                sidebar: SidebarMode::Expanded,
                keybindings: Keybindings::default(),
                terminal: Box::default(),
            })
        ));
    }

    #[test]
    fn losing_cwd_fences_the_old_source_and_preserves_global_reloads() {
        let mut state = CuenvPaneState::default();
        assert!(state.observe_cwd(PathBuf::from("/project")));
        let old_generation = state.begin_source();
        assert!(state.apply(
            old_generation,
            parse_value(serde_json::json!({"padding": {"left": 4}}))
        ));

        assert!(state.forget_cwd());
        assert_eq!(state.presentation.terminal.terminal_padding.left, 4);
        assert!(!state.apply(
            old_generation,
            parse_value(serde_json::json!({"padding": {"left": 9}}))
        ));

        let global_generation = state.begin_source();
        assert!(state.apply(
            global_generation,
            parse_value(serde_json::json!({"padding": {"left": 12}}))
        ));
        assert!(!state.forget_cwd());
        assert_eq!(state.presentation.terminal.terminal_padding.left, 12);
        assert!(state.apply(
            global_generation,
            parse_value(serde_json::json!({"padding": {"left": 129}}))
        ));
        assert_eq!(state.presentation.terminal.terminal_padding.left, 12);

        assert!(state.observe_cwd(PathBuf::from("/project")));
        assert!(!state.apply(
            global_generation,
            parse_value(serde_json::json!({"padding": {"left": 7}}))
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
            sidebar: SidebarMode::Expanded,
            keybindings: Keybindings::default(),
            terminal: Box::default(),
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
            sidebar: SidebarMode::Expanded,
            keybindings: Keybindings::default(),
            terminal: Box::default(),
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
    fn chord_labels_follow_each_platform_convention() {
        let chord = KeyChord::parse("ctrl-shift-p").unwrap();
        assert_eq!(chord.word_label(), "Ctrl+Shift+P");
        assert_eq!(chord.glyph_label(), "⌃⇧P");
        let chord = KeyChord::parse("cmd-alt-pageup").unwrap();
        assert_eq!(chord.word_label(), "Alt+Super+Pageup");
        assert_eq!(chord.glyph_label(), "⌥⌘Pageup");
        assert_eq!(KeyChord::parse("k").unwrap().word_label(), "K");
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
        // A seven-byte value can still split a UTF-8 character at a channel
        // boundary; malformed input must be rejected without panicking.
        assert!(matches!(
            parse_value(serde_json::json!({"rail": "#aéabc"})),
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
                sidebar: SidebarMode::Expanded,
                keybindings: Keybindings::default(),
                terminal: Box::default(),
            })
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_settings_unify_before_defaults_are_applied() {
        let (root, source) = layered_source(
            "terminal-layered",
            Some("font: {family: \"Monaspace Neon\", size: 22}\npadding: left: 3"),
            Some(
                "font: {family: *\"Global Font\" | string, size: *16 | number, lineHeight: *1.4 | number}\n\
                 padding: {top: *10 | int, right: *11 | int, bottom: *12 | int, left: *13 | int}\n\
                 transparency: *30 | int",
            ),
        );
        let Evaluation::Valid(presentation) = source.evaluate() else {
            panic!("expected unified terminal presentation");
        };
        let terminal = presentation.terminal;
        assert_eq!(terminal.font.family, "Monaspace Neon");
        assert_eq!(terminal.font.size_px, 22.0);
        assert_eq!(terminal.font.line_height_multiplier, 1.4);
        assert_eq!(terminal.terminal_padding.top, 10);
        assert_eq!(terminal.terminal_padding.right, 11);
        assert_eq!(terminal.terminal_padding.bottom, 12);
        assert_eq!(terminal.terminal_padding.left, 3);
        assert_eq!(terminal.terminal_transparency_percent, 30);
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
    fn global_source_is_available_without_a_working_directory() {
        let root =
            std::env::temp_dir().join(format!("cuetty-global-without-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let global = root.join("config/cuetty");
        assert_eq!(
            ConfigSource::from_directories(None, Some(global.clone())).unwrap(),
            None
        );

        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("config.cue"),
            "package cuetty\npadding: left: 6\n",
        )
        .unwrap();
        let source = ConfigSource::from_directories(None, Some(global.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(
            ConfigSource::from_directories(None, Some(global.clone())).unwrap(),
            Some(source.clone())
        );
        assert!(
            matches!(source.evaluate(), Evaluation::Valid(presentation) if presentation.terminal.terminal_padding.left == 6)
        );

        // Discovering the global path as the actual CWD changes the binding,
        // so task discovery is enabled and any prior worker is fenced.
        assert_ne!(
            ConfigSource::from_directories(Some(&global), Some(global.clone())).unwrap(),
            Some(source)
        );
        std::fs::remove_dir_all(&global).unwrap();
        assert_eq!(
            ConfigSource::from_directories(None, Some(global.clone())).unwrap(),
            None
        );
        std::fs::create_dir_all(&global).unwrap();
        assert!(
            ConfigSource::from_directories(None, Some(global))
                .unwrap()
                .is_some()
        );
        std::fs::remove_dir_all(root).unwrap();
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
