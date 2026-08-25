//! Cuenv-backed, presentation-only configuration for Cuetty.
//!
//! CUE is evaluated on a worker thread.  The terminal UI receives only this
//! validated representation, so configuration cannot reach a renderer or a
//! session directly.

use cuengine::ModuleEvalOptions;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const PACKAGE: &str = "cuetty";
const MAX_BANNER_BYTES: usize = 120;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CuettyPresentation {
    pub banner: Option<String>,
    pub border: Option<BorderColor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorderColor(pub u8, pub u8, pub u8);

impl BorderColor {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CuettyDto {
    #[serde(default)]
    banner: Option<String>,
    #[serde(default)]
    border: Option<String>,
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
    pub notice: Option<String>,
}

impl CuenvPaneState {
    pub fn observe_cwd(&mut self, cwd: PathBuf) -> bool {
        if self.cwd.as_ref() == Some(&cwd) {
            return false;
        }
        self.cwd = Some(cwd);
        self.generation = self.generation.wrapping_add(1);
        self.presentation = CuettyPresentation::default();
        self.notice = None;
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigSource {
    pub module_root: Option<PathBuf>,
    pub target_dir: PathBuf,
}

impl ConfigSource {
    /// Select configuration from the shell's current working directory.
    /// Any `.cue` file in that directory may provide package `cuetty`.
    pub fn from_cwd(start: &Path) -> Result<Self, String> {
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
        })
    }

    pub fn evaluate(&self) -> Evaluation {
        let module_root = self.module_root.as_deref().unwrap_or(&self.target_dir);
        let options = ModuleEvalOptions {
            recursive: false,
            package_name: Some(PACKAGE.into()),
            target_dir: Some(self.target_dir.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let module = match cuengine::evaluate_module(module_root, PACKAGE, Some(&options)) {
            Ok(module) => module,
            Err(error) => {
                return Evaluation::Invalid(format!("Cuetty CUE evaluation failed: {error}"));
            }
        };
        let Some(value) = module
            .instances
            .get(".")
            .or_else(|| module.instances.values().next())
        else {
            // The public Cuenv evaluator found no instance for package
            // `cuetty` in this exact directory. No presentation applies.
            return Evaluation::Absent;
        };
        parse_value(value.clone())
    }
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
    let border = match dto.border {
        Some(value) => match parse_color(&value) {
            Ok(color) => Some(color),
            Err(error) => return Evaluation::Invalid(error),
        },
        None => None,
    };
    Evaluation::Valid(CuettyPresentation { banner, border })
}

fn parse_color(value: &str) -> Result<BorderColor, String> {
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return Err("border must use quoted #RRGGBB syntax".into());
    }
    let channel = |from| {
        u8::from_str_radix(&value[from..from + 2], 16)
            .map_err(|_| "border must use quoted #RRGGBB syntax".to_string())
    };
    Ok(BorderColor(channel(1)?, channel(3)?, channel(5)?))
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
            parse_value(serde_json::json!({"banner": "Welcome to Cuetty", "border": "#ff0000"})),
            Evaluation::Valid(CuettyPresentation {
                banner: Some("Welcome to Cuetty".into()),
                border: Some(BorderColor(255, 0, 0))
            })
        );
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
                border: Some(BorderColor(255, 0, 0)),
            })
        ));
        assert!(state.observe_cwd(PathBuf::from("/b")));
        assert_eq!(state.presentation, CuettyPresentation::default());
        assert!(!state.apply(
            generation_a,
            Evaluation::Valid(CuettyPresentation {
                banner: Some("late A".into()),
                border: None,
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
            border: None,
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
            border: Some(BorderColor(255, 0, 0)),
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
    fn rejects_unknown_fields_and_non_hex_colours() {
        assert!(matches!(
            parse_value(serde_json::json!({"wat": true})),
            Evaluation::Invalid(_)
        ));
        assert!(matches!(
            parse_value(serde_json::json!({"border": "red"})),
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
            "package cuetty\nbanner: \"Welcome to Cuetty\"\nborder: \"#ff0000\"\n",
        )
        .unwrap();
        let source = ConfigSource::from_cwd(&target).unwrap();
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
        let source = ConfigSource::from_cwd(&root).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Valid(_)));
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
        let source = ConfigSource::from_cwd(&root).unwrap();
        let evaluation = source.evaluate();
        assert!(matches!(evaluation, Evaluation::Absent));
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
        let source = ConfigSource::from_cwd(&child).unwrap();
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
        std::fs::write(
            root.join("terminal.cue"),
            "package cuetty\nborder: #ff0000\n",
        )
        .unwrap();
        let source = ConfigSource::from_cwd(&root).unwrap();
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
        let source = ConfigSource::from_cwd(&root).unwrap();
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
        let source = ConfigSource::from_cwd(&root).unwrap();
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
        let source = ConfigSource::from_cwd(&root).unwrap();
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
        let source = ConfigSource::from_cwd(&root).unwrap();
        assert!(matches!(source.evaluate(), Evaluation::Invalid(_)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
