use super::*;
use crate::TaskDependency;
use serde_json::json;
use std::cell::Cell;
use std::fs;
use tempfile::TempDir;

/// A module on disk whose projects are held in memory: `<root>/<directory>`
/// for each, as a [`ProjectSource`] that counts what it is asked.
struct Module {
    _directory: TempDir,
    root: PathBuf,
    projects: BTreeMap<String, Project>,
    output_ref_deps: BTreeMap<String, Vec<OutputRefDep>>,
    /// How often a project name was bound.
    lookups: Cell<usize>,
    evaluations: Cell<usize>,
    /// Names that cannot bind, and why.
    unbindable: BTreeMap<String, String>,
}

impl Module {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let root = directory.path().canonicalize().unwrap();
        Self {
            _directory: directory,
            root,
            projects: BTreeMap::new(),
            output_ref_deps: BTreeMap::new(),
            lookups: Cell::new(0),
            evaluations: Cell::new(0),
            unbindable: BTreeMap::new(),
        }
    }

    /// Add the project `value` describes in `directory`, expanded as module
    /// evaluation expands it.
    fn project(mut self, directory: &str, value: serde_json::Value) -> Self {
        fs::create_dir_all(self.root.join(directory)).unwrap();
        let mut project: Project = serde_json::from_value(value).unwrap();
        project.expand_cross_project_references(Path::new(directory));
        self.projects.insert(directory.to_string(), project);
        self
    }

    fn directory(&self, directory: &str) -> PathBuf {
        self.root.join(directory)
    }

    /// The scope of a run in the project in `directory`.
    fn scope(&self, directory: &str) -> TaskScope {
        let project = &self.projects[directory];
        TaskScope::new(RootProject {
            project: ScopeProject {
                root: self.directory(directory),
                module_root: self.root.clone(),
                manifest: Arc::new(project.clone()),
            },
            tasks: TaskIndex::build(&project.tasks).unwrap().to_tasks(),
            output_ref_deps: self
                .output_ref_deps
                .get(directory)
                .cloned()
                .unwrap_or_default(),
        })
    }
}

impl ProjectSource for Module {
    fn imported_project(&self, origin: &Path, name: &str) -> std::result::Result<PathBuf, String> {
        // Every project imports every other here, so the name binds to the
        // first directory that carries it, unless the origin "imports" it
        // from a child directory that only inherits the name.
        self.lookups.set(self.lookups.get() + 1);
        if let Some(problem) = self.unbindable.get(name) {
            return Err(problem.clone());
        }
        self.projects
            .iter()
            .find(|(directory, project)| {
                project.name == name && self.directory(directory).as_path() != origin
            })
            .map(|(directory, _)| self.directory(directory))
            .ok_or_else(|| format!("project '{name}' is not imported here"))
    }

    fn workspace_root(&self, _origin: &Path) -> PathBuf {
        self.root.clone()
    }

    fn project_at(&self, root: &Path) -> Result<SourcedProject> {
        self.evaluations.set(self.evaluations.get() + 1);
        let directory = root
            .strip_prefix(&self.root)
            .map(forward_slashes)
            .unwrap_or_default();
        let manifest = self
            .projects
            .get(&directory)
            .ok_or_else(|| Error::configuration(format!("no project in '{}'", root.display())))?;
        Ok(SourcedProject {
            root: root.to_path_buf(),
            module_root: self.root.clone(),
            manifest: manifest.clone(),
            output_ref_deps: self
                .output_ref_deps
                .get(&directory)
                .cloned()
                .unwrap_or_default(),
        })
    }
}

/// `app`, whose `build` reads `content.tasks.articles.outputFiles` (as
/// loading names it, `{task: "#content:articles"}`).
fn app_and_content() -> Module {
    Module::new()
        .project(
            "app",
            json!({
                "name": "app",
                "tasks": {
                    "build": {
                        "command": "make",
                        "inputs": ["src/**", {"task": "#content:articles"}],
                    },
                    "lint": {"command": "lint"},
                },
            }),
        )
        .project(
            "content",
            json!({
                "name": "content",
                "tasks": {
                    "articles": {
                        "command": "render",
                        "outputs": ["out/one.md"],
                        "labels": ["publish"],
                    },
                },
            }),
        )
}

fn task<'scope>(scope: &'scope TaskScope, name: &str) -> &'scope Task {
    scope
        .node(name)
        .and_then(TaskNode::as_task)
        .unwrap_or_else(|| panic!("no task '{name}' in the scope"))
}

fn dependencies(task: &Task) -> Vec<&str> {
    task.depends_on
        .iter()
        .map(TaskDependency::task_name)
        .collect()
}

fn order(graph: &TaskGraph) -> Vec<String> {
    graph
        .topological_sort()
        .unwrap()
        .into_iter()
        .map(|node| node.name)
        .collect()
}

#[test]
fn a_qualified_name_is_read_as_project_and_task() {
    assert_eq!(
        ExternalTaskName::parse("#content:docs.build"),
        Some(ExternalTaskName {
            project: "content",
            task: "docs.build"
        })
    );
    assert_eq!(ExternalTaskName::parse("docs.build"), None);
    assert_eq!(ExternalTaskName::parse("#content"), None);
    assert_eq!(ExternalTaskName::parse("#:build"), None);
    assert_eq!(
        ExternalTaskName {
            project: "content",
            task: "articles[0]"
        }
        .qualified(),
        "#content:articles[0]"
    );
}

#[test]
fn a_dependency_on_another_project_runs_first_and_cannot_be_selected() {
    let module = app_and_content();
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    // What a run selects from is the project's own tasks, by name or label.
    let mut selectable: Vec<&str> = scope.local().list_tasks();
    selectable.sort_unstable();
    assert_eq!(selectable, ["build", "lint"]);
    assert!(
        scope
            .local()
            .tasks
            .values()
            .filter_map(TaskNode::as_task)
            .all(|task| !task.labels.contains(&"publish".to_string()))
    );

    let graph = scope.graph(["build"]).unwrap();
    assert_eq!(order(&graph), ["#content:articles", "build"]);
}

#[test]
fn output_files_of_another_project_are_staged_where_they_are_written() {
    let module = app_and_content();
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    let build = task(&scope, "build");
    assert_eq!(
        build.inputs,
        vec![
            Input::Path("src/**".to_string()),
            Input::Mapped(MappedInput {
                source: "content/out/one.md".to_string(),
                destination: "content/out/one.md".to_string(),
                producer_task: Some("#content:articles".to_string()),
                anchor: None,
            }),
        ]
    );
    assert_eq!(dependencies(build), ["#content:articles"]);
}

#[test]
fn a_map_on_the_outputs_of_another_project_fails_the_reader() {
    let module = Module::new()
        .project(
            "app",
            json!({
                "name": "app",
                "tasks": {"build": {"command": "make", "inputs": [
                    {"task": "#content:articles", "map": [{"from": "out/one.md", "to": "vendor/one.md"}]},
                ]}},
            }),
        )
        .project(
            "content",
            json!({"name": "content", "tasks": {"articles": {"command": "render", "outputs": ["out/one.md"]}}}),
        );
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    let error = scope.graph(["build"]).err().unwrap().to_string();
    assert!(
        error.contains("`map` applies to tasks of this project only"),
        "{error}"
    );
}

#[test]
fn output_files_of_a_task_that_declares_no_outputs_fail_the_reader() {
    let module = Module::new()
        .project(
            "app",
            json!({
                "name": "app",
                "tasks": {"build": {"command": "make", "inputs": [{"task": "#content:articles"}]}},
            }),
        )
        .project(
            "content",
            json!({"name": "content", "tasks": {"articles": {"command": "render"}}}),
        );
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    let error = scope.graph(["build"]).err().unwrap().to_string();
    assert!(
        error.contains(
            "reads the `outputFiles` of '#content:articles', which declares no `outputs`"
        ),
        "{error}"
    );
}

#[test]
fn a_task_of_another_project_belongs_to_that_project() {
    let module = app_and_content();
    let mut scope = module.scope("app");
    scope.reach(["build"], &module);

    let articles = scope.owner("#content:articles").unwrap();
    assert_eq!(articles.key, Some("content"));
    assert_eq!(articles.task, "articles");
    assert_eq!(articles.project.root, module.directory("content"));
    assert_eq!(articles.project.manifest.name, "content");
    assert_eq!(
        task(&scope, "#content:articles").project_root,
        Some(module.directory("content"))
    );

    let build = scope.owner("build").unwrap();
    assert_eq!(build.key, None);
    assert_eq!(build.project.manifest.name, "app");
    assert!(scope.owner("#docs:build").is_none());

    let external: Vec<&str> = scope.external_projects().map(|(key, _)| key).collect();
    assert_eq!(external, ["content"]);
}

#[test]
fn a_project_reached_from_two_projects_is_one_node() {
    // Reached from `docs` and through `app`: one project, loaded once.
    let module = app_and_content().project(
        "docs",
        json!({
            "name": "docs",
            "tasks": {
                "site": {
                    "command": "site",
                    "dependsOn": ["#content:articles", "#app:build"],
                },
            },
        }),
    );
    let mut scope = module.scope("docs");

    scope.reach(["site"], &module);

    assert_eq!(
        dependencies(task(&scope, "#app:build")),
        ["#content:articles"]
    );
    let graph = scope.graph(["site"]).unwrap();
    assert_eq!(order(&graph), ["#content:articles", "#app:build", "site"]);
    assert_eq!(module.evaluations.get(), 2);
}

#[test]
fn nothing_is_loaded_for_a_task_that_reaches_no_other_project() {
    let module = app_and_content();
    let mut scope = module.scope("app");

    scope.reach(["lint"], &module);

    assert_eq!(module.lookups.get(), 0);
    assert_eq!(module.evaluations.get(), 0);
    assert_eq!(order(&scope.graph(["lint"]).unwrap()), ["lint"]);
}

#[test]
fn a_project_that_cannot_be_loaded_fails_the_task_that_needs_it_and_says_why() {
    let mut module = app_and_content();
    module.unbindable.insert(
        "content".to_string(),
        "project 'content' is not imported here".to_string(),
    );
    let mut scope = module.scope("app");

    scope.reach(["build", "lint"], &module);

    assert!(scope.graph(["lint"]).is_ok());
    let error = scope.graph(["build"]).err().unwrap().to_string();
    assert!(error.contains("#content:articles"), "{error}");
    assert!(
        error.contains("Project 'content' could not be loaded"),
        "{error}"
    );
    assert!(error.contains("is not imported here"), "{error}");
    assert_eq!(module.evaluations.get(), 0, "nothing else is looked for");
    // The input is left as it was written, never dropped.
    assert_eq!(
        task(&scope, "build").inputs[1].task_output_name(),
        Some("#content:articles")
    );
}

#[test]
fn names_inside_another_project_never_mean_a_task_of_this_one() {
    // Both projects have a task called `fetch`. Everything `content` says
    // about its own must stay about its own.
    let mut module = Module::new()
        .project(
            "app",
            json!({
                "name": "app",
                "tasks": {
                    "fetch": {"command": "fetch-app"},
                    "build": {
                        "command": "make",
                        "dependsOn": ["#content:articles", "#content:all", "#content:steps"],
                    },
                },
            }),
        )
        .project(
            "content",
            json!({
                "name": "content",
                "tasks": {
                    "fetch": {"command": "fetch-content", "outputs": ["raw/**"]},
                    "articles": {
                        "command": "render",
                        "args": ["--from", "cuenv:ref:fetch:stdout"],
                        "env": {"RAW": "cuenv:ref:fetch:stderr", "PLAIN": "kept"},
                        "inputs": [{"task": "fetch"}],
                    },
                    "all": {
                        "type": "group",
                        "dependsOn": ["fetch"],
                        "one": {"command": "one", "dependsOn": ["articles"]},
                    },
                    "steps": [
                        {"command": "first", "dependsOn": ["fetch"]},
                        {"command": "second"},
                    ],
                },
            }),
        );
    module.output_ref_deps.insert(
        "content".to_string(),
        vec![OutputRefDep::new("articles", "fetch")],
    );
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    let articles = task(&scope, "#content:articles");
    assert_eq!(dependencies(articles), ["#content:fetch"]);
    assert_eq!(articles.args, ["--from", "cuenv:ref:#content:fetch:stdout"]);
    assert_eq!(
        articles.env["RAW"],
        json!("cuenv:ref:#content:fetch:stderr")
    );
    assert_eq!(articles.env["PLAIN"], json!("kept"));
    assert_eq!(
        articles.inputs,
        vec![Input::Mapped(MappedInput {
            source: "content/raw/**".to_string(),
            destination: "content/raw".to_string(),
            producer_task: Some("#content:fetch".to_string()),
            anchor: None,
        })]
    );
    assert_eq!(
        scope.output_ref_deps(),
        [OutputRefDep::new("#content:articles", "#content:fetch")]
    );

    assert_eq!(task(&scope, "#content:fetch").command, "fetch-content");
    let graph = scope.graph(["build"]).unwrap();
    let order = order(&graph);
    assert!(!order.contains(&"fetch".to_string()), "{order:?}");
    assert_eq!(order.first().map(String::as_str), Some("#content:fetch"));
    assert_eq!(order.last().map(String::as_str), Some("build"));

    // Groups and sequences keep their shape under the qualified name.
    assert_eq!(
        dependencies(task(&scope, "#content:all.one")),
        ["#content:articles"]
    );
    assert_eq!(
        dependencies(task(&scope, "#content:steps[0]")),
        ["#content:fetch"]
    );
    let position = |name: &str| {
        order
            .iter()
            .position(|node| node == name)
            .unwrap_or_else(|| panic!("'{name}' is not in {order:?}"))
    };
    assert!(position("#content:articles") < position("#content:all.one"));
    assert!(position("#content:steps[0]") < position("#content:steps[1]"));
}

#[test]
fn a_task_the_walk_never_reached_cannot_be_put_in_a_graph() {
    // Its dependencies are still as its project wrote them, and would be
    // read as tasks of the wrong project.
    let module = app_and_content();
    let mut scope = module.scope("app");
    scope.reach(["build"], &module);

    let error = scope.graph(["lint"]).err().unwrap().to_string();

    assert!(error.contains("outside the scope of this run"), "{error}");
}

/// `app` with a group and a sequence whose members read the `outputFiles`
/// of a task of `content`.
fn nested_consumers() -> Module {
    let consumer = || {
        json!({
            "command": "make",
            "inputs": [{"task": "#content:articles"}],
        })
    };
    Module::new()
        .project(
            "app",
            json!({
                "name": "app",
                "tasks": {
                    "checks": {"type": "group", "unit": consumer()},
                    "release": [consumer(), {"command": "publish"}],
                },
            }),
        )
        .project(
            "content",
            json!({"name": "content", "tasks": {"articles": {"command": "render", "outputs": ["out/one.md"]}}}),
        )
}

fn mapped_from_content(task: &Task) -> bool {
    task.inputs
        == [Input::Mapped(MappedInput {
            source: "content/out/one.md".to_string(),
            destination: "content/out/one.md".to_string(),
            producer_task: Some("#content:articles".to_string()),
            anchor: None,
        })]
        && dependencies(task) == ["#content:articles"]
}

#[test]
fn every_copy_of_a_node_is_resolved_once_whichever_name_reaches_it_first() {
    // A group's child is indexed under its own name too, as a copy. A run
    // of the group executes the child inside the group; a run of the child
    // executes the copy. Both are resolved, and neither twice.
    for first in [["checks", "checks.unit"], ["checks.unit", "checks"]] {
        let module = nested_consumers();
        let mut scope = module.scope("app");

        scope.reach([first[0]], &module);
        scope.reach([first[1]], &module);

        assert!(
            mapped_from_content(task(&scope, "checks.unit")),
            "{first:?}"
        );
        let inside = scope
            .local()
            .get("checks")
            .and_then(TaskNode::as_group)
            .and_then(|group| group.children.get("unit"))
            .and_then(TaskNode::as_task)
            .unwrap();
        assert!(mapped_from_content(inside), "{first:?}");
        assert_eq!(module.evaluations.get(), 1, "{first:?}");
    }

    // A step of a sequence is only ever the step inside the sequence.
    for first in [["release", "release[0]"], ["release[0]", "release"]] {
        let module = nested_consumers();
        let mut scope = module.scope("app");

        scope.reach([first[0]], &module);
        scope.reach([first[1]], &module);

        assert!(mapped_from_content(task(&scope, "release[0]")), "{first:?}");
        assert_eq!(
            order(&scope.graph(["release"]).unwrap()),
            ["#content:articles", "release[0]", "release[1]"],
            "{first:?}"
        );
    }
}

#[test]
fn a_dependency_chain_across_three_projects_is_followed() {
    let module = app_and_content().project(
        "sources",
        json!({
            "name": "sources",
            "tasks": {"pull": {"command": "pull", "outputs": ["raw.json"]}},
        }),
    );
    let mut module = module;
    if let Some(TaskNode::Task(articles)) = module
        .projects
        .get_mut("content")
        .and_then(|content| content.tasks.get_mut("articles"))
    {
        articles.inputs = vec![Input::Task(crate::TaskOutput {
            task: "#sources:pull".to_string(),
            map: None,
        })];
        articles
            .depends_on
            .push(TaskDependency::from_name("#sources:pull"));
    }
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    assert_eq!(
        order(&scope.graph(["build"]).unwrap()),
        ["#sources:pull", "#content:articles", "build"]
    );
    // The middle project reads the files of the last where they are built.
    assert_eq!(
        task(&scope, "#content:articles").inputs,
        vec![Input::Mapped(MappedInput {
            source: "sources/raw.json".to_string(),
            destination: "sources/raw.json".to_string(),
            producer_task: Some("#sources:pull".to_string()),
            anchor: None,
        })]
    );
}

#[test]
fn a_reference_back_to_the_runs_own_project_is_a_local_dependency() {
    let module = Module::new()
        .project(
            "app",
            json!({
                "name": "app",
                "tasks": {
                    "schema": {"command": "schema"},
                    "build": {"command": "make", "dependsOn": ["#content:articles"]},
                },
            }),
        )
        .project(
            "content",
            json!({
                "name": "content",
                "tasks": {
                    "articles": {"command": "render", "dependsOn": ["#app:schema"]},
                },
            }),
        );
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    assert_eq!(
        order(&scope.graph(["build"]).unwrap()),
        ["schema", "#content:articles", "build"]
    );
}

/// Two members of one bun workspace, each with the contributor's steps
/// injected, as `cuenv task` injects them.
fn workspace_members() -> Module {
    let member = |name: &str, dependency: Option<&str>| {
        let mut build = json!({
            "command": "bun",
            "args": ["run", "build"],
            "dependsOn": ["cuenv:contributor:bun.workspace.setup"],
        });
        if let Some(dependency) = dependency {
            build["dependsOn"]
                .as_array_mut()
                .unwrap()
                .push(json!(dependency));
        }
        json!({
            "name": name,
            "tasks": {
                "build": build,
                "cuenv:contributor:bun.workspace.install": {
                    "command": "bun",
                    "args": ["install", "--frozen-lockfile"],
                    "hermetic": false,
                },
                "cuenv:contributor:bun.workspace.setup": {
                    "script": "true",
                    "hermetic": false,
                    "dependsOn": ["cuenv:contributor:bun.workspace.install"],
                },
            },
        })
    };
    let module = Module::new()
        .project("app", member("app", Some("#content:build")))
        .project("content", member("content", None));
    fs::write(
        module.root.join("package.json"),
        r#"{"workspaces": ["app", "content"]}"#,
    )
    .unwrap();
    for directory in ["app", "content"] {
        fs::write(module.directory(directory).join("package.json"), "{}").unwrap();
        fs::write(module.directory(directory).join("bun.lock"), "").unwrap();
    }
    module
}

#[test]
fn two_members_of_a_workspace_share_its_install_step() {
    let module = workspace_members();
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    let graph = scope.graph(["build"]).unwrap();
    let installs: Vec<String> = order(&graph)
        .into_iter()
        .filter(|name| name.ends_with("bun.workspace.install"))
        .collect();
    assert_eq!(installs, ["cuenv.contributor.bun.workspace.install"]);
    assert_eq!(
        dependencies(task(&scope, "#content:build")),
        ["cuenv.contributor.bun.workspace.setup"]
    );
    assert!(
        scope
            .node("#content:cuenv.contributor.bun.workspace.install")
            .is_none()
    );
}

#[test]
fn members_of_different_workspaces_each_install() {
    let module = workspace_members();
    // Without a workspace above them, each project is its own.
    fs::write(module.root.join("package.json"), "{}").unwrap();
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    let graph = scope.graph(["build"]).unwrap();
    let mut installs: Vec<String> = order(&graph)
        .into_iter()
        .filter(|name| name.ends_with("bun.workspace.install"))
        .collect();
    installs.sort();
    assert_eq!(
        installs,
        [
            "#content:cuenv.contributor.bun.workspace.install",
            "cuenv.contributor.bun.workspace.install"
        ]
    );
}

#[test]
fn output_files_of_a_project_outside_the_module_fail_the_reader() {
    // `tools` is not below the module root of `app`: tasks of the same
    // module only can be read through `outputFiles`.
    let module = Module::new()
        .project(
            "services/app",
            json!({
                "name": "app",
                "tasks": {"build": {"command": "make", "inputs": [{"task": "#tools:generate"}]}},
            }),
        )
        .project(
            "tools",
            json!({"name": "tools", "tasks": {"generate": {"command": "generate", "outputs": ["out"]}}}),
        );
    let app = &module.projects["services/app"];
    let mut scope = TaskScope::new(RootProject {
        project: ScopeProject {
            root: module.directory("services/app"),
            module_root: module.directory("services"),
            manifest: Arc::new(app.clone()),
        },
        tasks: TaskIndex::build(&app.tasks).unwrap().to_tasks(),
        output_ref_deps: Vec::new(),
    });

    scope.reach(["build"], &module);

    let error = scope.graph(["build"]).err().unwrap().to_string();
    assert!(error.contains("outside this CUE module"), "{error}");
}

/// `app` reads `inputs` in its task `build`; `content` holds an article
/// and the module root a lockfile.
fn reading_files(inputs: serde_json::Value) -> Module {
    let module = Module::new()
        .project(
            "app",
            json!({"name": "app", "tasks": {
                "build": {"command": "make", "inputs": inputs},
                "lint": {"command": "make", "inputs": ["src/**"]},
            }}),
        )
        .project(
            "content",
            json!({"name": "content", "tasks": {"build": {"command": "make"}}}),
        );
    fs::create_dir_all(module.directory("content/articles")).unwrap();
    fs::write(module.directory("content/articles/one.md"), "one").unwrap();
    fs::write(module.directory("bun.lock"), "lock").unwrap();
    module
}

#[test]
fn a_file_reference_is_bound_to_its_anchor_and_loads_no_project() {
    let module = reading_files(json!([
        "src/**",
        {"cuenvFiles": "project", "cuenvProject": "content", "paths": ["articles/**"]},
        {"cuenvFiles": "module", "paths": ["bun.lock"]},
    ]));
    let mut scope = module.scope("app");

    scope.reach(["build"], &module);

    assert_eq!(order(&scope.graph(["build"]).unwrap()), ["build"]);
    assert_eq!(
        module.evaluations.get(),
        0,
        "no project is loaded for its files"
    );
    assert_eq!(
        module.lookups.get(),
        1,
        "the name is bound among the imports, once"
    );
    assert_eq!(
        task(&scope, "build").inputs,
        vec![
            Input::Path("src/**".to_string()),
            Input::Mapped(MappedInput {
                source: "content/articles/**".to_string(),
                destination: "content/articles".to_string(),
                producer_task: None,
                anchor: Some("content".to_string()),
            }),
            Input::Mapped(MappedInput {
                source: "bun.lock".to_string(),
                destination: "bun.lock".to_string(),
                producer_task: None,
                anchor: Some(String::new()),
            }),
        ]
    );
}

#[test]
fn a_file_reference_that_does_not_bind_fails_only_the_graphs_that_reach_it() {
    for (reference, expected) in [
        (
            json!({"cuenvFiles": "project", "cuenvProject": "missing", "paths": ["a/**"]}),
            "project 'missing' is not imported here",
        ),
        (
            json!({"cuenvFiles": "project", "cuenvProject": "content", "paths": ["posts/**"]}),
            "`posts/**` matches nothing in content/",
        ),
        (
            json!({"cuenvFiles": "module", "paths": ["../outside"]}),
            "is not a path inside the module root",
        ),
    ] {
        let module = reading_files(json!([reference]));
        let mut scope = module.scope("app");

        scope.reach(["build", "lint"], &module);

        let Err(error) = scope.graph(["build"]) else {
            panic!("an unbound reference");
        };
        assert!(error.to_string().contains("task 'build'"), "{error}");
        assert!(error.to_string().contains(expected), "{error}");
        assert!(scope.graph(["lint"]).is_ok(), "lint does not reach it");
    }
}

#[test]
fn a_file_reference_never_matches_into_the_cache_store() {
    // The store lies in the repository, as `<project>/.cuenv-cache` does
    // without a platform cache directory, and holds the only JSON file.
    let module = reading_files(json!([
        {"cuenvFiles": "project", "cuenvProject": "content", "paths": ["**/*.json"]},
    ]));
    let store = module.directory("content/.cuenv-cache");
    fs::create_dir_all(store.join("cas")).unwrap();
    fs::write(store.join("cas/blob.json"), "{}").unwrap();

    let mut plain = module.scope("app");
    plain.reach(["build"], &module);
    assert!(plain.graph(["build"]).is_ok());

    let mut scope = module.scope("app").excluding_cache_store(store);
    scope.reach(["build"], &module);
    let Err(error) = scope.graph(["build"]) else {
        panic!("a reference that matches only the store");
    };
    assert!(
        error
            .to_string()
            .contains("`**/*.json` matches nothing in content/"),
        "{error}"
    );
}

#[test]
fn a_problem_recorded_against_a_reached_node_fails_the_graph() {
    let mut module = reading_files(json!(["src/**"]));
    if let Some(app) = module.projects.get_mut("app") {
        app.diagnostics.tasks.insert(
            "build".to_string(),
            vec!["task 'build': broken".to_string()],
        );
    }
    let mut scope = module.scope("app");

    scope.reach(["build", "lint"], &module);

    let Err(error) = scope.graph(["build"]) else {
        panic!("a reached problem");
    };
    assert!(
        error.to_string().contains("task 'build': broken"),
        "{error}"
    );
    assert!(scope.graph(["lint"]).is_ok());
}
