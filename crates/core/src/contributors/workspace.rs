//! Built-in task contributors for package-manager workspaces.

use super::{
    AutoAssociate, CONTRIBUTOR_TASK_PREFIX, Contributor, ContributorActivation, ContributorTask,
};

/// Create the built-in bun workspace contributor
#[must_use]
pub fn bun_workspace_contributor() -> Contributor {
    Contributor {
        id: "bun.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["bun".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "bun.workspace.install".to_string(),
                command: Some("bun".to_string()),
                args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
                inputs: vec!["package.json".to_string(), "bun.lock".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install Bun dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "bun.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["bun.workspace.install".to_string()],
                description: Some("Bun workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["bun".to_string(), "bunx".to_string()],
            inject_dependency: Some(format!("{}bun.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Create the built-in npm workspace contributor
#[must_use]
pub fn npm_workspace_contributor() -> Contributor {
    Contributor {
        id: "npm.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["npm".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "npm.workspace.install".to_string(),
                command: Some("npm".to_string()),
                args: vec!["ci".to_string()],
                inputs: vec!["package.json".to_string(), "package-lock.json".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install npm dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "npm.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["npm.workspace.install".to_string()],
                description: Some("npm workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["npm".to_string(), "npx".to_string()],
            inject_dependency: Some(format!("{}npm.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Create the built-in pnpm workspace contributor
#[must_use]
pub fn pnpm_workspace_contributor() -> Contributor {
    Contributor {
        id: "pnpm.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["pnpm".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "pnpm.workspace.install".to_string(),
                command: Some("pnpm".to_string()),
                args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
                inputs: vec!["package.json".to_string(), "pnpm-lock.yaml".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install pnpm dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "pnpm.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["pnpm.workspace.install".to_string()],
                description: Some("pnpm workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["pnpm".to_string(), "pnpx".to_string()],
            inject_dependency: Some(format!("{}pnpm.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Create the built-in yarn workspace contributor
#[must_use]
pub fn yarn_workspace_contributor() -> Contributor {
    Contributor {
        id: "yarn.workspace".to_string(),
        when: Some(ContributorActivation {
            workspace_member: vec!["yarn".to_string()],
            ..Default::default()
        }),
        tasks: vec![
            ContributorTask {
                id: "yarn.workspace.install".to_string(),
                command: Some("yarn".to_string()),
                args: vec!["install".to_string(), "--immutable".to_string()],
                inputs: vec!["package.json".to_string(), "yarn.lock".to_string()],
                outputs: vec!["node_modules".to_string()],
                hermetic: false,
                description: Some("Install Yarn dependencies".to_string()),
                ..Default::default()
            },
            ContributorTask {
                id: "yarn.workspace.setup".to_string(),
                script: Some("true".to_string()),
                hermetic: false,
                depends_on: vec!["yarn.workspace.install".to_string()],
                description: Some("Yarn workspace setup complete".to_string()),
                ..Default::default()
            },
        ],
        auto_associate: Some(AutoAssociate {
            command: vec!["yarn".to_string()],
            inject_dependency: Some(format!("{}yarn.workspace.setup", CONTRIBUTOR_TASK_PREFIX)),
        }),
    }
}

/// Returns all built-in workspace contributors
#[must_use]
pub fn builtin_workspace_contributors() -> Vec<Contributor> {
    vec![
        bun_workspace_contributor(),
        npm_workspace_contributor(),
        pnpm_workspace_contributor(),
        yarn_workspace_contributor(),
    ]
}
