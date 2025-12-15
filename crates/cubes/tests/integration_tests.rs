use cuenv_cubes::{Cube, GenerateOptions, Generator};
use tempfile::TempDir;

#[test]
fn test_basic_cube_cue() {
    // Create a simple cube in CUE format
    let temp_dir = TempDir::new().unwrap();
    let cube_path = temp_dir.path().join("cube.cue");

    let cube_cue = r#"package cubes

files: {
    "test.json": {
        path: "test.json"
        content: "{\"name\":\"test\"}"
        language: "json"
        mode: "managed"
        format: {
            indent: "space"
            indentSize: 2
        }
    }
}

context: null
"#;

    std::fs::write(&cube_path, cube_cue).unwrap();

    // Load and generate
    let cube = Cube::load(&cube_path).unwrap();
    let generator = Generator::new(cube);

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
