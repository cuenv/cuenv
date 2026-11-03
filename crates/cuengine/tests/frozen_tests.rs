//! The actual Rust -> C -> Go snapshot boundary, including allocation ownership.

use cuengine::frozen::{
    DeferredValue, FreezeOptions, FrozenEvaluationOptions, FrozenInstance, FrozenValidationOptions,
    PreparationOptions, PreparedSpecializationOptions, ScalarFill, ScalarValue,
    SpecializationOptions, ValuePath, ValueSelector,
};
use serde_json::{Value, value::RawValue};
use std::error::Error;
use std::fs;

type TestResult = Result<(), Box<dyn Error>>;

fn path(fields: &[&str]) -> ValuePath {
    fields
        .iter()
        .map(|field| ValueSelector::Field((*field).to_owned()))
        .collect()
}

fn fixture() -> Result<(tempfile::TempDir, FrozenInstance), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    fs::create_dir_all(directory.path().join("cue.mod"))?;
    fs::create_dir_all(directory.path().join("schema"))?;
    fs::write(
        directory.path().join("cue.mod/module.cue"),
        "module: \"example.test/frozen@v0\"\nlanguage: version: \"v0.15.0\"\n",
    )?;
    fs::write(
        directory.path().join("schema/types.cue"),
        "package schema\n#Config: {items: [{id: \"ok\", amount: number, enabled: bool}]}\n",
    )?;
    fs::write(
        directory.path().join("env.cue"),
        r#"package app
import "example.test/frozen/schema"
source: {id: string, amount: number, enabled: bool}
let Alias = source.id
config: schema.#Config & {items: [{id: Alias, amount: source.amount, enabled: source.enabled}]}
"#,
    )?;
    let snapshot = FrozenInstance::load(&FreezeOptions {
        module_root: directory.path().to_owned(),
        directory: directory.path().to_owned(),
        package: "app".to_owned(),
    })?;
    Ok((directory, snapshot))
}

fn deferred() -> Vec<DeferredValue> {
    ["id", "amount", "enabled"]
        .into_iter()
        .map(|field| DeferredValue {
            path: vec![
                ValueSelector::Field("config".to_owned()),
                ValueSelector::Field("items".to_owned()),
                ValueSelector::Index(0),
                ValueSelector::Field(field.to_owned()),
            ],
            target: path(&["source", field]),
        })
        .collect()
}

#[test]
fn snapshot_survives_source_deletion_and_preserves_unknown_provenance() -> TestResult {
    let (directory, snapshot) = fixture()?;
    directory.close()?;
    snapshot.validate(&FrozenValidationOptions {
        concrete_paths: vec![path(&["source"]), path(&["config"])],
        abstract_paths: vec![path(&["source"])],
        deferred: deferred(),
        ..FrozenValidationOptions::default()
    })?;
    let inspected = snapshot.evaluate(&FrozenEvaluationOptions {
        export_paths: vec![path(&["config"])],
        reference_paths: vec![path(&["config"])],
        ..FrozenEvaluationOptions::default()
    })?;
    assert_eq!(inspected.references.len(), 3);
    assert!(
        inspected
            .references
            .iter()
            .all(|reference| reference.supported)
    );

    let mut options = SpecializationOptions {
        projection: path(&["config"]),
        deferred: deferred(),
        ..SpecializationOptions::default()
    };
    let unknown = snapshot.specialize(&options)?;
    assert_eq!(unknown.unresolved.len(), 3);
    let projected: Value = serde_json::from_str(unknown.value.get())?;
    assert!(projected["items"][0]["id"].is_null());

    options.fills.push(ScalarFill {
        path: path(&["source", "id"]),
        value: ScalarValue::string("ok")?,
    });
    let partial = snapshot.specialize(&options)?;
    assert_eq!(partial.unresolved.len(), 2);

    let exact_number = "9007199254740993.1234567890123456789";
    options.fills.push(ScalarFill {
        path: path(&["source", "amount"]),
        value: ScalarValue::number(exact_number)?,
    });
    options.fills.push(ScalarFill {
        path: path(&["source", "enabled"]),
        value: ScalarValue::from_json(&serde_json::from_str::<Box<RawValue>>("true")?)?,
    });
    let concrete = snapshot.specialize(&options)?;
    assert!(concrete.unresolved.is_empty());
    assert!(concrete.value.get().contains(exact_number));

    options.fills.clear();
    assert_eq!(snapshot.specialize(&options)?.unresolved.len(), 3);
    Ok(())
}

#[test]
fn failures_and_debug_never_contain_runtime_values_or_snapshot_source() -> TestResult {
    let (_directory, snapshot) = fixture()?;
    for value in [r#""x""#, r#""xy""#, "true", "13.25"] {
        let scalar = ScalarValue::from_json(&serde_json::from_str::<Box<RawValue>>(value)?)?;
        assert_eq!(format!("{scalar:?}"), "ScalarValue(<redacted>)");
        let options = SpecializationOptions {
            projection: path(&["config"]),
            deferred: deferred(),
            fills: vec![ScalarFill {
                path: path(&["source", "id"]),
                value: scalar,
            }],
            ..SpecializationOptions::default()
        };
        let error = snapshot
            .specialize(&options)
            .err()
            .ok_or("expected failure")?;
        assert!(
            error
                .to_string()
                .contains("CUE operation failed validation")
        );
        assert!(!error.to_string().contains("conflicting values"));
        assert!(!format!("{error:?}").contains("sentinel-secret"));
    }
    assert_eq!(format!("{snapshot:?}"), "FrozenInstance { .. }");
    for invalid in ["null", "[]", "{}", "NaN", "1 + 2", "1 true"] {
        assert!(ScalarValue::number(invalid).is_err());
    }
    Ok(())
}

#[test]
fn cloned_snapshots_can_specialize_concurrently() -> TestResult {
    let (_directory, snapshot) = fixture()?;
    std::thread::scope(|scope| -> TestResult {
        let mut workers = Vec::new();
        for _ in 0..4 {
            let snapshot = snapshot.clone();
            workers.push(scope.spawn(move || {
                snapshot.specialize(&SpecializationOptions {
                    projection: path(&["config"]),
                    deferred: deferred(),
                    ..SpecializationOptions::default()
                })
            }));
        }
        for worker in workers {
            let result = worker
                .join()
                .map_err(|_| "specialization worker panicked")??;
            assert_eq!(result.unresolved.len(), 3);
        }
        Ok(())
    })
}

#[test]
fn bootstrap_scope_cannot_authorize_an_independent_global_predicate() -> TestResult {
    let (directory, _) = fixture()?;
    let file = directory.path().join("env.cue");
    let original = fs::read_to_string(&file)?;
    fs::write(file, format!("{original}\n_check: source.id & !=\"bad\"\n"))?;
    let snapshot = FrozenInstance::load(&FreezeOptions {
        module_root: directory.path().to_owned(),
        directory: directory.path().to_owned(),
        package: "app".to_owned(),
    })?;
    let mut inspection = FrozenEvaluationOptions {
        reference_paths: vec![path(&["config"])],
        source_paths: Some(Vec::new()),
        ..FrozenEvaluationOptions::default()
    };
    assert_eq!(snapshot.evaluate(&inspection)?.references.len(), 3);
    inspection.source_paths = Some(vec![path(&["source", "id"])]);
    assert!(snapshot.evaluate(&inspection).is_err());
    assert!(
        snapshot
            .validate(&FrozenValidationOptions {
                concrete_paths: vec![path(&["config"])],
                deferred: deferred(),
                source_paths: inspection.source_paths.clone(),
                ..FrozenValidationOptions::default()
            })
            .is_err()
    );
    assert!(
        snapshot
            .specialize(&SpecializationOptions {
                projection: path(&["config"]),
                deferred: deferred(),
                reference_paths: inspection.reference_paths,
                source_paths: inspection.source_paths,
                ..SpecializationOptions::default()
            })
            .is_err()
    );
    Ok(())
}

#[test]
fn prepared_capability_preserves_fresh_validation_and_private_ownership() -> TestResult {
    let (directory, snapshot) = fixture()?;
    let prepared = snapshot.prepare(&PreparationOptions {
        reference_paths: vec![path(&["config"])],
        projections: vec![path(&["config"])],
        ..PreparationOptions::default()
    })?;
    directory.close()?;
    drop(snapshot);
    assert_eq!(format!("{prepared:?}"), "PreparedInstance { .. }");
    let mut options = PreparedSpecializationOptions {
        projection: path(&["config"]),
        deferred: deferred(),
        ..PreparedSpecializationOptions::default()
    };
    assert_eq!(prepared.specialize(&options)?.unresolved.len(), 3);
    options.fills.push(ScalarFill {
        path: path(&["source", "id"]),
        value: ScalarValue::string("ok")?,
    });
    assert_eq!(prepared.specialize(&options)?.unresolved.len(), 2);
    options.fills[0].value = ScalarValue::string("sentinel-private-fill")?;
    let error = prepared
        .specialize(&options)
        .err()
        .ok_or("expected failure")?;
    assert!(!format!("{error:?}").contains("sentinel-private-fill"));
    options.fills.clear();
    assert_eq!(prepared.specialize(&options)?.unresolved.len(), 3);
    options.projection = path(&["config", "items"]);
    assert!(prepared.specialize(&options).is_err());
    let options = PreparedSpecializationOptions {
        projection: path(&["config"]),
        deferred: deferred(),
        ..PreparedSpecializationOptions::default()
    };
    std::thread::scope(|scope| -> TestResult {
        let mut workers = Vec::new();
        for _ in 0..4 {
            let prepared = prepared.clone();
            let options = options.clone();
            workers.push(scope.spawn(move || prepared.specialize(&options)));
        }
        for worker in workers {
            assert_eq!(
                worker
                    .join()
                    .map_err(|_| "prepared worker panicked")??
                    .unresolved
                    .len(),
                3
            );
        }
        Ok(())
    })
}
