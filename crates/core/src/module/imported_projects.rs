//! Which directory a project name means to the project that imports it.
//!
//! A file reference (`content.#Files & {paths: [...]}`) carries the name of
//! the project whose files it reads, stamped by that project's `#Files`. It
//! can only have been written through an import of that project, so the
//! name is looked up among the packages the consuming project imports,
//! never among every project of the module: a hand-written reference to a
//! project that is not imported does not bind, and a same-named project the
//! consumer does not import is never bound in its place.
//!
//! The name means its declaring directory: the shallowest directory, among
//! the imported package's own and the directories above it holding files of
//! the same package, whose instance carries that `name`. A directory below a
//! project inherits its `name` and is part of that project, never a second
//! anchor.

use std::path::{Path, PathBuf};

/// One package a project imports, as the binding of names reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportRecord {
    /// The import path, as written.
    pub import_path: String,
    /// The import path of the package that imports it; empty when the
    /// consuming project does.
    pub imported_by: String,
    /// The package's directory, relative to the module root (`.` or empty
    /// for the root) when it lies inside the module, absolute otherwise.
    pub directory: String,
    /// The package name.
    pub package: String,
    /// The path of the module the package belongs to.
    pub module: String,
    /// The package's concrete `name`, if it has one.
    pub name: Option<String>,
    /// The `name` of the instance of the same package in each directory
    /// above it that holds files of that package, nearest first.
    pub ancestors: Vec<(String, Option<String>)>,
}

/// The packages one project imports, transitively.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedProjects {
    /// The module path of the importing project.
    pub module: String,
    /// The CUE package cuenv evaluates projects from.
    pub package: String,
    /// What the project imports.
    pub records: Vec<ImportRecord>,
}

impl ImportedProjects {
    /// The declaring directory of the project called `name`, relative to the
    /// module root (empty for the root), among what this project imports.
    ///
    /// # Errors
    ///
    /// Returns what to do when no imported package carries the name, when a
    /// package that carries it is in another package or another module, or
    /// only inherits the name from a directory above it, and when two
    /// directories declare it.
    pub fn bind(&self, name: &str) -> Result<PathBuf, String> {
        let carriers: Vec<&ImportRecord> = self
            .records
            .iter()
            .filter(|record| record.name.as_deref() == Some(name))
            .collect();
        if carriers.is_empty() {
            return Err(format!(
                "project '{name}' is not imported here: import the project that holds the files \
                 by its own directory, `{name} \"{}/<directory of {name}>:{}\"`, and write \
                 `{name}.#Files & {{paths: [...]}}`",
                import_prefix(&self.module),
                self.package
            ));
        }
        let mut declaring: Vec<PathBuf> = Vec::new();
        for carrier in carriers {
            let directory = self.declaring_directory(name, carrier)?;
            if !declaring.contains(&directory) {
                declaring.push(directory);
            }
        }
        declaring.sort();
        match declaring.as_slice() {
            [directory] => Ok(directory.clone()),
            _ => Err(format!(
                "project name '{name}' is declared by {}: project names are unique in a module",
                declaring
                    .iter()
                    .map(|directory| format!("`{}`", display(directory)))
                    .collect::<Vec<_>>()
                    .join(" and ")
            )),
        }
    }

    /// The directory a package carrying `name` binds it to, or why it binds
    /// nothing.
    fn declaring_directory(&self, name: &str, carrier: &ImportRecord) -> Result<PathBuf, String> {
        if carrier.module != self.module {
            return Err(format!(
                "project '{name}' is imported from module `{}` (`{}`): file references name files \
                 of the same CUE module only",
                carrier.module, carrier.import_path
            ));
        }
        let directory = module_relative(&carrier.directory);
        if carrier.package != self.package {
            return Err(format!(
                "`{}` declares package `{}`; cuenv projects use package `{}`",
                display(&directory),
                carrier.package,
                self.package
            ));
        }
        // The ancestors are nearest first: the last that carries the name
        // is the shallowest.
        let shallowest = carrier
            .ancestors
            .iter()
            .filter(|(_, ancestor)| ancestor.as_deref() == Some(name))
            .map(|(ancestor, _)| module_relative(ancestor))
            .next_back();
        match shallowest {
            None => Ok(directory),
            Some(declaring) => {
                let importer = if carrier.imported_by.is_empty() {
                    "this project".to_string()
                } else {
                    format!("`{}`", carrier.imported_by)
                };
                Err(format!(
                    "`{}` (imported by {importer} as `{}`) only inherits project '{name}' from \
                     `{}`: import the project's own directory, `{name} \"{}/{}:{}\"`",
                    display(&directory),
                    carrier.import_path,
                    display(&declaring),
                    import_prefix(&self.module),
                    declaring.to_string_lossy().replace('\\', "/"),
                    self.package
                ))
            }
        }
    }
}

/// A module path as an import path starts: without its major version.
fn import_prefix(module: &str) -> &str {
    module.split_once('@').map_or(module, |(path, _)| path)
}

/// A directory as records spell it, with the module root as the empty path.
fn module_relative(directory: &str) -> PathBuf {
    if directory == "." {
        PathBuf::new()
    } else {
        PathBuf::from(directory)
    }
}

fn display(directory: &Path) -> String {
    if directory.as_os_str().is_empty() {
        ".".to_string()
    } else {
        directory.to_string_lossy().replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE: &str = "github.com/acme/site@v0";

    fn record(import_path: &str, directory: &str, name: Option<&str>) -> ImportRecord {
        ImportRecord {
            import_path: import_path.to_string(),
            directory: directory.to_string(),
            package: "cuenv".to_string(),
            module: MODULE.to_string(),
            name: name.map(str::to_string),
            ..ImportRecord::default()
        }
    }

    fn imports(records: Vec<ImportRecord>) -> ImportedProjects {
        ImportedProjects {
            module: MODULE.to_string(),
            package: "cuenv".to_string(),
            records,
        }
    }

    fn content() -> ImportRecord {
        ImportRecord {
            ancestors: vec![(".".to_string(), None)],
            ..record(
                "github.com/acme/site/content:cuenv",
                "content",
                Some("content"),
            )
        }
    }

    #[test]
    fn a_name_binds_to_the_imported_project_that_declares_it() {
        let schema = ImportRecord {
            module: "github.com/cuenv/cuenv@v0".to_string(),
            package: "schema".to_string(),
            ..record("github.com/cuenv/cuenv/schema", "/cache/schema", None)
        };
        let imported = imports(vec![schema, content()]);

        assert_eq!(imported.bind("content"), Ok(PathBuf::from("content")));
    }

    #[test]
    fn a_root_project_binds_to_the_module_root() {
        let root = record("github.com/acme/site:cuenv", ".", Some("site"));

        assert_eq!(imports(vec![root]).bind("site"), Ok(PathBuf::new()));
    }

    #[test]
    fn a_project_that_is_not_imported_does_not_bind() {
        let error = imports(vec![content()])
            .bind("docs")
            .expect_err("docs is not imported");

        assert!(
            error.contains("project 'docs' is not imported here"),
            "{error}"
        );
        assert!(
            error.contains("`docs \"github.com/acme/site/<directory of docs>:cuenv\"`"),
            "{error}"
        );
    }

    #[test]
    fn a_child_directory_that_inherits_the_name_is_not_the_project() {
        let drafts = ImportRecord {
            imported_by: "github.com/acme/site/shared".to_string(),
            ancestors: vec![
                ("content".to_string(), Some("content".to_string())),
                (".".to_string(), None),
            ],
            ..record(
                "github.com/acme/site/content/drafts:cuenv",
                "content/drafts",
                Some("content"),
            )
        };

        for records in [vec![drafts.clone()], vec![content(), drafts]] {
            let error = imports(records)
                .bind("content")
                .expect_err("a child directory");
            assert!(
                error.contains("`content/drafts` (imported by `github.com/acme/site/shared`"),
                "{error}"
            );
            assert!(
                error.contains("only inherits project 'content' from `content`"),
                "{error}"
            );
            assert!(
                error.contains("`content \"github.com/acme/site/content:cuenv\"`"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_project_of_another_package_or_module_does_not_bind() {
        let other_package = ImportRecord {
            package: "content".to_string(),
            ..content()
        };
        let error = imports(vec![other_package])
            .bind("content")
            .expect_err("another package");
        assert!(
            error.contains(
                "`content` declares package `content`; cuenv projects use package `cuenv`"
            ),
            "{error}"
        );

        let vendored = ImportRecord {
            module: "github.com/vendor/content@v0".to_string(),
            ..record(
                "github.com/vendor/content:cuenv",
                "vendor/content",
                Some("content"),
            )
        };
        // Never bound in its place: a same-named local project is not looked
        // for.
        let error = imports(vec![vendored])
            .bind("content")
            .expect_err("another module");
        assert!(
            error.contains("imported from module `github.com/vendor/content@v0`"),
            "{error}"
        );
        assert!(error.contains("same CUE module only"), "{error}");
    }

    #[test]
    fn two_directories_declaring_one_name_do_not_bind() {
        let elsewhere = record("github.com/acme/site/blog:cuenv", "blog", Some("content"));

        let error = imports(vec![content(), elsewhere])
            .bind("content")
            .expect_err("a duplicate name");

        assert!(
            error.contains("project name 'content' is declared by `blog` and `content`"),
            "{error}"
        );
    }

    #[test]
    fn a_package_imported_twice_binds_once() {
        let again = ImportRecord {
            import_path: "github.com/acme/site/content".to_string(),
            ..content()
        };

        assert_eq!(
            imports(vec![content(), again]).bind("content"),
            Ok(PathBuf::from("content"))
        );
    }
}
