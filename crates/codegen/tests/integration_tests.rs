use cuenv_codegen::{Blueprint, Generator, GenerateOptions};
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_basic_blueprint_json() {
    // Create a simple blueprint in JSON format
    let temp_dir = TempDir::new().unwrap();
    let blueprint_path = temp_dir.path().join("blueprint.json");

    let blueprint_json = r#"{
        "files": {
            "test.json": {
                "path": "test.json",
                "content": "{\"name\":\"test\"}",
                "language": "json",
                "mode": "managed",
                "format": {
                    "indent": "space",
                    "indentSize": 2
                }
            }
        },
        "context": null
    }"#;

    std::fs::write(&blueprint_path, blueprint_json).unwrap();

    // Load and generate
    let blueprint = Blueprint::load(&blueprint_path).unwrap();
    let generator = Generator::new(blueprint);

    let output_dir = TempDir::new().unwrap();
    let options = GenerateOptions {
        output_dir: output_dir.path().to_path_buf(),
        check: false,
        diff: false,
    };

    let result = generator.generate(&options);
    assert!(result.is_ok());

    let generated = result.unwrap();
    assert_eq!(generated.len(), 1);

    let file_path = output_dir.path().join("test.json");
    assert!(file_path.exists());

    let content = std::fs::read_to_string(file_path).unwrap();
    assert!(content.contains("\"name\""));
}
