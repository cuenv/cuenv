use super::*;

#[test]
fn test_purity_analysis_pure_flake() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "abc123",
                        "narHash": "sha256-xxxxxxxxxxxxx"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Strict,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let result = compiler.analyze_flake_purity();

    assert!(result.is_some());
    let (digest, purity) = result.unwrap().unwrap();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(purity, PurityMode::Strict);
}

#[test]
fn test_purity_strict_mode_rejects_unlocked() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Strict,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let result = compiler.analyze_flake_purity();

    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn test_purity_warning_mode_injects_uuid() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Warning,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project.clone(), options.clone());
    let result1 = compiler.analyze_flake_purity().unwrap().unwrap();

    let compiler2 = Compiler::with_options(project, options);
    let result2 = compiler2.analyze_flake_purity().unwrap().unwrap();

    // Each compile should produce different digests due to UUID injection
    assert_ne!(result1.0, result2.0);
    assert_eq!(result1.1, PurityMode::Warning);
}

#[test]
fn test_purity_override_mode_uses_overrides() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-base"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let mut input_overrides = HashMap::new();
    input_overrides.insert("nixpkgs".to_string(), "sha256-custom".to_string());

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Override,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        input_overrides,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project.clone(), options.clone());
    let result1 = compiler.analyze_flake_purity().unwrap().unwrap();

    // Same compiler, same overrides = deterministic digest
    let compiler2 = Compiler::with_options(project, options);
    let result2 = compiler2.analyze_flake_purity().unwrap().unwrap();

    assert_eq!(result1.0, result2.0);
    assert_eq!(result1.1, PurityMode::Override);
}

#[test]
fn test_compute_runtime() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-test"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Strict,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let runtime = compiler
        .compute_runtime(
            "nix-x86_64-linux",
            "github:NixOS/nixpkgs",
            "devShells.x86_64-linux.default",
            "x86_64-linux",
        )
        .unwrap();

    assert_eq!(runtime.id, "nix-x86_64-linux");
    assert_eq!(runtime.flake, "github:NixOS/nixpkgs");
    assert!(runtime.digest.starts_with("sha256:"));
    assert_eq!(runtime.purity, PurityMode::Strict);
}
