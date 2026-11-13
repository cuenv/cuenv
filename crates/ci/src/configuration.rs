//! What a project's CUE configuration is built from.
//!
//! A task is not only affected by the files it declares as inputs. Its
//! command, its inputs and its dependencies are themselves computed from CUE:
//! the project's own files, the same-package files of the directories above
//! it, every package it imports, the files its `@embed` attributes read, and
//! the module's `cue.mod`. A change to any of those can change every task of
//! the project, and no declared input shows it. So such a change selects
//! every task of the project: coarse, and it fails closed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cuengine::InstanceSources;

/// The package cuenv evaluates in each project directory.
const PACKAGE: &str = "cuenv";

/// The files that, when one of them changes, change what a project's
/// configuration may evaluate to. Everything is relative to the root of the
/// checkout, the frame changed files are reported in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigurationSources {
    /// Directories that contribute, or may have contributed, `.cue` files to
    /// the project's instance: each directory one of its files is in, and
    /// every directory above it up to the module root.
    ///
    /// Any `.cue` file of such a directory counts, not only the files the
    /// instance is built from today: a file that was deleted is no longer
    /// among them, and a file that was added may join them. The directories
    /// above count because the last file of the package there may have been
    /// deleted, moved, given another package or excluded by a build tag, and
    /// the directory then contributes nothing the loader can report.
    directories: BTreeSet<PathBuf>,
    /// The paths and globs `@embed` attributes read.
    embeds: Vec<String>,
    /// The module's `cue.mod` directory: its language version and its
    /// dependencies, which are not among the instance's files.
    module_metadata: PathBuf,
}

/// Where to find the instance of a project.
#[derive(Debug, Clone, Copy)]
pub struct ProjectInstance<'project> {
    /// The root of the CUE module.
    pub module_root: &'project Path,
    /// The project's directory.
    pub directory: &'project Path,
    /// The module root relative to the root of the checkout: empty unless
    /// the module is nested in the repository.
    pub module_prefix: &'project Path,
}

impl ConfigurationSources {
    /// The sources of the project's instance, as the CUE loader resolves
    /// them. The instance is loaded and not evaluated.
    ///
    /// # Errors
    ///
    /// Fails when the instance cannot be loaded.
    pub fn load(project: ProjectInstance<'_>) -> cuenv_core::Result<Self> {
        let sources = cuengine::instance_sources(project.module_root, project.directory, PACKAGE)
            .map_err(|error| {
            cuenv_core::Error::configuration(format!(
                "Failed to list the CUE files of the project at {}: {error}",
                project.directory.display()
            ))
        })?;
        Ok(Self::placed(project.module_prefix, &sources))
    }

    /// Place what the loader reports, which is relative to the module root,
    /// in the checkout.
    #[must_use]
    pub fn placed(module_prefix: &Path, sources: &InstanceSources) -> Self {
        Self {
            directories: contributing_directories(sources)
                .into_iter()
                .map(|directory| module_prefix.join(directory))
                .collect(),
            embeds: sources
                .embeds
                .iter()
                .map(|embed| module_prefix.join(embed).to_string_lossy().into_owned())
                .collect(),
            module_metadata: module_prefix.join("cue.mod"),
        }
    }

    /// Whether one of the changed files can change the configuration.
    #[must_use]
    pub fn changed_by(&self, files: &[PathBuf]) -> bool {
        files.iter().any(|file| self.is_source(file))
    }

    fn is_source(&self, file: &Path) -> bool {
        if file.starts_with(&self.module_metadata) {
            return true;
        }
        let is_cue = file.extension().is_some_and(|extension| extension == "cue");
        if is_cue
            && self
                .directories
                .contains(file.parent().unwrap_or_else(|| Path::new("")))
        {
            return true;
        }
        let file = [file.to_path_buf()];
        self.embeds
            .iter()
            .any(|embed| cuenv_core::matches_pattern(&file, Path::new(""), embed))
    }
}

/// The path filters under which a workflow must start for a change to a
/// project's configuration, relative to the module root.
///
/// A directory that contributes `.cue` files is emitted as `<dir>/*.cue`,
/// so that adding a file to it does not change the generated workflow. An
/// `@embed` path is emitted as itself, and a glob as the directory it is
/// rooted in. A segment GitHub's filter syntax would read as a pattern cuts
/// the path at its parent.
#[must_use]
pub fn trigger_paths(sources: &InstanceSources) -> BTreeSet<String> {
    let directories = contributing_directories(sources);

    let mut paths = BTreeSet::from(["cue.mod/**".to_string()]);
    for directory in &directories {
        let segments = segments(directory);
        let literal = literal_segments(&segments);
        if literal.len() == segments.len() {
            let files = if literal.is_empty() {
                "*.cue".to_string()
            } else {
                format!("{}/*.cue", literal.join("/"))
            };
            paths.insert(files);
        } else {
            paths.insert(directory_filter(&literal));
        }
    }
    for embed in &sources.embeds {
        let embed = cuenv_vcs::pattern::normalize(embed);
        let segments = segments(Path::new(&embed));
        let literal = literal_segments(&segments);
        if literal.len() == segments.len() && !literal.is_empty() {
            paths.insert(literal.join("/"));
        } else {
            paths.insert(directory_filter(&literal));
        }
    }
    paths
}

/// Each directory of the instance's files and every directory above it,
/// relative to the module root; the module root itself is the empty path.
fn contributing_directories(sources: &InstanceSources) -> BTreeSet<PathBuf> {
    sources
        .files
        .iter()
        .flat_map(|file| {
            Path::new(file)
                .ancestors()
                .skip(1)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn segments(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The leading segments that can be emitted literally in a GitHub filter.
fn literal_segments(segments: &[String]) -> Vec<&str> {
    segments
        .iter()
        .map(String::as_str)
        .take_while(|segment| {
            !cuenv_vcs::pattern::is_glob(segment) && !is_special_in_github_filter(segment)
        })
        .collect()
}

/// The filter for everything beneath a directory given by its segments.
#[must_use]
pub(crate) fn directory_filter(segments: &[&str]) -> String {
    if segments.is_empty() {
        "**".to_string()
    } else {
        format!("{}/**", segments.join("/"))
    }
}

/// Whether a literal path segment would be read as a pattern by GitHub's
/// `paths` filters: `*`, `?`, `+` and `[]` are operators, `\` escapes, and a
/// leading `!` negates.
#[must_use]
pub(crate) fn is_special_in_github_filter(segment: &str) -> bool {
    segment.contains(['*', '?', '+', '[', ']', '\\']) || segment.starts_with('!')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(files: &[&str], embeds: &[&str]) -> InstanceSources {
        serde_json::from_value(serde_json::json!({"files": files, "embeds": embeds}))
            .expect("instance sources")
    }

    fn changed(configuration: &ConfigurationSources, file: &str) -> bool {
        configuration.changed_by(&[PathBuf::from(file)])
    }

    #[test]
    fn a_cue_file_in_a_contributing_directory_changes_the_configuration() {
        let configuration = ConfigurationSources::placed(
            Path::new(""),
            &sources(&["env.cue", "app/env.cue", "shared/names.cue"], &[]),
        );

        // The project's own file, an inherited one, an imported one.
        assert!(changed(&configuration, "app/env.cue"));
        assert!(changed(&configuration, "env.cue"));
        assert!(changed(&configuration, "shared/names.cue"));
        // A file that was added to, or deleted from, such a directory.
        assert!(changed(&configuration, "app/tasks.cue"));
        assert!(changed(&configuration, "base.cue"));
        // The module's own metadata and dependencies.
        assert!(changed(&configuration, "cue.mod/module.cue"));
        assert!(changed(&configuration, "cue.mod/pkg/example.com/x/x.cue"));

        assert!(!changed(&configuration, "docs/env.cue"));
        assert!(!changed(&configuration, "app/src/env.cue"));
        assert!(!changed(&configuration, "app/README.md"));
        assert!(!changed(&configuration, "shared/logo.svg"));
    }

    #[test]
    fn a_directory_above_a_contributing_one_counts_even_with_no_file_left() {
        // `services/defaults.cue` was the only file of the package in
        // `services/`, and it was deleted: the loader no longer reports it.
        let configuration =
            ConfigurationSources::placed(Path::new(""), &sources(&["services/api/env.cue"], &[]));

        assert!(changed(&configuration, "services/defaults.cue"));
        assert!(changed(&configuration, "root.cue"));
        assert!(!changed(&configuration, "services/web/env.cue"));
    }

    #[test]
    fn what_an_embed_reads_changes_the_configuration() {
        let configuration = ConfigurationSources::placed(
            Path::new(""),
            &sources(
                &["app/env.cue"],
                &["nix/source-files.json", "app/config/*.yaml"],
            ),
        );

        assert!(changed(&configuration, "nix/source-files.json"));
        assert!(changed(&configuration, "app/config/production.yaml"));
        assert!(!changed(&configuration, "app/config/readme.md"));
        assert!(!changed(&configuration, "nix/other.json"));
    }

    #[test]
    fn a_nested_module_is_placed_by_where_it_sits_in_the_checkout() {
        let configuration = ConfigurationSources::placed(
            Path::new("mono"),
            &sources(&["env.cue", "app/env.cue"], &["app/data.json"]),
        );

        assert!(changed(&configuration, "mono/app/env.cue"));
        assert!(changed(&configuration, "mono/env.cue"));
        assert!(changed(&configuration, "mono/cue.mod/module.cue"));
        assert!(changed(&configuration, "mono/app/data.json"));
        assert!(!changed(&configuration, "app/env.cue"));
        assert!(!changed(&configuration, "env.cue"));
        assert!(!changed(&configuration, "cue.mod/module.cue"));
    }

    #[test]
    fn trigger_paths_name_directories_so_a_new_file_does_not_change_the_workflow() {
        let paths = trigger_paths(&sources(
            &["env.cue", "base.cue", "app/env.cue", "shared/names.cue"],
            &["nix/source-files.json", "app/config/*.yaml", "**/*.json"],
        ));

        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            [
                "**",
                "*.cue",
                "app/*.cue",
                "app/config/**",
                "cue.mod/**",
                "nix/source-files.json",
                "shared/*.cue",
            ]
        );
    }

    #[test]
    fn a_directory_github_would_read_as_a_pattern_is_cut_at_its_parent() {
        let paths = trigger_paths(&sources(&["libs/c++/env.cue"], &["libs/c++/data.json"]));

        assert!(paths.contains("libs/**"), "{paths:?}");
        assert!(!paths.iter().any(|path| path.contains('+')), "{paths:?}");
    }
}
