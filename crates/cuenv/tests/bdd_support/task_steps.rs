#![allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::unwrap_used
)]

use super::TestWorld;
use cucumber::{given, then, when};
use std::fmt::Write;

fn generate_task_cue(tasks: &[(String, String, Vec<String>)]) -> String {
    // Only include `let _t = tasks` if any task has dependencies (CUE requires let clauses to be used)
    let has_deps = tasks.iter().any(|(_, _, deps)| !deps.is_empty());

    let mut cue = if has_deps {
        String::from(
            r#"package test

name: "task-test"

let _t = tasks

tasks: {
"#,
        )
    } else {
        String::from(
            r#"package test

name: "task-test"

tasks: {
"#,
        )
    };

    for (name, command, deps) in tasks {
        // Parse command - if it contains spaces, split into command and args
        let (cmd, args): (String, Option<String>) = if command.contains(' ') {
            let mut parts = command.splitn(2, ' ');
            let cmd_part = parts.next().unwrap_or("").to_string();
            let args_part = parts.next().map(|s| s.to_string());
            (cmd_part, args_part)
        } else {
            (command.clone(), None)
        };

        let _ = writeln!(cue, "    {name}: {{");
        let _ = writeln!(cue, "        command: \"{cmd}\"");

        if let Some(args_str) = args {
            // Parse arguments - handle both quoted strings and shell commands
            if args_str.starts_with("-c") {
                let _ = writeln!(
                    cue,
                    "        args: [\"-c\", \"{}\"]",
                    args_str
                        .trim_start_matches("-c")
                        .trim()
                        .trim_matches(|c| c == '\'' || c == '"')
                );
            } else {
                let _ = writeln!(cue, "        args: [\"{}\"]", args_str.trim_matches('"'));
            }
        } else {
            let _ = writeln!(cue, "        args: [\"{name} executed\"]");
        }

        if !deps.is_empty() {
            // Use CUE refs for dependsOn (e.g., _t.build instead of "build")
            let deps_str = deps
                .iter()
                .map(|d| format!("_t.{d}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(cue, "        dependsOn: [{deps_str}]");
        }
        cue.push_str("    }\n");
    }

    cue.push_str("}\n");
    cue
}

#[given(expr = "a project with tasks:")]
async fn given_project_with_tasks(world: &mut TestWorld, step: &cucumber::gherkin::Step) {
    // Parse the data table from the step
    let table = step.table.as_ref().expect("Expected a data table");

    let tasks: Vec<(String, String, Vec<String>)> = table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            let name = row[0].clone();
            let command = row[1].clone();
            let deps_str = row[2].trim_matches(|c| c == '[' || c == ']');
            let deps: Vec<String> = if deps_str.is_empty() {
                vec![]
            } else {
                deps_str.split(',').map(|s| s.trim().to_string()).collect()
            };
            (name, command, deps)
        })
        .collect();

    let cue_content = generate_task_cue(&tasks);
    world.write_test_project("task_test", &cue_content).await;
}

#[given(expr = "a project with parallel tasks {string} and {string}")]
async fn given_project_with_parallel_tasks(world: &mut TestWorld, task1: String, task2: String) {
    // Schema-free for test isolation
    let cue_content = format!(
        r#"package test

name: "parallel-task-test"

tasks: {{
    check: {{
        type: "group"
        {task1}: {{
            command: "echo"
            args: ["{task1} executed"]
        }}
        {task2}: {{
            command: "echo"
            args: ["{task2} executed"]
        }}
    }}
}}
"#
    );

    world
        .write_test_project("parallel_test", &cue_content)
        .await;
}

#[given(expr = "a project with a parallel group {string} containing {string} and {string}")]
async fn given_project_with_parallel_group(
    world: &mut TestWorld,
    group: String,
    task1: String,
    task2: String,
) {
    // Schema-free for test isolation
    let cue_content = format!(
        r#"package test

name: "group-task-test"

tasks: {{
    {group}: {{
        type: "group"
        {task1}: {{
            command: "echo"
            args: ["{task1} executed"]
        }}
        {task2}: {{
            command: "echo"
            args: ["{task2} executed"]
        }}
    }}
}}
"#
    );

    world.write_test_project("group_test", &cue_content).await;
}

#[when(expr = "I run {string}")]
async fn when_i_run_command(world: &mut TestWorld, command: String) {
    // Parse the command - expecting "cuenv task <args>" or "cuenv env <args>"
    let parts: Vec<&str> = command.split_whitespace().collect();

    if parts.first() == Some(&"cuenv") {
        // Build args with --path and --package before the command args
        let path = world.current_dir.to_str().unwrap().to_string();
        let path_str: &'static str = Box::leak(path.into_boxed_str());

        // Get the subcommand (e.g., "task" or "env")
        let subcommand = parts.get(1).copied().unwrap_or("");

        // Build the full args list with path/package options in correct position
        let mut args = vec![subcommand];

        // Add remaining args from the command
        args.extend(parts[2..].iter().copied());

        // Add --path and --package at the end (they're global options)
        args.push("--path");
        args.push(path_str);
        args.push("--package");
        args.push("test");

        world.run_cuenv(&args).await.unwrap();
    } else {
        assert!(
            command.starts_with("cuenv"),
            "Expected command to start with 'cuenv', got: {command}"
        );
    }
}

#[then(expr = "the task {string} should complete before {string}")]
fn then_task_completes_before(world: &mut TestWorld, first: String, second: String) {
    // Check the output for task execution order
    // The output should show first task completing before second starts
    let output = &world.last_output;

    // Find positions of task names in output
    let first_pos = output.find(&format!("{first} executed"));
    let second_pos = output.find(&format!("{second} executed"));

    match (first_pos, second_pos) {
        (Some(f), Some(s)) => {
            assert!(
                f < s,
                "Task '{first}' should complete before '{second}'. Output: {output}"
            );
        }
        (None, _) => {
            // If we can't find "executed" markers, check for task names in order
            let first_mention = output.find(&first);
            let second_mention = output.find(&second);
            if let (Some(f), Some(s)) = (first_mention, second_mention) {
                assert!(
                    f < s,
                    "Task '{first}' should appear before '{second}' in output. Output: {output}"
                );
            }
        }
        _ => {}
    }
}

#[then(expr = "the task {string} should fail")]
fn then_task_should_fail(world: &mut TestWorld, task: String) {
    let output = &world.last_output;
    assert!(
        output.to_lowercase().contains("fail")
            || output.to_lowercase().contains("error")
            || world.last_exit_code != 0,
        "Task '{task}' should have failed. Output: {output}, Exit code: {}",
        world.last_exit_code
    );
}

#[then(expr = "the task {string} should not execute")]
fn then_task_should_not_execute(world: &mut TestWorld, task: String) {
    let output = &world.last_output;
    // The task should not appear as executed in the output
    assert!(
        !output.contains(&format!("{task} executed")),
        "Task '{task}' should not have executed. Output: {output}"
    );
}

#[then(expr = "both {string} and {string} should execute")]
fn then_both_tasks_execute(world: &mut TestWorld, task1: String, task2: String) {
    let output = &world.last_output;
    assert!(
        output.contains(&format!("{task1} executed")) || output.contains(&task1),
        "Task '{task1}' should have executed. Output: {output}"
    );
    assert!(
        output.contains(&format!("{task2} executed")) || output.contains(&task2),
        "Task '{task2}' should have executed. Output: {output}"
    );
}

#[then(expr = "the task {string} should execute")]
fn then_task_should_execute(world: &mut TestWorld, task: String) {
    let output = &world.last_output;
    assert!(
        output.contains(&format!("{task} executed")) || output.contains(&task),
        "Task '{task}' should have executed. Output: {output}"
    );
}

#[then(expr = "the output should contain {string}")]
fn then_output_contains(world: &mut TestWorld, expected: String) {
    assert!(
        world.last_output.contains(&expected),
        "Output should contain '{}'. Actual output: {}",
        expected,
        world.last_output
    );
}

#[then(expr = "the exit code should be {int}")]
fn then_exit_code_is(world: &mut TestWorld, code: i32) {
    assert_eq!(
        world.last_exit_code, code,
        "Exit code should be {}. Actual: {}. Output: {}",
        code, world.last_exit_code, world.last_output
    );
}

#[then(expr = "the exit code should not be {int}")]
fn then_exit_code_is_not(world: &mut TestWorld, code: i32) {
    assert_ne!(
        world.last_exit_code, code,
        "Exit code should not be {}. Output: {}",
        code, world.last_output
    );
}
