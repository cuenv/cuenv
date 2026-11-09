use std::fs;

use crate::types::Hook;

fn hook(directory: Option<&str>, inputs: &[&str]) -> Hook {
    Hook {
        order: 0,
        propagate: false,
        command: "sh".to_string(),
        args: Vec::new(),
        dir: directory.map(ToString::to_string),
        inputs: inputs.iter().map(ToString::to_string).collect(),
        source: None,
    }
}

#[test]
fn relative_hook_directories_are_resolved_against_the_project() {
    let project = tempfile::tempdir().unwrap();
    let directory = project.path().join("hook-input-subdirectory");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("input"), "first").unwrap();
    let hooks = [hook(Some("hook-input-subdirectory"), &["input"])];
    let first = super::compute_execution_hash(&hooks, project.path()).unwrap();
    fs::write(directory.join("input"), "second").unwrap();
    let second = super::compute_execution_hash(&hooks, project.path()).unwrap();
    assert_ne!(first, second);
}

#[test]
fn missing_empty_and_non_file_inputs_are_distinguished() {
    let project = tempfile::tempdir().unwrap();
    let hooks = [hook(None, &["input"])];
    let missing = super::compute_execution_hash(&hooks, project.path()).unwrap();
    fs::write(project.path().join("input"), []).unwrap();
    let empty = super::compute_execution_hash(&hooks, project.path()).unwrap();
    assert_ne!(missing, empty);
    fs::remove_file(project.path().join("input")).unwrap();
    assert_eq!(
        super::compute_execution_hash(&hooks, project.path()).unwrap(),
        missing
    );
    fs::create_dir(project.path().join("input")).unwrap();
    assert!(super::compute_execution_hash(&hooks, project.path()).is_err());
}

#[test]
fn adjacent_input_contents_have_unambiguous_boundaries() {
    let project = tempfile::tempdir().unwrap();
    let hooks = [hook(None, &["a", "b"])];
    fs::write(project.path().join("a"), b"xfile:b:").unwrap();
    fs::write(project.path().join("b"), b"y").unwrap();
    let first = super::compute_execution_hash(&hooks, project.path()).unwrap();
    fs::write(project.path().join("a"), b"x").unwrap();
    fs::write(project.path().join("b"), b"file:b:y").unwrap();
    let second = super::compute_execution_hash(&hooks, project.path()).unwrap();
    assert_ne!(first, second);
}
