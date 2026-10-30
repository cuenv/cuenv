//! Project names are unique within the evaluated package.
//!
//! Cross-project task references, CI pipelines and infrastructure state all
//! address a project by its `name`, so two projects with the same name (or
//! names differing only by case) in one package are an error wherever the
//! whole module is evaluated. A directory whose `name` is inherited from an
//! ancestor directory of the same package is part of that project, not a
//! second one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::ModuleEvaluation;
use crate::Error;

/// A project name declared by more than one project of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateProjectName {
    /// The name as the first project (by path) declares it.
    pub name: String,
    /// The directories of the projects that declare it, sorted, relative to
    /// the module root.
    pub paths: Vec<PathBuf>,
}

/// One project as the uniqueness check sees it: its directory relative to
/// the module root and its `name`.
#[derive(Debug, Clone, Copy)]
pub struct NamedProject<'project> {
    /// Directory of the project, relative to the module root (`.` for the
    /// root).
    pub path: &'project Path,
    /// The project's `name`.
    pub name: &'project str,
}

/// The names more than one project declares, compared case-insensitively.
/// A project whose name equals that of a project in an ancestor directory
/// inherits it and is part of that project. Sorted by name.
#[must_use]
pub fn duplicate_project_names<'project>(
    projects: impl IntoIterator<Item = NamedProject<'project>>,
) -> Vec<DuplicateProjectName> {
    let projects: Vec<NamedProject<'project>> = projects.into_iter().collect();
    let declared = |path: &Path, name: &str| {
        projects
            .iter()
            .any(|project| project.path == path && project.name == name)
    };
    let mut groups: BTreeMap<String, Vec<NamedProject<'project>>> = BTreeMap::new();
    for project in &projects {
        let inherited = ancestors(project.path).any(|ancestor| declared(ancestor, project.name));
        if !inherited {
            groups
                .entry(project.name.to_lowercase())
                .or_default()
                .push(*project);
        }
    }
    groups
        .into_values()
        .filter(|group| group.len() > 1)
        .map(|mut group| {
            group.sort_by(|left, right| left.path.cmp(right.path));
            DuplicateProjectName {
                name: group[0].name.to_string(),
                paths: group
                    .iter()
                    .map(|project| project.path.to_path_buf())
                    .collect(),
            }
        })
        .collect()
}

/// The directories above `path`, nearest first, ending with the module root
/// `.`; none for the root itself.
fn ancestors(path: &Path) -> impl Iterator<Item = &Path> {
    const ROOT: &str = ".";
    let top = (path != Path::new(ROOT) && !path.as_os_str().is_empty()).then(|| Path::new(ROOT));
    path.ancestors()
        .skip(1)
        .filter(|ancestor| !ancestor.as_os_str().is_empty() && *ancestor != Path::new(ROOT))
        .chain(top)
}

/// The error for `duplicates`, naming every project involved.
#[must_use]
pub fn duplicate_names_error(duplicates: &[DuplicateProjectName]) -> Error {
    let listed: Vec<String> = duplicates
        .iter()
        .map(|duplicate| {
            let paths: Vec<String> = duplicate
                .paths
                .iter()
                .map(|path| format!("'{}'", path.display()))
                .collect();
            format!("'{}' is declared by {}", duplicate.name, paths.join(", "))
        })
        .collect();
    Error::configuration(format!(
        "project names must be unique in a package (compared without regard to case): {}. \
         Rename all but one of them; tasks, CI pipelines and infrastructure state address a \
         project by its name",
        listed.join("; ")
    ))
}

impl ModuleEvaluation {
    /// Refuse a module in which two projects share a name (compared
    /// case-insensitively).
    ///
    /// # Errors
    ///
    /// Returns a configuration error naming every duplicate.
    pub fn require_unique_project_names(&self) -> crate::Result<()> {
        let duplicates = duplicate_project_names(self.projects().filter_map(|instance| {
            instance.project_name().map(|name| NamedProject {
                path: instance.path.as_path(),
                name,
            })
        }));
        if duplicates.is_empty() {
            Ok(())
        } else {
            Err(duplicate_names_error(&duplicates))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(path: &'static str, name: &'static str) -> NamedProject<'static> {
        NamedProject {
            path: Path::new(path),
            name,
        }
    }

    #[test]
    fn distinct_names_pass() {
        assert!(duplicate_project_names([named("api", "api"), named("web", "web")]).is_empty());
    }

    #[test]
    fn a_reused_name_is_reported_with_every_path() {
        let duplicates =
            duplicate_project_names([named("b", "web"), named("a", "web"), named("c", "api")]);
        assert_eq!(
            duplicates,
            [DuplicateProjectName {
                name: "web".into(),
                paths: vec![PathBuf::from("a"), PathBuf::from("b")],
            }]
        );
    }

    #[test]
    fn names_differing_only_by_case_collide() {
        let duplicates = duplicate_project_names([named("a", "Web"), named("b", "web")]);
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].name, "Web");
    }

    #[test]
    fn an_inherited_name_is_the_same_project() {
        assert!(
            duplicate_project_names([
                named(".", "root"),
                named("services", "root"),
                named("services/web/assets", "root"),
            ])
            .is_empty()
        );
        assert!(
            duplicate_project_names([named("services/web", "web"), named("services/web/x", "web")])
                .is_empty()
        );
        // A sibling is not an ancestor.
        assert_eq!(
            duplicate_project_names([
                named("services/web", "web"),
                named("services/webapp", "web")
            ])
            .len(),
            1
        );
    }

    #[test]
    fn the_error_names_projects_and_paths() {
        let error = duplicate_names_error(&duplicate_project_names([
            named("a", "web"),
            named("b", "WEB"),
        ]))
        .to_string();
        assert!(error.contains("'web' is declared by 'a', 'b'"), "{error}");
        assert!(error.contains("must be unique"), "{error}");
    }
}
