//! Immutable CUE configuration and native output-reference provenance.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cuengine::frozen::{
    DeferredValue, FreezeOptions, FrozenEvaluationOptions, FrozenInstance, FrozenReference,
    FrozenValidationOptions, PreparationOptions, PreparedInstance, PreparedSpecializationOptions,
    ScalarFill, ScalarValue, ValuePath, ValueSelector, VerificationOptions,
};
use cuenv_core::ModuleEvaluation;
use cuenv_core::cue::discovery::compute_relative_path;
use cuenv_core::module::DirectReference;
use cuenv_infrastructure::references::{
    ConfigurationEvaluator, ConfigurationSelector, ConfigurationSpecialization,
    ConfigurationVerification, NativeReference, OutputIdentity, SourceAddress,
    SpecializedConfiguration,
};
use cuenv_infrastructure::type_system::{parse_json_preserving_numbers, scan_json_numbers};
use cuenv_infrastructure::{InfrastructureError, ResourceAddress};
use serde_json::Value as Json;

use super::output_selectors::{OutputSelectors, diagnostic_path};
use crate::cli::CliError;

pub(super) struct SnapshotRequest<'request> {
    pub(super) directory: &'request Path,
    pub(super) package: &'request str,
    pub(super) environment: Option<&'request str>,
}

/// The original source graph and reference provenance for one run.
#[derive(Debug)]
pub(super) struct NativeConfigurationEvaluator {
    snapshot: PreparedInstance,
    root: ValuePath,
    references: Vec<NativeReference>,
    numeric_references: BTreeSet<NativeReference>,
    timeout: Duration,
    /// Every selected output, referenced or not.
    declared_outputs: Vec<OutputIdentity>,
    /// Selected configuration compared with and without real outputs.
    verification_roots: Vec<ValuePath>,
    /// Output namespaces and unselected configuration, which real outputs may change.
    verification_mutable: Vec<ValuePath>,
    /// Leaves that must carry exactly the output they reference.
    verification_references: Vec<DeferredValue>,
}

/// Replace preliminary configuration with an immutable CUE instance.
pub(super) fn freeze_configuration(
    module: &mut ModuleEvaluation,
    request: &SnapshotRequest<'_>,
) -> Result<Option<Arc<NativeConfigurationEvaluator>>, CliError> {
    let snapshot = FrozenInstance::load(&FreezeOptions {
        module_root: module.root.clone(),
        directory: request.directory.to_path_buf(),
        package: request.package.to_string(),
    })
    .map_err(|error| {
        frozen_failure(&error, "the CUE infrastructure package could not be frozen")
    })?;
    let root = selected_root(request.environment);
    let mut reference_paths: Vec<_> = ["providers", "resources", "data", "providerEnvironment"]
        .into_iter()
        .map(|namespace| append_field(&root, namespace))
        .collect();
    // These fields affect identity, credentials or provider inputs before any
    // resource output exists. A narrowed CUE value there must not hide an
    // unsupported dependency on a runtime output.
    reference_paths.extend(
        ["name", "env", "cache"]
            .into_iter()
            .map(|field| vec![field_selector(field)]),
    );
    reference_paths.extend(
        ["state", "migrateStateTo", "workflows"]
            .into_iter()
            .map(|field| vec![field_selector("infrastructure"), field_selector(field)]),
    );
    let export_paths: Vec<_> = ["name", "env", "infrastructure", "cache"]
        .into_iter()
        .map(|field| vec![field_selector(field)])
        .collect();
    // Find all output declarations from the same snapshot, including inactive
    // environments. This bootstrap is only used for names, never provider
    // input, and deliberately keeps every number as its original text.
    let bootstrap = snapshot
        .evaluate(&FrozenEvaluationOptions {
            export_paths: export_paths.clone(),
            reference_paths: Vec::new(),
            source_paths: Some(Vec::new()),
        })
        .map_err(|error| {
            frozen_failure(
                &error,
                "the frozen CUE infrastructure could not be bootstrapped",
            )
        })?;
    let bootstrap_number_paths: Vec<_> = scan_json_numbers(&bootstrap.value)
        .map_err(|_| CliError::config("the frozen CUE configuration contains an invalid number"))?
        .into_iter()
        .map(|token| token.path)
        .collect();
    let bootstrap_value = parse_json_preserving_numbers(&bootstrap.value, &bootstrap_number_paths)
        .map_err(|_| CliError::config("the frozen CUE configuration could not be inspected"))?;
    let source_paths = all_output_paths(&bootstrap_value);
    let inspected = snapshot
        .evaluate(&FrozenEvaluationOptions {
            export_paths,
            reference_paths: reference_paths.clone(),
            source_paths: Some(source_paths.clone()),
        })
        .map_err(|error| {
            inspection_failure(
                &error,
                "the frozen CUE infrastructure could not be inspected",
            )
        })?;
    let relative = compute_relative_path(request.directory, &module.root);
    let direct = inspected.references.iter().map(direct_reference).collect();
    let reference_map = HashMap::from([(relative.clone(), direct)]);
    let selectors = OutputSelectors {
        instance_path: &relative,
        references: &reference_map,
        environment: request.environment,
    };
    let selected: HashSet<_> = selectors
        .selected_references()?
        .into_iter()
        .map(|reference| (reference.path.as_str(), reference.target.as_slice()))
        .collect();
    let mut preserved_numbers = BTreeSet::new();
    for (reference, direct) in inspected.references.iter().zip(&reference_map[&relative]) {
        if (reference.primitive_type == "number" || reference.computed)
            && selected.contains(&(direct.path.as_str(), direct.target.as_slice()))
        {
            preserved_numbers.insert(configuration_path(&reference.path)?);
        }
    }
    for token in scan_json_numbers(&inspected.value)
        .map_err(|_| CliError::config("the frozen CUE configuration contains an invalid number"))?
    {
        if unselected_number_path(&token.path, request.environment) {
            preserved_numbers.insert(token.path);
        } else if !preserved_numbers.contains(&token.path)
            && token.number.as_json_number().is_none()
        {
            return Err(CliError::config(format!(
                "{}: the CUE number cannot be represented without losing precision",
                display_configuration_path(&token.path)
            )));
        }
    }
    let preserved_numbers: Vec<_> = preserved_numbers.into_iter().collect();
    let value =
        parse_json_preserving_numbers(&inspected.value, &preserved_numbers).map_err(|_| {
            CliError::config(
                "the frozen CUE configuration could not be decoded without losing precision",
            )
        })?;
    let mut references = Vec::new();
    let mut numeric_references = BTreeSet::new();
    let mut deferred = Vec::new();
    for (reference, direct) in inspected.references.iter().zip(&reference_map[&relative]) {
        if selected.contains(&(direct.path.as_str(), direct.target.as_slice())) {
            let native = native_reference(reference, &value, &root)?;
            if reference.primitive_type == "number" || reference.computed {
                numeric_references.insert(native.clone());
            }
            references.push(native);
            deferred.push(DeferredValue {
                path: reference.path.clone(),
                target: reference.target.clone(),
            });
        }
    }
    let mut concrete_paths = vec![vec![
        field_selector("infrastructure"),
        field_selector("state"),
    ]];
    if request.environment.is_some() {
        concrete_paths.push(root.clone());
    } else {
        for field in ["providers", "resources", "data"] {
            let path = append_field(&root, field);
            if json_at(&value, &path).is_some() {
                concrete_paths.push(path);
            }
        }
    }
    let mut declared_outputs = Vec::new();
    for namespace in output_namespaces(&value, &root) {
        let source = address(
            &value,
            &root,
            (
                path_field(&namespace, root.len())?,
                path_field(&namespace, root.len() + 1)?,
            ),
        )?;
        if let Some(outputs) = json_at(&value, &namespace).and_then(Json::as_object) {
            declared_outputs.extend(outputs.keys().map(|attribute| OutputIdentity {
                source: source.clone(),
                attribute: attribute.clone(),
            }));
        }
    }
    let mut verification_mutable = output_namespaces(&value, &root);
    if request.environment.is_none() {
        verification_mutable.push(append_field(&root, "environments"));
    }
    // A carried value must equal its output. CUE decides what a computed one is.
    let mut verification_references = Vec::new();
    for (reference, entry) in inspected
        .references
        .iter()
        .filter(|reference| deferred.iter().any(|entry| reference_is(reference, entry)))
        .zip(&deferred)
    {
        if reference.computed {
            verification_mutable.push(entry.path.clone());
        } else {
            verification_references.push(entry.clone());
        }
    }
    let verification_roots = vec![root.clone()];
    snapshot
        .validate(&FrozenValidationOptions {
            concrete_paths,
            abstract_paths: output_namespaces(&value, &root),
            deferred,
            source_paths: Some(source_paths.clone()),
        })
        .map_err(|error| frozen_failure(&error, "the selected CUE infrastructure configuration is incomplete or violates its constraints"))?;
    let instance = module
        .instances
        .get_mut(Path::new(&relative))
        .ok_or_else(|| CliError::config("the infrastructure package could not be evaluated"))?;
    instance.value = value;
    module.direct_references.insert(
        relative,
        reference_map.into_values().next().unwrap_or_default(),
    );
    if references.is_empty() && declared_outputs.is_empty() {
        return Ok(None);
    }
    let mut projections = Vec::new();
    for reference in &references {
        let projection = append_field(
            &declaration_path(&root, &reference.consumer),
            "configuration",
        );
        if !projections.contains(&projection) {
            projections.push(projection);
        }
    }
    let snapshot = snapshot
        .prepare(&PreparationOptions {
            reference_paths,
            source_paths: Some(source_paths),
            projections,
        })
        .map_err(|error| {
            inspection_failure(&error, "the frozen CUE references could not be prepared")
        })?;
    let timeout = cuengine::evaluation_timeout()
        .map_err(|_| CliError::config("CUENV_EVAL_TIMEOUT must be a positive number of seconds"))?;
    Ok(Some(Arc::new(NativeConfigurationEvaluator {
        snapshot,
        root,
        references,
        numeric_references,
        timeout,
        declared_outputs,
        verification_roots,
        verification_mutable,
        verification_references,
    })))
}

fn frozen_failure(error: &cuengine::CueEngineError, message: &'static str) -> CliError {
    match error {
        cuengine::CueEngineError::FrozenWorker { reason } => {
            CliError::eval(format!("{message}: CUE evaluation {reason}"))
        }
        _ => CliError::eval(message),
    }
}

fn inspection_failure(error: &cuengine::CueEngineError, message: &'static str) -> CliError {
    match error {
        cuengine::CueEngineError::FrozenWorker { .. } => frozen_failure(error, message),
        _ => CliError::eval_with_help(message, OUTPUT_REFERENCE_HELP),
    }
}

const OUTPUT_REFERENCE_HELP: &str = "A scalar configuration value may read, or be computed from, \
runtime outputs. What fields exist may not depend on one: conditions, labels, indexes and defaults \
cannot read an output. Diagnostics are withheld because they can contain private configuration \
values or keys.";

fn native_reference(
    reference: &FrozenReference,
    project: &Json,
    root: &ValuePath,
) -> Result<NativeReference, CliError> {
    let root_len = root.len();
    let source = address(
        project,
        root,
        (
            path_field(&reference.target, root_len)?,
            path_field(&reference.target, root_len + 1)?,
        ),
    )?;
    let consumer = address(
        project,
        root,
        (
            path_field(&reference.path, root_len)?,
            path_field(&reference.path, root_len + 1)?,
        ),
    )?;
    let attribute = path_field(&reference.target, root_len + 3)?.to_string();
    let destination = configuration_path(&reference.path[root_len + 3..])?;
    Ok(NativeReference {
        consumer,
        destination,
        source,
        attribute,
        computed: reference.computed,
    })
}

fn reference_is(reference: &FrozenReference, entry: &DeferredValue) -> bool {
    reference.path == entry.path && reference.target == entry.target
}

fn configuration_path(path: &[ValueSelector]) -> Result<Vec<ConfigurationSelector>, CliError> {
    path.iter()
        .map(|selector| match selector {
            ValueSelector::Field(name) => Ok(ConfigurationSelector::Field(name.clone())),
            ValueSelector::Index(index) => i64::try_from(*index)
                .map(ConfigurationSelector::Index)
                .map_err(|_| CliError::config("an output selector list index is too large")),
        })
        .collect()
}

fn unselected_number_path(path: &[ConfigurationSelector], environment: Option<&str>) -> bool {
    use ConfigurationSelector::Field;
    match path {
        [Field(root), Field(environments), Field(name), ..]
            if root == "infrastructure" && environments == "environments" =>
        {
            environment != Some(name.as_str())
        }
        [Field(root), Field(namespace), ..]
            if root == "infrastructure"
                && matches!(namespace.as_str(), "providers" | "resources" | "data") =>
        {
            environment.is_some()
        }
        [Field(root), Field(environments), Field(name), ..]
            if root == "env" && environments == "environment" =>
        {
            environment != Some(name.as_str())
        }
        _ => false,
    }
}

fn display_configuration_path(path: &[ConfigurationSelector]) -> String {
    let labels: Vec<_> = path
        .iter()
        .map(|selector| match selector {
            ConfigurationSelector::Field(field) => field.clone(),
            ConfigurationSelector::Index(_) => String::new(),
        })
        .collect();
    diagnostic_path(&labels)
}

fn path_field(path: &ValuePath, index: usize) -> Result<&str, CliError> {
    match path.get(index) {
        Some(ValueSelector::Field(value)) => Ok(value),
        _ => Err(CliError::config("an output selector uses an invalid path")),
    }
}

fn address(
    project: &Json,
    root: &ValuePath,
    (namespace, name): (&str, &str),
) -> Result<SourceAddress, CliError> {
    let mut path = append_field(root, namespace);
    path.push(field_selector(name));
    let resource_type = json_at(project, &path)
        .and_then(|declaration| declaration.get("type"))
        .and_then(Json::as_str)
        .ok_or_else(|| {
            CliError::config(
                "an output selector names an undeclared or incomplete resource or data source",
            )
        })?;
    let address = ResourceAddress::parse(&format!("{resource_type}.{name}")).ok_or_else(|| {
        CliError::config("an output selector names an invalid resource or data source address")
    })?;
    match namespace {
        "resources" => Ok(SourceAddress::Resource(address)),
        "data" => Ok(SourceAddress::DataSource(address)),
        _ => Err(CliError::config(
            "an output selector names an invalid namespace",
        )),
    }
}

fn output_namespaces(project: &Json, root: &ValuePath) -> Vec<ValuePath> {
    let mut paths = Vec::new();
    for namespace in ["resources", "data"] {
        let namespace_path = append_field(root, namespace);
        if let Some(declarations) = json_at(project, &namespace_path).and_then(Json::as_object) {
            for (name, declaration) in declarations {
                if declaration.get("outputs").is_some() {
                    let mut path = append_field(&namespace_path, name);
                    path.push(field_selector("outputs"));
                    paths.push(path);
                }
            }
        }
    }
    paths
}

/// Every output field, including unselected and invalid declarations, is a
/// potential dependency when referenced from a selected or global expression.
fn all_output_paths(project: &Json) -> Vec<ValuePath> {
    let mut paths = Vec::new();
    for root in infrastructure_roots(project) {
        for namespace in output_namespaces(project, &root) {
            if let Some(outputs) = json_at(project, &namespace).and_then(Json::as_object) {
                paths.extend(outputs.keys().map(|name| append_field(&namespace, name)));
            }
        }
    }
    paths
}

fn infrastructure_roots(project: &Json) -> Vec<ValuePath> {
    let mut roots = vec![selected_root(None)];
    if let Some(environments) = project
        .pointer("/infrastructure/environments")
        .and_then(Json::as_object)
    {
        roots.extend(environments.keys().map(|name| selected_root(Some(name))));
    }
    roots
}

fn selected_root(environment: Option<&str>) -> ValuePath {
    let mut root = vec![field_selector("infrastructure")];
    if let Some(environment) = environment {
        root.extend([field_selector("environments"), field_selector(environment)]);
    }
    root
}

fn append_field(path: &ValuePath, field: &str) -> ValuePath {
    let mut result = path.clone();
    result.push(field_selector(field));
    result
}

fn field_selector(field: &str) -> ValueSelector {
    ValueSelector::Field(field.to_string())
}

fn declaration_path(root: &ValuePath, source: &SourceAddress) -> ValuePath {
    let (namespace, address) = match source {
        SourceAddress::Resource(address) => ("resources", address),
        SourceAddress::DataSource(address) => ("data", address),
    };
    let mut path = append_field(root, namespace);
    path.push(field_selector(&address.name));
    path
}

fn json_at<'value>(value: &'value Json, path: &ValuePath) -> Option<&'value Json> {
    path.iter()
        .try_fold(value, |value, selector| match selector {
            ValueSelector::Field(field) => value.get(field),
            ValueSelector::Index(index) => value.get(*index),
        })
}

fn direct_reference(reference: &FrozenReference) -> DirectReference {
    let labels = |path: &ValuePath| {
        path.iter()
            .map(|selector| match selector {
                ValueSelector::Field(field) => field.clone(),
                ValueSelector::Index(index) => index.to_string(),
            })
            .collect::<Vec<_>>()
    };
    DirectReference {
        path: display_path(&reference.path),
        labels: labels(&reference.path),
        target: labels(&reference.target),
        primitive_type: reference.primitive_type.clone(),
        supported: reference.supported,
    }
}

fn display_path(path: &ValuePath) -> String {
    // This canonical form distinguishes quoted labels from indices when
    // matching references. It contains untrusted values and must not be
    // included in diagnostics; use `diagnostic_path` for those instead.
    let mut rendered = String::new();
    for selector in path {
        match selector {
            ValueSelector::Field(field) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push_str(&Json::String(field.clone()).to_string());
            }
            ValueSelector::Index(index) => {
                let _ = write!(rendered, "[{index}]");
            }
        }
    }
    rendered
}

fn scalar_value(value: &cuenv_infrastructure::type_system::Value) -> Result<ScalarValue, ()> {
    use cuenv_infrastructure::type_system::Value;
    match value.untyped() {
        Value::String(value) => ScalarValue::string(value),
        Value::Number(value) => ScalarValue::number(value.as_str()),
        Value::Boolean(value) => {
            let raw = serde_json::value::to_raw_value(value).map_err(|_| ())?;
            ScalarValue::from_json(&raw)
        }
        _ => return Err(()),
    }
    .map_err(|_| ())
}

impl NativeConfigurationEvaluator {
    fn declaration_path(&self, source: &SourceAddress) -> ValuePath {
        declaration_path(&self.root, source)
    }

    fn source_path(&self, identity: &OutputIdentity) -> ValuePath {
        let mut path = self.declaration_path(&identity.source);
        path.extend([
            field_selector("outputs"),
            field_selector(&identity.attribute),
        ]);
        path
    }

    fn destination_path(
        &self,
        reference: &NativeReference,
    ) -> cuenv_infrastructure::Result<ValuePath> {
        let mut path = self.declaration_path(&reference.consumer);
        path.push(field_selector("configuration"));
        for selector in &reference.destination {
            path.push(match selector {
                ConfigurationSelector::Field(field) => field_selector(field),
                ConfigurationSelector::Index(index) => {
                    ValueSelector::Index(usize::try_from(*index).map_err(|_| {
                        InfrastructureError::configuration("invalid native configuration index")
                    })?)
                }
            });
        }
        Ok(path)
    }
}

impl ConfigurationEvaluator for NativeConfigurationEvaluator {
    fn references(&self) -> &[NativeReference] {
        &self.references
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn declared_outputs(&self) -> &[OutputIdentity] {
        &self.declared_outputs
    }

    fn verify(&self, input: &ConfigurationVerification<'_>) -> cuenv_infrastructure::Result<()> {
        let invalid = || InfrastructureError::configuration("an output value is not a scalar");
        let mut fills = Vec::new();
        for (identity, value) in input.sources {
            if self.declared_outputs.contains(identity) {
                fills.push((
                    identity,
                    ScalarFill {
                        path: self.source_path(identity),
                        value: scalar_value(value).map_err(|()| invalid())?,
                    },
                ));
            }
        }
        let check = |values: Vec<ScalarFill>| {
            self.snapshot.verify(&VerificationOptions {
                fills: values,
                roots: self.verification_roots.clone(),
                mutable: self.verification_mutable.clone(),
                references: self.verification_references.clone(),
            })
        };
        let worker = || InfrastructureError::ConfigurationEvaluation {
            consumer: "the CUE configuration".to_string(),
            reason: "could not acquire or complete a CUE worker",
        };
        match check(fills.iter().map(|(_, fill)| fill.clone()).collect()) {
            Ok(()) => return Ok(()),
            Err(cuengine::CueEngineError::FrozenWorker { .. }) => return Err(worker()),
            Err(_) => {}
        }
        // Name the output, never its value. Only a failed run pays for this.
        let mut subject = "real output values".to_string();
        for (identity, fill) in &fills {
            match check(vec![fill.clone()]) {
                Ok(()) => {}
                Err(cuengine::CueEngineError::FrozenWorker { .. }) => return Err(worker()),
                Err(_) => {
                    subject = format!(
                        "the real value of output {}.{}",
                        identity.source, identity.attribute
                    );
                    break;
                }
            }
        }
        Err(InfrastructureError::configuration(format!(
            "{subject} changed this CUE configuration outside its declared output references or \
             violated its constraints"
        )))
    }

    fn specialize(
        &self,
        input: &ConfigurationSpecialization<'_>,
    ) -> cuenv_infrastructure::Result<SpecializedConfiguration> {
        let failure = || {
            InfrastructureError::configuration(format!(
                "the CUE configuration for {} could not be specialized or violates its constraints",
                input.consumer
            ))
        };
        let mut identities = BTreeSet::new();
        let mut fills = Vec::new();
        for reference in self
            .references
            .iter()
            .filter(|reference| &reference.consumer == input.consumer)
        {
            let identity = OutputIdentity {
                source: reference.source.clone(),
                attribute: reference.attribute.clone(),
            };
            if identities.insert(identity.clone())
                && let Some(value) = input.sources.get(&identity)
            {
                let scalar = scalar_value(value).map_err(|()| failure())?;
                fills.push(ScalarFill {
                    path: self.source_path(&identity),
                    value: scalar,
                });
            }
        }
        let mut deferred = Vec::new();
        for reference in input.deferred {
            if &reference.consumer != input.consumer || !self.references.contains(reference) {
                return Err(failure());
            }
            deferred.push(DeferredValue {
                path: self.destination_path(reference)?,
                target: self.source_path(&OutputIdentity {
                    source: reference.source.clone(),
                    attribute: reference.attribute.clone(),
                }),
            });
        }
        let projection = append_field(&self.declaration_path(input.consumer), "configuration");
        let specialized = self
            .snapshot
            .specialize(&PreparedSpecializationOptions {
                fills,
                projection,
                deferred: deferred.clone(),
            })
            .map_err(|error| match error {
                cuengine::CueEngineError::FrozenWorker { .. } => {
                    InfrastructureError::ConfigurationEvaluation {
                        consumer: input.consumer.to_string(),
                        reason: "could not acquire or complete a CUE worker",
                    }
                }
                _ => failure(),
            })?;
        if specialized.unresolved.len() != deferred.len()
            || !deferred
                .iter()
                .all(|value| specialized.unresolved.contains(value))
        {
            return Err(failure());
        }
        let numeric_paths: Vec<_> = self
            .numeric_references
            .iter()
            .filter(|reference| &reference.consumer == input.consumer)
            .map(|reference| reference.destination.clone())
            .collect();
        let Json::Object(configuration) =
            parse_json_preserving_numbers(&specialized.value, &numeric_paths)
                .map_err(|_| failure())?
        else {
            return Err(failure());
        };
        Ok(SpecializedConfiguration {
            configuration,
            unresolved: input.deferred.to_vec(),
        })
    }
}
