//! Core types for representing workspaces, dependencies, and package managers.

mod dependency;
mod package_manager;
mod workspace;

pub use dependency::{
    DependencyRef, DependencySource, DependencySpec, LockfileEntry, Url, Version, VersionReq,
};
pub use package_manager::PackageManager;
pub use workspace::{Workspace, WorkspaceMember};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_workspace_new() {
        let workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        assert_eq!(workspace.root, PathBuf::from("/workspace"));
        assert_eq!(workspace.manager, PackageManager::Npm);
        assert_eq!(workspace.members.len(), 0);
        assert_eq!(workspace.lockfile, None);
    }

    #[test]
    fn test_workspace_add_member() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        let member = WorkspaceMember {
            name: "pkg1".to_string(),
            path: PathBuf::from("packages/pkg1"),
            manifest_path: PathBuf::from("packages/pkg1/package.json"),
            dependencies: vec![],
        };

        workspace.add_member(member);
        assert_eq!(workspace.member_count(), 1);
    }

    #[test]
    fn test_workspace_find_member() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        let member1 = WorkspaceMember {
            name: "pkg1".to_string(),
            path: PathBuf::from("packages/pkg1"),
            manifest_path: PathBuf::from("packages/pkg1/package.json"),
            dependencies: vec![],
        };

        let member2 = WorkspaceMember {
            name: "pkg2".to_string(),
            path: PathBuf::from("packages/pkg2"),
            manifest_path: PathBuf::from("packages/pkg2/package.json"),
            dependencies: vec!["pkg1".to_string()],
        };

        workspace.add_member(member1);
        workspace.add_member(member2);

        assert!(workspace.find_member("pkg1").is_some());
        assert!(workspace.find_member("pkg2").is_some());
        assert!(workspace.find_member("pkg3").is_none());

        let found = workspace.find_member("pkg2").unwrap();
        assert_eq!(found.name, "pkg2");
        assert_eq!(found.dependencies, vec!["pkg1"]);
    }

    #[test]
    fn test_workspace_contains_path() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Bun);

        workspace.add_member(WorkspaceMember {
            name: "website".to_string(),
            path: PathBuf::from("projects/website"),
            manifest_path: PathBuf::from("projects/website/package.json"),
            dependencies: vec![],
        });

        workspace.add_member(WorkspaceMember {
            name: "api".to_string(),
            path: PathBuf::from("packages/api"),
            manifest_path: PathBuf::from("packages/api/package.json"),
            dependencies: vec![],
        });

        // Exact match
        assert!(workspace.contains_path(&PathBuf::from("projects/website")));
        assert!(workspace.contains_path(&PathBuf::from("packages/api")));

        // Not a member
        assert!(!workspace.contains_path(&PathBuf::from("other/project")));
        assert!(!workspace.contains_path(&PathBuf::from("projects/different")));

        // Empty workspace
        let empty = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);
        assert!(!empty.contains_path(&PathBuf::from("any/path")));
    }

    #[test]
    fn test_workspace_member_count() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Cargo);
        assert_eq!(workspace.member_count(), 0);

        workspace.add_member(WorkspaceMember {
            name: "crate1".to_string(),
            path: PathBuf::from("crates/crate1"),
            manifest_path: PathBuf::from("crates/crate1/Cargo.toml"),
            dependencies: vec![],
        });
        assert_eq!(workspace.member_count(), 1);

        workspace.add_member(WorkspaceMember {
            name: "crate2".to_string(),
            path: PathBuf::from("crates/crate2"),
            manifest_path: PathBuf::from("crates/crate2/Cargo.toml"),
            dependencies: vec![],
        });
        assert_eq!(workspace.member_count(), 2);
    }

    #[test]
    fn test_workspace_serialization() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        workspace.add_member(WorkspaceMember {
            name: "pkg1".to_string(),
            path: PathBuf::from("packages/pkg1"),
            manifest_path: PathBuf::from("packages/pkg1/package.json"),
            dependencies: vec![],
        });

        let json = serde_json::to_string(&workspace).unwrap();
        let deserialized: Workspace = serde_json::from_str(&json).unwrap();

        assert_eq!(workspace, deserialized);
    }

    #[test]
    fn test_package_manager_lockfile_name() {
        assert_eq!(PackageManager::Npm.lockfile_name(), "package-lock.json");
        assert_eq!(PackageManager::Bun.lockfile_name(), "bun.lock");
        assert_eq!(PackageManager::Pnpm.lockfile_name(), "pnpm-lock.yaml");
        assert_eq!(PackageManager::YarnClassic.lockfile_name(), "yarn.lock");
        assert_eq!(PackageManager::YarnModern.lockfile_name(), "yarn.lock");
        assert_eq!(PackageManager::Cargo.lockfile_name(), "Cargo.lock");
        assert_eq!(PackageManager::Deno.lockfile_name(), "deno.lock");
    }

    #[test]
    fn test_package_manager_manifest_name() {
        assert_eq!(PackageManager::Npm.manifest_name(), "package.json");
        assert_eq!(PackageManager::Bun.manifest_name(), "package.json");
        assert_eq!(PackageManager::Pnpm.manifest_name(), "package.json");
        assert_eq!(PackageManager::YarnClassic.manifest_name(), "package.json");
        assert_eq!(PackageManager::YarnModern.manifest_name(), "package.json");
        assert_eq!(PackageManager::Cargo.manifest_name(), "Cargo.toml");
        assert_eq!(PackageManager::Deno.manifest_name(), "deno.json");
    }

    #[test]
    fn test_package_manager_workspace_config_name() {
        assert_eq!(PackageManager::Npm.workspace_config_name(), "package.json");
        assert_eq!(PackageManager::Bun.workspace_config_name(), "package.json");
        assert_eq!(
            PackageManager::Pnpm.workspace_config_name(),
            "pnpm-workspace.yaml"
        );
        assert_eq!(
            PackageManager::YarnClassic.workspace_config_name(),
            "package.json"
        );
        assert_eq!(
            PackageManager::YarnModern.workspace_config_name(),
            "package.json"
        );
        assert_eq!(PackageManager::Cargo.workspace_config_name(), "Cargo.toml");
        assert_eq!(PackageManager::Deno.workspace_config_name(), "deno.json");
    }

    #[test]
    fn test_package_manager_display() {
        assert_eq!(PackageManager::Npm.to_string(), "npm");
        assert_eq!(PackageManager::Bun.to_string(), "bun");
        assert_eq!(PackageManager::Pnpm.to_string(), "pnpm");
        assert_eq!(PackageManager::YarnClassic.to_string(), "yarn-classic");
        assert_eq!(PackageManager::YarnModern.to_string(), "yarn-modern");
        assert_eq!(PackageManager::Cargo.to_string(), "cargo");
        assert_eq!(PackageManager::Deno.to_string(), "deno");
    }

    #[test]
    fn test_package_manager_serialization() {
        let managers = vec![
            PackageManager::Npm,
            PackageManager::Bun,
            PackageManager::Pnpm,
            PackageManager::YarnClassic,
            PackageManager::YarnModern,
            PackageManager::Cargo,
            PackageManager::Deno,
        ];

        for manager in managers {
            let json = serde_json::to_string(&manager).unwrap();
            let deserialized: PackageManager = serde_json::from_str(&json).unwrap();
            assert_eq!(manager, deserialized);
        }
    }

    #[test]
    fn test_dependency_spec_registry() {
        let spec = DependencySpec::Registry {
            version: "^1.0.0".to_string(),
            registry: None,
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_dependency_spec_workspace() {
        let spec = DependencySpec::Workspace {
            path: PathBuf::from("../other-package"),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_dependency_spec_git() {
        let spec = DependencySpec::Git {
            url: "https://github.com/user/repo.git".to_string(),
            rev: Some("main".to_string()),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_dependency_spec_path() {
        let spec = DependencySpec::Path {
            path: PathBuf::from("../local-dep"),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_lockfile_entry() {
        let entry = LockfileEntry {
            name: "example".to_string(),
            version: "1.2.3".to_string(),
            source: DependencySource::Registry("https://registry.npmjs.org/".to_string()),
            checksum: Some("sha512-abc123".to_string()),
            dependencies: vec![DependencyRef {
                name: "dep1".to_string(),
                version_req: "^2.0.0".to_string(),
            }],
            is_workspace_member: false,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: LockfileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_lockfile_entry_workspace_member() {
        let entry = LockfileEntry {
            name: "workspace-pkg".to_string(),
            version: "0.1.0".to_string(),
            source: DependencySource::Workspace(PathBuf::from("packages/workspace-pkg")),
            checksum: None,
            dependencies: vec![],
            is_workspace_member: true,
        };

        assert!(entry.is_workspace_member);

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: LockfileEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_find_member_by_path() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Bun);

        workspace.add_member(WorkspaceMember {
            name: "website".to_string(),
            path: PathBuf::from("projects/website"),
            manifest_path: PathBuf::from("projects/website/package.json"),
            dependencies: vec![],
        });

        workspace.add_member(WorkspaceMember {
            name: "api".to_string(),
            path: PathBuf::from("packages/api"),
            manifest_path: PathBuf::from("packages/api/package.json"),
            dependencies: vec![],
        });

        // Find by exact path
        let found = workspace.find_member_by_path(&PathBuf::from("projects/website"));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "website");

        let found = workspace.find_member_by_path(&PathBuf::from("packages/api"));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "api");

        // Not found
        assert!(
            workspace
                .find_member_by_path(&PathBuf::from("other/path"))
                .is_none()
        );
    }

    #[test]
    fn test_resolve_workspace_dependency_paths() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Bun);

        workspace.add_member(WorkspaceMember {
            name: "shared".to_string(),
            path: PathBuf::from("packages/shared"),
            manifest_path: PathBuf::from("packages/shared/package.json"),
            dependencies: vec![],
        });

        workspace.add_member(WorkspaceMember {
            name: "app".to_string(),
            path: PathBuf::from("packages/app"),
            manifest_path: PathBuf::from("packages/app/package.json"),
            dependencies: vec!["shared".to_string()],
        });

        let deps = workspace.resolve_workspace_dependency_paths("app");
        assert_eq!(deps.len(), 1);
        assert!(deps.contains(&PathBuf::from("packages/shared")));

        // shared has no workspace deps
        let deps = workspace.resolve_workspace_dependency_paths("shared");
        assert!(deps.is_empty());
    }

    #[test]
    fn test_resolve_transitive_workspace_deps() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Bun);

        // core -> (no deps)
        // utils -> core
        // app -> utils (transitively depends on core)
        workspace.add_member(WorkspaceMember {
            name: "core".to_string(),
            path: PathBuf::from("packages/core"),
            manifest_path: PathBuf::from("packages/core/package.json"),
            dependencies: vec![],
        });

        workspace.add_member(WorkspaceMember {
            name: "utils".to_string(),
            path: PathBuf::from("packages/utils"),
            manifest_path: PathBuf::from("packages/utils/package.json"),
            dependencies: vec!["core".to_string()],
        });

        workspace.add_member(WorkspaceMember {
            name: "app".to_string(),
            path: PathBuf::from("packages/app"),
            manifest_path: PathBuf::from("packages/app/package.json"),
            dependencies: vec!["utils".to_string()],
        });

        let deps = workspace.resolve_workspace_dependency_paths("app");
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&PathBuf::from("packages/utils")));
        assert!(deps.contains(&PathBuf::from("packages/core")));
    }

    #[test]
    fn test_resolve_workspace_deps_with_circular() {
        let mut workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Npm);

        // a -> b -> c -> a (circular)
        workspace.add_member(WorkspaceMember {
            name: "a".to_string(),
            path: PathBuf::from("packages/a"),
            manifest_path: PathBuf::from("packages/a/package.json"),
            dependencies: vec!["b".to_string()],
        });

        workspace.add_member(WorkspaceMember {
            name: "b".to_string(),
            path: PathBuf::from("packages/b"),
            manifest_path: PathBuf::from("packages/b/package.json"),
            dependencies: vec!["c".to_string()],
        });

        workspace.add_member(WorkspaceMember {
            name: "c".to_string(),
            path: PathBuf::from("packages/c"),
            manifest_path: PathBuf::from("packages/c/package.json"),
            dependencies: vec!["a".to_string()],
        });

        // Should not infinite loop, should collect all deps
        let deps = workspace.resolve_workspace_dependency_paths("a");
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&PathBuf::from("packages/b")));
        assert!(deps.contains(&PathBuf::from("packages/c")));
    }

    #[test]
    fn test_resolve_workspace_deps_unknown_member() {
        let workspace = Workspace::new(PathBuf::from("/workspace"), PackageManager::Bun);

        // Unknown member should return empty set
        let deps = workspace.resolve_workspace_dependency_paths("nonexistent");
        assert!(deps.is_empty());
    }
}
