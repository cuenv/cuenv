use super::{Detected, detect, shebang_program};

fn program(name: &str) -> Detected {
    Detected::Program(name.to_string())
}

#[test]
fn shebangs_name_their_program_through_env_and_its_options() {
    assert_eq!(shebang_program("#!/bin/bash"), Some("bash".to_string()));
    assert_eq!(
        shebang_program("#!/usr/bin/env bash"),
        Some("bash".to_string())
    );
    assert_eq!(
        shebang_program("#!/usr/bin/env -S bun run"),
        Some("bun".to_string())
    );
    assert_eq!(
        shebang_program("#!/usr/bin/env -S LANG=C python3 -u"),
        Some("python3".to_string())
    );
    assert_eq!(shebang_program("echo hi"), None);
    assert_eq!(shebang_program("#!"), None);
}

#[test]
fn the_shebang_wins_then_an_unambiguous_extension() {
    assert_eq!(
        detect("x.py", "#!/usr/bin/env python3.12"),
        Some(program("python3.12"))
    );
    assert_eq!(detect("x.ts", "#!/usr/bin/env bun"), Some(program("bun")));
    assert_eq!(detect("x.py", ""), Some(program("python3")));
    assert_eq!(detect("x.nu", ""), Some(program("nu")));
    assert_eq!(detect("x.fish", ""), Some(program("fish")));
    assert_eq!(detect("x.bash", ""), Some(program("bash")));
    assert_eq!(detect("x", "#!/bin/sh"), Some(program("sh")));
    // JavaScript and TypeScript are never assumed to be Bun, nor `.sh` bash.
    for file in ["x.ts", "x.js", "x.sh"] {
        assert_eq!(detect(file, ""), Some(Detected::Ambiguous), "{file}");
    }
    assert_eq!(
        detect("x.js", "#!/usr/bin/env node"),
        Some(Detected::Unsupported("node".to_string()))
    );
    // A file with no sign of being a script is no script.
    assert_eq!(detect("README.md", "# Scripts"), None);
    assert_eq!(detect("Makefile", "all:"), None);
}
