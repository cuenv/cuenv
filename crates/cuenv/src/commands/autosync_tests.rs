use super::store::{FileStamp, InfrastructurePrint, Store, digest, stamps};
use super::{AutoSync, AutoSyncMode, infrastructure_is_current};
use std::fs;
use std::path::Path;

#[test]
fn a_stamp_changes_when_a_file_is_written_or_removed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.txt");
    assert_eq!(FileStamp::of(&file), FileStamp::Missing);
    fs::write(&file, "one").unwrap();
    let first = FileStamp::of(&file);
    assert_ne!(first, FileStamp::Missing);
    fs::write(&file, "three").unwrap();
    assert_ne!(FileStamp::of(&file), first);
    fs::remove_file(&file).unwrap();
    assert_eq!(FileStamp::of(&file), FileStamp::Missing);
}

#[test]
fn fingerprints_survive_a_reopen_of_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("generated.json");
    fs::write(&file, "{}").unwrap();
    {
        let mut store = Store::open(dir.path()).unwrap();
        store.prints.codegen.insert(
            "app".to_string(),
            super::CodegenPrint {
                config: digest(&("config", 1)),
                outputs: stamps([file.as_path()]),
            },
        );
        store.save();
    }
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        store.prints.codegen["app"].outputs,
        stamps([Path::new(&file)])
    );
    assert_eq!(store.prints.codegen["app"].config, digest(&("config", 1)));
}

#[test]
fn a_disabled_process_claims_nothing() {
    let autosync = AutoSync::default();
    *autosync.mode.lock().unwrap() = AutoSyncMode::Write;
    assert!(autosync.claim(Path::new("/project")));
    assert!(!autosync.claim(Path::new("/project")), "only once per path");
    assert!(autosync.claim_rules());
    assert!(!autosync.claim_rules());
    autosync.disable();
    assert!(!autosync.claim(Path::new("/other")));
}

#[test]
fn a_digest_does_not_depend_on_map_order() {
    let mut first = std::collections::HashMap::new();
    let mut second = std::collections::HashMap::new();
    for key in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        first.insert(key.to_string(), key.len());
    }
    for key in ["h", "g", "f", "e", "d", "c", "b", "a"] {
        second.insert(key.to_string(), key.len());
    }
    assert_eq!(digest(&first), digest(&second));
}

#[test]
fn generated_infrastructure_types_are_current_until_something_changes() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("cuenv.lock");
    let provider = dir.path().join("provider.cue");
    fs::write(&lock, "version = 5").unwrap();
    fs::write(&provider, "package x").unwrap();
    let print = InfrastructurePrint {
        cuenv_version: "1.0.0".to_string(),
        files: stamps([lock.as_path(), provider.as_path()]),
    };

    assert!(!infrastructure_is_current(None, "1.0.0"), "never synced");
    assert!(infrastructure_is_current(Some(&print), "1.0.0"));
    assert!(
        !infrastructure_is_current(Some(&print), "1.1.0"),
        "a newer cuenv may render the types differently"
    );

    fs::write(&lock, "version = 5\n# pinned another release").unwrap();
    assert!(
        !infrastructure_is_current(Some(&print), "1.0.0"),
        "the lockfile changed"
    );

    let print = InfrastructurePrint {
        cuenv_version: "1.0.0".to_string(),
        files: stamps([lock.as_path(), provider.as_path()]),
    };
    fs::remove_file(&provider).unwrap();
    assert!(
        !infrastructure_is_current(Some(&print), "1.0.0"),
        "a generated provider file is missing, as on a fresh clone"
    );
}
