//! The packages a project imports, read from its CUE instance, which is
//! where the project name of a file reference is bound.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cuenv_core::module::{ImportRecord, ImportedProjects};

/// The field a project's name is read from.
const NAME_FIELD: &str = "name";

/// The CUE package `cuenv ci` evaluates projects from.
pub const PROJECT_PACKAGE: &str = "cuenv";

/// The project whose file reference is being bound.
#[derive(Debug, Clone, Copy)]
pub struct FilesOwner<'owner> {
    /// The root of the project's CUE module.
    pub module_root: &'owner Path,
    /// The project's directory.
    pub directory: &'owner Path,
    /// The CUE package of the project.
    pub package: &'owner str,
}

/// What the projects of one run import, by directory, each read once.
#[derive(Debug, Default)]
pub struct ImportsByDirectory {
    known: Mutex<HashMap<PathBuf, Result<ImportedProjects, String>>>,
}

impl ImportsByDirectory {
    /// The declaring directory, relative to the module root, of the project
    /// called `name` among what `owner` imports.
    ///
    /// # Errors
    ///
    /// Returns why the name binds to no directory (see
    /// [`ImportedProjects::bind`]), or why `owner`'s imports cannot be
    /// listed.
    pub fn bind(&self, owner: FilesOwner<'_>, name: &str) -> Result<PathBuf, String> {
        let mut known = self
            .known
            .lock()
            .map_err(|_| "the imports of this run are unavailable".to_string())?;
        let imported = known
            .entry(owner.directory.to_path_buf())
            .or_insert_with(|| {
                imported_projects(owner.module_root, owner.directory, owner.package)
            });
        imported.as_ref().map_err(Clone::clone)?.bind(name)
    }
}

/// The packages the instance of `package` in `directory` imports, each with
/// its `name` and the names of the same package's instances above it.
///
/// Nothing is evaluated on its own: the bridge loads the instance and builds
/// each imported package to look `name` up, so a package whose other fields
/// are in error still has a name, and a directory the project does not
/// import is never read.
///
/// # Errors
///
/// Returns why the instance's imports cannot be listed.
pub fn imported_projects(
    module_root: &Path,
    directory: &Path,
    package: &str,
) -> Result<ImportedProjects, String> {
    let fields = [NAME_FIELD.to_string()];
    let sources = cuengine::instance_sources_with(
        module_root,
        directory,
        cuengine::SourcesRequest {
            package_name: package,
            fields: &fields,
        },
    )
    .map_err(|error| {
        format!(
            "the packages that `{}` imports cannot be listed: {error}",
            directory.display()
        )
    })?;
    Ok(ImportedProjects {
        module: sources.module,
        package: package.to_string(),
        records: sources.imports.into_iter().map(record).collect(),
    })
}

fn record(imported: cuengine::ImportedInstance) -> ImportRecord {
    let name = |fields: &std::collections::BTreeMap<String, serde_json::Value>| {
        fields
            .get(NAME_FIELD)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    ImportRecord {
        name: name(&imported.fields),
        ancestors: imported
            .ancestors
            .iter()
            .map(|ancestor| (ancestor.directory.clone(), name(&ancestor.fields)))
            .collect(),
        import_path: imported.import_path,
        imported_by: imported.imported_by,
        directory: imported.directory,
        package: imported.package,
        module: imported.module,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        fs::write(path, content).expect("a file");
    }

    #[test]
    fn a_project_name_binds_through_what_the_project_imports() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        write(
            root,
            "cue.mod/module.cue",
            "module: \"example.com/site@v0\"\nlanguage: version: \"v0.14.1\"\n",
        );
        write(
            root,
            "content/env.cue",
            "package cuenv\n\nname: \"content\"\n",
        );
        write(
            root,
            "content/drafts/env.cue",
            "package cuenv\n\ndrafts: true\n",
        );
        write(root, "docs/env.cue", "package cuenv\n\nname: \"docs\"\n");
        write(
            root,
            "app/env.cue",
            "package cuenv\n\nimport (\n\tcontent \"example.com/site/content:cuenv\"\n\tdrafts \
             \"example.com/site/content/drafts:cuenv\"\n)\n\nname: \"app\"\nuses: [content.name, \
             drafts.drafts]\n",
        );
        write(
            root,
            "blog/env.cue",
            "package cuenv\n\nimport content \"example.com/site/content:cuenv\"\n\nname: \
             \"blog\"\nuses: content.name\n",
        );

        let blog = imported_projects(root, &root.join("blog"), "cuenv").expect("blog's imports");
        assert_eq!(blog.module, "example.com/site@v0");
        assert_eq!(
            blog.bind("content"),
            Ok(std::path::PathBuf::from("content"))
        );
        let error = blog.bind("docs").expect_err("docs is not imported by blog");
        assert!(error.contains("not imported"), "{error}");

        let app = imported_projects(root, &root.join("app"), "cuenv").expect("app's imports");
        let error = app
            .bind("content")
            .expect_err("content/drafts only inherits");
        assert!(error.contains("only inherits project 'content'"), "{error}");
    }
}
