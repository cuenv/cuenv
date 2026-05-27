use super::*;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use tempfile::tempdir;

macro_rules! sync_vcs {
    ($root:expr, $dependencies:expr, $options:expr, $scope:expr $(,)?) => {
        sync_vcs_dependencies(VcsSyncRequest {
            module_root: $root,
            dependencies: $dependencies,
            options: $options,
            scope: $scope,
        })
    };
}

#[test]
fn gitignore_section_is_replaced() {
    let workspace = create_workspace();
    fs::write(
        workspace.path().join(".gitignore"),
        "target/\n\n# BEGIN cuenv vcs\nold/\n# END cuenv vcs\n",
    )
    .expect("gitignore");

    sync_gitignore(workspace.path(), &[".cuenv/vcs/lib/".to_string()]).expect("sync gitignore");
    let content = fs::read_to_string(workspace.path().join(".gitignore")).expect("gitignore");
    assert!(content.contains("target/"));
    assert!(content.contains("# BEGIN cuenv vcs"));
    assert!(content.contains(".cuenv/vcs/lib/"));
    assert!(!content.contains("old/"));

    fs::write(
        workspace.path().join(".gitignore"),
        "# BEGIN cuenv vcs\nold/\n",
    )
    .expect("malformed gitignore");
    assert!(sync_gitignore(workspace.path(), &[".cuenv/vcs/lib/".to_string()]).is_err());
}

#[test]
fn invalid_materialization_paths_are_rejected() {
    let workspace = create_workspace();
    let root = workspace.path();
    assert!(validate_materialization_path(root, "../dep").is_err());
    assert!(validate_materialization_path(root, "/dep").is_err());
    assert!(validate_materialization_path(root, ".").is_err());
    assert!(validate_materialization_path(root, "./").is_err());
    assert!(validate_materialization_path(root, ".git/hooks").is_err());
    assert!(validate_materialization_path(root, "vendor/.git/hooks").is_err());
    assert!(validate_materialization_path(root, ".cuenv/vcs/cache/lib").is_err());
    assert!(validate_materialization_path(root, ".cuenv/vcs/tmp/lib").is_err());
    assert!(validate_materialization_path(root, "vendor/de[p]").is_err());
    assert!(validate_materialization_path(root, "vendor/dep").is_ok());
}

#[test]
fn syncs_vendored_dependency_from_local_repo() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/lib".to_string(),
            subdir: None,
        },
    };

    let output = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("sync should succeed");

    assert!(output.contains("lib: Synced"));
    assert!(workspace.path().join("vendor/lib/lib.txt").exists());
    assert!(!workspace.path().join("vendor/lib/.git").exists());
    assert!(workspace.path().join("vendor/lib/.cuenv-vcs").exists());
    assert!(workspace.path().join("cuenv.lock").exists());
    let gitignore = fs::read_to_string(workspace.path().join(".gitignore")).expect("gitignore");
    assert!(gitignore.contains("vendor/lib/.cuenv-vcs"));
}

#[test]
fn check_rejects_changed_vcs_spec() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("sync should succeed");

    let mut changed = dependency;
    changed.spec.reference = "other".to_string();
    let options = SyncOptions {
        mode: SyncMode::Check,
        ..SyncOptions::default()
    };
    let err = sync_vcs!(
        workspace.path(),
        vec![changed],
        &options,
        VcsSyncScope::Path,
    )
    .expect_err("check should reject changed spec");
    assert!(err.to_string().contains("out of sync"));
}

#[test]
fn check_rejects_modified_vendored_content() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("sync should succeed");
    fs::write(workspace.path().join("vendor/lib/lib.txt"), "changed\n").expect("mutate vendor");

    let options = SyncOptions {
        mode: SyncMode::Check,
        ..SyncOptions::default()
    };
    let err = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &options,
        VcsSyncScope::Path,
    )
    .expect_err("check should reject modified vendor");
    assert!(err.to_string().contains("has tree"));
}

#[test]
fn unmanaged_git_target_is_not_replaced() {
    let source = create_source_repo();
    let workspace = create_workspace();
    fs::create_dir_all(workspace.path().join("vendor/lib/.git")).expect("existing checkout");
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/lib".to_string(),
            subdir: None,
        },
    };

    let err = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect_err("sync should refuse unmanaged target");
    assert!(err.to_string().contains("Refusing to overwrite unmanaged"));
}

#[test]
fn dry_run_does_not_write_workspace_cache() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };
    let options = SyncOptions {
        mode: SyncMode::DryRun,
        ..SyncOptions::default()
    };

    sync_vcs!(
        workspace.path(),
        vec![dependency],
        &options,
        VcsSyncScope::Path,
    )
    .expect("dry-run should succeed");

    assert!(!workspace.path().join(".cuenv/vcs/cache").exists());
    assert!(!workspace.path().join(".cuenv/vcs/lib").exists());
    assert!(!workspace.path().join("cuenv.lock").exists());
}

#[test]
fn duplicate_names_and_paths_are_rejected() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/lib".to_string(),
            subdir: None,
        },
    };
    let mut conflicting_name = dependency.clone();
    conflicting_name.spec.reference = "HEAD".to_string();
    let err = sync_vcs!(
        workspace.path(),
        vec![dependency.clone(), conflicting_name],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect_err("same name with different spec should fail");
    assert!(err.to_string().contains("different configuration"));

    let mut conflicting_path = dependency;
    conflicting_path.name = "other".to_string();
    let mut normalized_conflict = conflicting_path.clone();
    normalized_conflict.name = "third".to_string();
    normalized_conflict.spec.path = "vendor/lib/".to_string();
    let err = sync_vcs!(
        workspace.path(),
        vec![conflicting_path, normalized_conflict],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect_err("same path should fail");
    assert!(
        err.to_string().contains("overlapping paths"),
        "unexpected error: {err}"
    );
}

#[test]
fn non_vendored_dependency_updates_gitignore() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };

    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("sync should succeed");

    let gitignore = fs::read_to_string(workspace.path().join(".gitignore")).expect("gitignore");
    assert!(gitignore.contains("# BEGIN cuenv vcs"));
    assert!(gitignore.contains(".cuenv/vcs/lib/"));
    assert!(gitignore.contains(".cuenv/vcs/cache/"));
    assert!(gitignore.contains(".cuenv/vcs/tmp/"));
    assert!(workspace.path().join(".cuenv/vcs/lib/.git").exists());

    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("second sync should not treat cuenv marker as dirty");

    fs::write(workspace.path().join(".cuenv/vcs/lib/lib.txt"), "changed\n")
        .expect("mutate checkout");
    let options = SyncOptions {
        mode: SyncMode::Check,
        ..SyncOptions::default()
    };
    let err = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &options,
        VcsSyncScope::Path,
    )
    .expect_err("check should reject dirty checkout");
    assert!(err.to_string().contains("uncommitted changes"));
}

#[test]
fn nested_module_cache_path_is_ignored_relative_to_git_root() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let module_root = workspace.path().join("apps/api");
    fs::create_dir_all(module_root.join("cue.mod")).expect("cue module");
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };

    sync_vcs!(
        &module_root,
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("sync should succeed");

    let gitignore = fs::read_to_string(workspace.path().join(".gitignore")).expect("gitignore");
    assert!(gitignore.contains(".cuenv/vcs/lib/"));
    assert!(gitignore.contains("apps/api/.cuenv/vcs/cache/"));
    assert!(gitignore.contains("apps/api/.cuenv/vcs/tmp/"));
    assert!(module_root.join(".cuenv/vcs/cache").exists());

    let options = SyncOptions {
        mode: SyncMode::Check,
        ..SyncOptions::default()
    };
    sync_vcs!(&module_root, vec![dependency], &options, VcsSyncScope::Path)
        .expect("check should use the nested cache ignore path");
}

#[test]
fn check_update_does_not_write_workspace_cache() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("sync should succeed");
    fs::remove_dir_all(workspace.path().join(".cuenv/vcs/cache")).expect("remove cache");

    let options = SyncOptions {
        mode: SyncMode::Check,
        update_tools: Some(Vec::new()),
        ..SyncOptions::default()
    };
    let _ = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &options,
        VcsSyncScope::Path
    );

    assert!(!workspace.path().join(".cuenv/vcs/cache").exists());
}

#[test]
fn workspace_sync_prunes_removed_vcs_entries() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("sync should succeed");

    sync_vcs!(
        workspace.path(),
        Vec::new(),
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("prune should succeed");

    let lockfile = Lockfile::load(&workspace.path().join(LOCKFILE_NAME))
        .expect("load lockfile")
        .expect("lockfile remains");
    assert!(lockfile.vcs.is_empty());
    let gitignore = fs::read_to_string(workspace.path().join(".gitignore")).expect("gitignore");
    assert!(!gitignore.contains("# BEGIN cuenv vcs"));
    assert!(!workspace.path().join(".cuenv/vcs/lib").exists());
}

#[test]
fn workspace_sync_prunes_changed_vcs_paths() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let mut dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("sync should succeed");

    dependency.spec.path = ".cuenv/vcs/lib-renamed".to_string();
    sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("path change should sync");

    assert!(!workspace.path().join(".cuenv/vcs/lib").exists());
    assert!(workspace.path().join(".cuenv/vcs/lib-renamed").exists());
}

#[test]
fn workspace_sync_prunes_parent_before_materializing_child_path() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let mut dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("sync should succeed");

    dependency.spec.path = ".cuenv/vcs/lib/child".to_string();
    sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("path change should prune old parent before materializing child");

    assert!(!workspace.path().join(".cuenv/vcs/lib/.git").exists());
    assert!(workspace.path().join(".cuenv/vcs/lib/child/.git").exists());
}

#[test]
fn workspace_sync_allows_renamed_dependency_at_same_path() {
    let source = create_source_repo();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "lib".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/lib".to_string(),
            subdir: None,
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("sync should succeed");

    let mut renamed = dependency;
    renamed.name = "renamed".to_string();
    sync_vcs!(
        workspace.path(),
        vec![renamed],
        &SyncOptions::default(),
        VcsSyncScope::Workspace,
    )
    .expect("rename should reuse existing checkout");

    assert!(workspace.path().join(".cuenv/vcs/lib").exists());
    let lockfile = Lockfile::load(&workspace.path().join(LOCKFILE_NAME))
        .expect("load lockfile")
        .expect("lockfile remains");
    assert!(lockfile.find_vcs("lib").is_none());
    assert!(lockfile.find_vcs("renamed").is_some());
}

fn create_workspace() -> tempfile::TempDir {
    let dir = tempdir().expect("tempdir");
    run_git(
        [OsStr::new("init"), OsStr::new("-b"), OsStr::new("main")],
        Some(dir.path()),
    )
    .expect("git init");
    dir
}

fn create_source_repo() -> tempfile::TempDir {
    let dir = tempdir().expect("tempdir");
    run_git(
        [OsStr::new("init"), OsStr::new("-b"), OsStr::new("main")],
        Some(dir.path()),
    )
    .expect("git init");
    run_git(
        [
            OsStr::new("config"),
            OsStr::new("user.email"),
            OsStr::new("test@example.com"),
        ],
        Some(dir.path()),
    )
    .expect("git config email");
    run_git(
        [
            OsStr::new("config"),
            OsStr::new("user.name"),
            OsStr::new("Cuenv Test"),
        ],
        Some(dir.path()),
    )
    .expect("git config name");
    run_git(
        [
            OsStr::new("config"),
            OsStr::new("commit.gpgsign"),
            OsStr::new("false"),
        ],
        Some(dir.path()),
    )
    .expect("git config commit signing");
    let mut file = fs::File::create(dir.path().join("lib.txt")).expect("file");
    writeln!(file, "hello").expect("write");
    run_git([OsStr::new("add"), OsStr::new("lib.txt")], Some(dir.path())).expect("git add");
    run_git(
        [
            OsStr::new("commit"),
            OsStr::new("-m"),
            OsStr::new("initial"),
        ],
        Some(dir.path()),
    )
    .expect("git commit");
    dir
}

/// Source repo with multiple top-level directories so subdir tests can
/// verify a sparse checkout extracts only the requested subtree.
fn create_source_repo_with_subdirs() -> tempfile::TempDir {
    let dir = create_source_repo();
    let skills = dir.path().join(".agents/skills/example");
    fs::create_dir_all(&skills).expect("create skills subdir");
    fs::write(skills.join("SKILL.md"), "# Example skill\n").expect("write SKILL.md");
    fs::write(dir.path().join("other.txt"), "sibling content\n").expect("write sibling");
    run_git(
        [
            OsStr::new("add"),
            OsStr::new(".agents/skills/example/SKILL.md"),
            OsStr::new("other.txt"),
        ],
        Some(dir.path()),
    )
    .expect("git add subdirs");
    run_git(
        [
            OsStr::new("commit"),
            OsStr::new("-m"),
            OsStr::new("add subdirs"),
        ],
        Some(dir.path()),
    )
    .expect("git commit subdirs");
    dir
}

#[test]
fn non_vendored_subdir_extracts_subdir_and_is_ignored() {
    let source = create_source_repo_with_subdirs();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "skills".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: false,
            path: ".cuenv/vcs/skills".to_string(),
            subdir: Some(".agents/skills".to_string()),
        },
    };

    let output = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("non-vendored subdir sync should succeed");
    assert!(output.contains("skills: Synced"));

    let target = workspace.path().join(".cuenv/vcs/skills");
    assert!(target.join("example/SKILL.md").exists());
    assert!(!target.join("other.txt").exists());
    assert!(!target.join(".git").exists());
    assert!(target.join(".cuenv-vcs").exists());

    let gitignore = fs::read_to_string(workspace.path().join(".gitignore")).expect("gitignore");
    assert!(gitignore.contains(".cuenv/vcs/skills/"));

    let lockfile = Lockfile::load(&workspace.path().join(LOCKFILE_NAME))
        .expect("load lockfile")
        .expect("lockfile present");
    let entry = lockfile.find_vcs("skills").expect("entry present");
    assert!(!entry.vendor);
    assert_eq!(entry.subdir.as_deref(), Some(".agents/skills"));
    assert!(entry.subtree.is_some());
}

#[test]
fn subdir_invalid_values_rejected() {
    for bad in [
        "",
        "..",
        "./skills",
        "/abs",
        "a/../b",
        "glob*",
        " .agents/skills",
        ".agents/skills ",
        ".agents//skills",
        ".agents/skills/",
        "/.agents/skills",
        "a\\b",
        "--stdin",
        "-rf",
        ".agents/-evil",
    ] {
        assert!(
            validate_subdir(bad).is_err(),
            "expected '{bad}' to be rejected"
        );
    }
    assert!(validate_subdir(".agents/skills").is_ok());
    assert_eq!(validate_subdir(".agents/skills").unwrap(), ".agents/skills");
}

#[test]
fn sparse_checkout_extracts_only_subdir() {
    let source = create_source_repo_with_subdirs();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "skills".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: ".agents/skills".to_string(),
            subdir: Some(".agents/skills".to_string()),
        },
    };

    let output = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("subdir sync should succeed");
    assert!(output.contains("skills: Synced"));

    let target = workspace.path().join(".agents/skills");
    assert!(target.join("example/SKILL.md").exists());
    assert!(!target.join("lib.txt").exists());
    assert!(!target.join("other.txt").exists());
    assert!(!target.join(".git").exists());
    assert!(target.join(".cuenv-vcs").exists());

    let lockfile = Lockfile::load(&workspace.path().join(LOCKFILE_NAME))
        .expect("load lockfile")
        .expect("lockfile present");
    let entry = lockfile.find_vcs("skills").expect("entry present");
    assert_eq!(entry.subdir.as_deref(), Some(".agents/skills"));
    assert!(entry.subtree.is_some());
}

#[test]
fn check_rejects_modified_vendored_subdir() {
    let source = create_source_repo_with_subdirs();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "skills".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: ".agents/skills".to_string(),
            subdir: Some(".agents/skills".to_string()),
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("initial sync");
    fs::write(
        workspace.path().join(".agents/skills/example/SKILL.md"),
        "tampered\n",
    )
    .expect("mutate skill");

    let options = SyncOptions {
        mode: SyncMode::Check,
        ..SyncOptions::default()
    };
    let err = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &options,
        VcsSyncScope::Path,
    )
    .expect_err("check should reject modified subdir");
    assert!(err.to_string().contains("tree"), "unexpected error: {err}");
}

#[test]
fn changing_subdir_at_same_path_rematerialises() {
    let source = create_source_repo_with_subdirs();
    // Add a second skill under a different subdir.
    let other = source.path().join(".agents/skills/other");
    fs::create_dir_all(&other).expect("create other skill dir");
    fs::write(other.join("SKILL.md"), "# Other\n").expect("write other SKILL.md");
    run_git(
        [
            OsStr::new("add"),
            OsStr::new(".agents/skills/other/SKILL.md"),
        ],
        Some(source.path()),
    )
    .expect("git add other");
    run_git(
        [
            OsStr::new("commit"),
            OsStr::new("-m"),
            OsStr::new("add other"),
        ],
        Some(source.path()),
    )
    .expect("git commit other");

    let workspace = create_workspace();
    let mut dependency = CollectedVcsDependency {
        name: "pack".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/pack".to_string(),
            subdir: Some(".agents/skills/example".to_string()),
        },
    };
    sync_vcs!(
        workspace.path(),
        vec![dependency.clone()],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("first sync");
    assert!(workspace.path().join("vendor/pack/SKILL.md").exists());

    dependency.spec.subdir = Some(".agents/skills/other".to_string());
    sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect("subdir change should re-materialise");
    let content = fs::read_to_string(workspace.path().join("vendor/pack/SKILL.md"))
        .expect("read remateralised skill");
    assert!(content.contains("# Other"));
}

#[test]
fn subdir_referencing_blob_rejected() {
    let source = create_source_repo_with_subdirs();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "blob".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/blob".to_string(),
            subdir: Some("other.txt".to_string()),
        },
    };
    let err = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect_err("subdir pointing at a file must be rejected");
    assert!(
        err.to_string().contains("subdir") && err.to_string().contains("tree"),
        "unexpected error: {err}"
    );
}

#[test]
fn subdir_missing_at_reference_rejected() {
    let source = create_source_repo_with_subdirs();
    let workspace = create_workspace();
    let dependency = CollectedVcsDependency {
        name: "missing".to_string(),
        spec: VcsDependency {
            url: source.path().display().to_string(),
            reference: "main".to_string(),
            vendor: true,
            path: "vendor/missing".to_string(),
            subdir: Some("does/not/exist".to_string()),
        },
    };
    let err = sync_vcs!(
        workspace.path(),
        vec![dependency],
        &SyncOptions::default(),
        VcsSyncScope::Path,
    )
    .expect_err("missing subdir must be rejected");
    assert!(
        err.to_string().contains("subdir"),
        "unexpected error: {err}"
    );
}
