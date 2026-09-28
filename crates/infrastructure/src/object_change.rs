//! Port of Terraform core's `internal/plans/objchange`: the proposed new
//! state handed to `PlanResourceChange`, and the checks that the provider's
//! planned state is one Terraform would accept.
//!
//! Both walk the resource schema recursively through attributes, nested
//! attributes (protocol 6) and nested blocks in every nesting mode, as
//! Terraform does:
//!
//! - [`proposed_new`] mirrors `ProposedNew`: configuration values, with
//!   computed attributes the configuration leaves null carried over from the
//!   prior state. Collections are correlated with prior elements by index
//!   (lists), key (maps) or by which prior element could have come from the
//!   configured one (sets).
//! - [`plan_problems`] mirrors `AssertPlanValid`: a provider may change only
//!   computed attributes that the configuration leaves null, and may return
//!   the prior value in place of a configured one it considers equivalent.
//!
//! Problems name attribute paths but never values, which may be secrets.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use crate::schema::{Attribute, Block, NestedAttributes, NestedBlock, Nesting, Presence};
use crate::type_system::{PathStep, Type, Value, raw_equal};

static NULL_VALUE: Value = Value::Null;
static UNKNOWN_VALUE: Value = Value::Unknown;

/// The value of attribute `name` in an object: null in a null (or
/// non-object) value, unknown in an unknown one.
fn member<'value>(value: &'value Value, name: &str) -> &'value Value {
    match value {
        Value::Object(attributes) => attributes.get(name).unwrap_or(&NULL_VALUE),
        Value::Unknown => &UNKNOWN_VALUE,
        _ => &NULL_VALUE,
    }
}

/// The element at `index` of a list value, null when absent.
fn element(value: &Value, index: usize) -> &Value {
    match value {
        Value::List(elements) => elements.get(index).unwrap_or(&NULL_VALUE),
        Value::Unknown => &UNKNOWN_VALUE,
        _ => &NULL_VALUE,
    }
}

/// The schema of one element of a nested collection.
#[derive(Debug, Clone, Copy)]
enum ElementSchema<'schema> {
    /// An element of a nested block collection.
    Block(&'schema Block),
    /// An element of a nested attribute collection.
    Object(&'schema BTreeMap<String, Attribute>),
}

impl ElementSchema<'_> {
    fn element_type(self) -> Type {
        match self {
            Self::Block(block) => block.implied_type(),
            Self::Object(attributes) => NestedAttributes::object_type_of(attributes),
        }
    }
}

// ---------------------------------------------------------------------------
// Proposed new state
// ---------------------------------------------------------------------------

/// Terraform's `ProposedNew` for a resource block.
#[must_use]
pub fn proposed_new(block: &Block, prior: &Value, configuration: &Value) -> Value {
    if configuration.is_null() && prior.is_null() {
        return Value::Null;
    }
    if prior.is_null() {
        // A synthetic prior that looks like an empty configuration block
        // gives the attributes below one non-null level to read from.
        return proposed_new_block(block, &block.empty_value(), configuration);
    }
    proposed_new_block(block, prior, configuration)
}

fn proposed_new_block(block: &Block, prior: &Value, configuration: &Value) -> Value {
    if configuration.is_null() || matches!(configuration, Value::Unknown) {
        return prior.clone();
    }
    let mut attributes = proposed_new_attributes(&block.attributes, prior, configuration);
    for (name, nested) in &block.blocks {
        let value =
            proposed_new_nested_block(nested, member(prior, name), member(configuration, name));
        attributes.insert(name.clone(), value);
    }
    Value::Object(attributes)
}

fn proposed_new_nested_block(nested: &NestedBlock, prior: &Value, configuration: &Value) -> Value {
    if matches!(configuration, Value::Unknown) {
        return configuration.clone();
    }
    let schema = ElementSchema::Block(&nested.block);
    match nested.nesting {
        Nesting::Single if configuration.is_null() => configuration.clone(),
        Nesting::Single | Nesting::Group => proposed_new(&nested.block, prior, configuration),
        Nesting::List => proposed_new_list(schema, prior, configuration),
        Nesting::Map => proposed_new_map(schema, prior, configuration),
        Nesting::Set => proposed_new_set(schema, prior, configuration),
    }
}

fn proposed_new_nested_attributes(
    nested: &NestedAttributes,
    prior: &Value,
    configuration: &Value,
) -> Value {
    if matches!(configuration, Value::Unknown) {
        return configuration.clone();
    }
    let schema = ElementSchema::Object(&nested.attributes);
    match nested.nesting {
        Nesting::Single | Nesting::Group => proposed_new_element(schema, prior, configuration),
        Nesting::List => proposed_new_list(schema, prior, configuration),
        Nesting::Map => proposed_new_map(schema, prior, configuration),
        Nesting::Set => proposed_new_set(schema, prior, configuration),
    }
}

fn proposed_new_element(schema: ElementSchema<'_>, prior: &Value, configuration: &Value) -> Value {
    match schema {
        ElementSchema::Block(block) => proposed_new(block, prior, configuration),
        ElementSchema::Object(_) if configuration.is_null() => configuration.clone(),
        ElementSchema::Object(attributes) => {
            Value::Object(proposed_new_attributes(attributes, prior, configuration))
        }
    }
}

fn proposed_new_attributes(
    attributes: &BTreeMap<String, Attribute>,
    prior: &Value,
    configuration: &Value,
) -> BTreeMap<String, Value> {
    attributes
        .iter()
        .map(|(name, attribute)| {
            let prior_value = member(prior, name);
            let configured = member(configuration, name);
            let value = if attribute.presence.is_computed() && configured.is_null() {
                // An optional attribute whose prior value holds anything the
                // provider cannot compute must have been configured before;
                // it was removed, so propose the (null) configuration.
                if optional_value_not_computable(attribute, prior_value) {
                    configured.clone()
                } else {
                    prior_value.clone()
                }
            } else if let Some(nested) = &attribute.nested {
                proposed_new_nested_attributes(nested, prior_value, configured)
            } else {
                configured.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

fn proposed_new_list(schema: ElementSchema<'_>, prior: &Value, configuration: &Value) -> Value {
    let Value::List(configured) = configuration else {
        return configuration.clone();
    };
    if configured.is_empty() {
        return configuration.clone();
    }
    let prior_known = !matches!(prior, Value::Unknown);
    Value::List(
        configured
            .iter()
            .enumerate()
            .map(|(index, configured_element)| {
                let prior_element = element(prior, index);
                if prior_known && prior_element.is_null() {
                    configured_element.clone()
                } else {
                    proposed_new_element(schema, prior_element, configured_element)
                }
            })
            .collect(),
    )
}

fn proposed_new_map(schema: ElementSchema<'_>, prior: &Value, configuration: &Value) -> Value {
    let Value::Object(configured) = configuration else {
        return configuration.clone();
    };
    if configured.is_empty() {
        return configuration.clone();
    }
    Value::Object(
        configured
            .iter()
            .map(|(key, configured_element)| {
                let prior_element = member(prior, key);
                (
                    key.clone(),
                    proposed_new_element(schema, prior_element, configured_element),
                )
            })
            .collect(),
    )
}

fn proposed_new_set(schema: ElementSchema<'_>, prior: &Value, configuration: &Value) -> Value {
    let Value::List(configured) = configuration else {
        return configuration.clone();
    };
    if configured.is_empty() {
        return configuration.clone();
    }
    let prior_elements: &[Value] = match prior {
        Value::List(elements) => elements,
        _ => &[],
    };
    let mut used = vec![false; prior_elements.len()];
    Value::List(
        configured
            .iter()
            .map(|configured_element| {
                // The first unused prior element that could have come from
                // this configured one; set elements have no other identity.
                let matched = prior_elements
                    .iter()
                    .enumerate()
                    .find(|(index, candidate)| {
                        !used[*index]
                            && valid_prior_from_configuration(schema, candidate, configured_element)
                    });
                let prior_element = matched.map_or(&NULL_VALUE, |(index, candidate)| {
                    used[index] = true;
                    candidate
                });
                proposed_new_element(schema, prior_element, configured_element)
            })
            .collect(),
    )
}

/// Terraform's `optionalValueNotComputable`: an optional nested attribute
/// whose prior value holds a non-null attribute the provider cannot
/// compute must have been set by configuration.
fn optional_value_not_computable(attribute: &Attribute, value: &Value) -> bool {
    if !matches!(
        attribute.presence,
        Presence::Optional | Presence::OptionalComputed
    ) {
        return false;
    }
    attribute
        .nested
        .as_ref()
        .is_some_and(|nested| holds_non_computed(nested, value))
}

/// Terraform walks the value and asks `AttributeByPath` for each non-null
/// part; that finds only leaf attributes, so a nested attribute counts
/// through its children alone, never by being set itself.
fn holds_non_computed(nested: &NestedAttributes, value: &Value) -> bool {
    let object_holds = |object: &Value| {
        nested.attributes.iter().any(|(name, attribute)| {
            let member_value = member(object, name);
            if member_value.is_null() {
                return false;
            }
            match &attribute.nested {
                Some(inner) => holds_non_computed(inner, member_value),
                None => !attribute.presence.is_computed(),
            }
        })
    };
    match (nested.nesting, value) {
        (Nesting::Single | Nesting::Group, object @ Value::Object(_)) => object_holds(object),
        (Nesting::List | Nesting::Set, Value::List(elements)) => elements.iter().any(object_holds),
        (Nesting::Map, Value::Object(entries)) => entries.values().any(object_holds),
        _ => false,
    }
}

/// Terraform's `validPriorFromConfig`: whether a prior set element could
/// have been derived from a configured one, differing only in computed
/// attributes the configuration leaves null.
fn valid_prior_from_configuration(
    schema: ElementSchema<'_>,
    prior: &Value,
    configuration: &Value,
) -> bool {
    raw_equal(configuration, prior, &schema.element_type())
        || object_derives_from(schema, prior, configuration)
}

fn object_derives_from(schema: ElementSchema<'_>, prior: &Value, configuration: &Value) -> bool {
    let Value::Object(prior_attributes) = prior else {
        // Null and unknown values have nothing further to compare.
        return true;
    };
    let Value::Object(configured_attributes) = configuration else {
        return prior_attributes.is_empty();
    };
    prior_attributes.iter().all(|(name, prior_value)| {
        let configured = configured_attributes.get(name).unwrap_or(&NULL_VALUE);
        match schema {
            ElementSchema::Object(attributes) => attributes
                .get(name)
                .is_none_or(|attribute| attribute_derives_from(attribute, prior_value, configured)),
            ElementSchema::Block(block) => {
                match (block.attributes.get(name), block.blocks.get(name)) {
                    (Some(attribute), _) => {
                        attribute_derives_from(attribute, prior_value, configured)
                    }
                    (None, Some(nested)) => block_derives_from(nested, prior_value, configured),
                    (None, None) => true,
                }
            }
        }
    })
}

fn attribute_derives_from(attribute: &Attribute, prior: &Value, configuration: &Value) -> bool {
    if raw_equal(configuration, prior, &attribute.value_type) {
        return true;
    }
    // Nested sets cannot be correlated, so they must be equal.
    if matches!(attribute.value_type, Type::Set(_)) {
        return false;
    }
    match &attribute.nested {
        Some(nested) => collection_derives_from(
            Collection {
                schema: ElementSchema::Object(&nested.attributes),
                nesting: nested.nesting,
            },
            prior,
            configuration,
        ),
        // A leaf may differ only when the provider computes it and the
        // configuration leaves it null.
        None => attribute.presence.is_computed() && configuration.is_null(),
    }
}

fn block_derives_from(nested: &NestedBlock, prior: &Value, configuration: &Value) -> bool {
    if raw_equal(configuration, prior, &nested.implied_type()) {
        return true;
    }
    if nested.nesting == Nesting::Set {
        return false;
    }
    collection_derives_from(
        Collection {
            schema: ElementSchema::Block(&nested.block),
            nesting: nested.nesting,
        },
        prior,
        configuration,
    )
}

/// A nested collection's element schema and nesting mode.
#[derive(Debug, Clone, Copy)]
struct Collection<'schema> {
    schema: ElementSchema<'schema>,
    nesting: Nesting,
}

fn collection_derives_from(
    collection: Collection<'_>,
    prior: &Value,
    configuration: &Value,
) -> bool {
    let Collection { schema, nesting } = collection;
    let element_type = schema.element_type();
    let element_derives = |prior_element: &Value, configured: Option<&Value>| {
        configured.is_some_and(|configured| {
            raw_equal(configured, prior_element, &element_type)
                || object_derives_from(schema, prior_element, configured)
        })
    };
    match (nesting, prior) {
        (Nesting::Single | Nesting::Group, _) => object_derives_from(schema, prior, configuration),
        (Nesting::List, Value::List(prior_elements)) => {
            prior_elements
                .iter()
                .enumerate()
                .all(|(index, prior_element)| match configuration {
                    Value::List(configured) => {
                        element_derives(prior_element, configured.get(index))
                    }
                    _ => false,
                })
        }
        (Nesting::Map, Value::Object(prior_entries)) => {
            prior_entries
                .iter()
                .all(|(key, prior_element)| match configuration {
                    Value::Object(configured) => {
                        element_derives(prior_element, configured.get(key))
                    }
                    _ => false,
                })
        }
        (Nesting::Set, _) => false,
        // Null or unknown prior collections have no elements to walk.
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Plan validity
// ---------------------------------------------------------------------------

/// One reason a planned state is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanProblem {
    /// Attribute path, such as `rules[0].label`; empty for the resource.
    pub path: String,
    /// What is wrong, without any value.
    pub message: String,
}

impl fmt::Display for PlanProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

/// Terraform's `AssertPlanValid`: every way `values.planned` departs from
/// what a provider may plan for `values.configuration` given
/// `values.prior` (null for a create).
#[must_use]
pub fn plan_problems(block: &Block, values: PlanValues<'_>) -> Vec<PlanProblem> {
    let mut check = PlanCheck::default();
    check.block(block, values);
    check.problems
}

/// The prior, configured and planned values of a resource (or, during the
/// walk, of one part of it).
#[derive(Debug, Clone, Copy)]
pub struct PlanValues<'value> {
    /// Prior state; null for a create.
    pub prior: &'value Value,
    /// Configuration.
    pub configuration: &'value Value,
    /// The provider's planned state.
    pub planned: &'value Value,
}

type Values<'value> = PlanValues<'value>;

impl Values<'_> {
    fn member(self, name: &str) -> Self {
        Values {
            prior: member(self.prior, name),
            configuration: member(self.configuration, name),
            planned: member(self.planned, name),
        }
    }
}

#[derive(Debug, Default)]
struct PlanCheck {
    path: Vec<PathStep>,
    problems: Vec<PlanProblem>,
}

impl PlanCheck {
    fn problem(&mut self, message: impl Into<String>) {
        self.problems.push(PlanProblem {
            path: render_steps(&self.path),
            message: message.into(),
        });
    }

    fn within(&mut self, step: PathStep, check: impl FnOnce(&mut Self)) {
        self.path.push(step);
        check(self);
        self.path.pop();
    }

    fn block(&mut self, block: &Block, values: Values<'_>) {
        if values.planned.is_null() && !values.configuration.is_null() {
            self.problem("planned for absence but the configuration wants existence");
            return;
        }
        if values.configuration.is_null() && !values.planned.is_null() {
            self.problem("planned for existence but the configuration wants absence");
            return;
        }
        if values.planned.is_null() {
            return;
        }
        self.attributes(&block.attributes, values);
        for (name, nested) in &block.blocks {
            self.within(PathStep::Attribute(name.clone()), |check| {
                check.nested_block(nested, values.member(name));
            });
        }
    }

    fn nested_block(&mut self, nested: &NestedBlock, values: Values<'_>) {
        let Values {
            prior,
            configuration,
            planned,
        } = values;
        if nested.nesting != Nesting::Single && planned.is_null() {
            self.problem(format!(
                "attribute representing a {} of nested blocks must be empty to indicate no \
                 blocks, not null",
                nesting_name(nested.nesting)
            ));
            return;
        }
        if raw_equal(planned, configuration, &nested.implied_type()) {
            return;
        }
        if matches!(configuration, Value::Unknown) {
            self.problem("planned a value for an unknown dynamic block");
            return;
        }
        if matches!(planned, Value::Unknown) {
            self.problem("planned an unknown value for a non-computed block");
            return;
        }
        let block = &nested.block;
        match nested.nesting {
            Nesting::Single | Nesting::Group => self.block(block, values),
            Nesting::List => {
                let planned_elements = list_elements(planned);
                let configured_elements = list_elements(configuration);
                if planned_elements.len() != configured_elements.len() {
                    self.problem(format!(
                        "block count in plan ({}) disagrees with count in configuration ({})",
                        planned_elements.len(),
                        configured_elements.len()
                    ));
                    return;
                }
                for (index, (planned_element, configured_element)) in
                    planned_elements.iter().zip(configured_elements).enumerate()
                {
                    self.within(path_index(index), |check| {
                        if matches!(planned_element, Value::Unknown) {
                            check.problem(UNKNOWN_BLOCK_ELEMENT);
                        } else {
                            check.block(
                                block,
                                Values {
                                    prior: element(prior, index),
                                    configuration: configured_element,
                                    planned: planned_element,
                                },
                            );
                        }
                    });
                }
            }
            Nesting::Map => {
                let planned_entries = map_entries(planned);
                let configured_entries = map_entries(configuration);
                if planned_entries.len() != configured_entries.len() {
                    self.problem(format!(
                        "block count in plan ({}) disagrees with count in configuration ({})",
                        planned_entries.len(),
                        configured_entries.len()
                    ));
                    return;
                }
                for (&key, &planned_element) in &planned_entries {
                    let Some(&configured_element) = configured_entries.get(key) else {
                        self.problem(format!(
                            "block key {key:?} from plan is not present in configuration"
                        ));
                        continue;
                    };
                    self.within(PathStep::Key(key.clone()), |check| {
                        if matches!(planned_element, Value::Unknown) {
                            check.problem(UNKNOWN_BLOCK_ELEMENT);
                        } else {
                            check.block(
                                block,
                                Values {
                                    prior: member(prior, key),
                                    configuration: configured_element,
                                    planned: planned_element,
                                },
                            );
                        }
                    });
                }
                for key in configured_entries.keys() {
                    if !planned_entries.contains_key(key) {
                        self.problem(format!(
                            "block key {key:?} from configuration is not present in plan"
                        ));
                    }
                }
            }
            Nesting::Set => {
                // Set elements cannot be correlated, so only reject unknown
                // elements, as Terraform does.
                for (index, planned_element) in list_elements(planned).iter().enumerate() {
                    if matches!(planned_element, Value::Unknown) {
                        self.within(path_index(index), |check| {
                            check.problem(UNKNOWN_BLOCK_ELEMENT);
                        });
                    }
                }
            }
        }
    }

    fn attributes(&mut self, attributes: &BTreeMap<String, Attribute>, values: Values<'_>) {
        for (name, attribute) in attributes {
            self.within(PathStep::Attribute(name.clone()), |check| {
                check.value(attribute, values.member(name));
            });
        }
    }

    fn value(&mut self, attribute: &Attribute, values: Values<'_>) {
        let Values {
            prior,
            configuration,
            planned,
        } = values;
        let value_type = &attribute.value_type;
        if raw_equal(planned, configuration, value_type) {
            return;
        }
        // The provider returned the prior value in place of the configured
        // one: it considers them equivalent (semantic equality).
        if raw_equal(planned, prior, value_type) && !prior.is_null() && !configuration.is_null() {
            return;
        }
        match attribute.presence {
            Presence::Computed => return,
            Presence::OptionalComputed if configuration.is_null() => return,
            _ if configuration.is_null() && !planned.is_null() => {
                self.problem("planned a value for a non-computed attribute");
                return;
            }
            _ => {}
        }
        if let Some(nested) = &attribute.nested {
            self.nested_object(nested, values);
            return;
        }
        if prior.is_null() {
            self.problem("planned value does not match the configured value");
        } else {
            self.problem("planned value does not match the configured value nor the prior value");
        }
    }

    fn nested_object(&mut self, nested: &NestedAttributes, values: Values<'_>) {
        let Values {
            prior,
            configuration,
            planned,
        } = values;
        if planned.is_null() && !configuration.is_null() {
            self.problem("planned for absence but the configuration wants existence");
            return;
        }
        if configuration.is_null() && !planned.is_null() {
            self.problem("planned for existence but the configuration wants absence");
            return;
        }
        if !configuration.is_null() && matches!(planned, Value::Unknown) {
            self.problem("planned an unknown value for a configured value");
            return;
        }
        if planned.is_null() {
            return;
        }
        let attributes = &nested.attributes;
        match nested.nesting {
            Nesting::Single | Nesting::Group => self.attributes(attributes, values),
            Nesting::List => {
                let (Value::List(planned_elements), Value::List(configured_elements)) =
                    (planned, configuration)
                else {
                    self.problem("count in plan disagrees with count in configuration");
                    return;
                };
                if planned_elements.len() != configured_elements.len() {
                    self.problem(format!(
                        "count in plan ({}) disagrees with count in configuration ({})",
                        planned_elements.len(),
                        configured_elements.len()
                    ));
                    return;
                }
                for (index, (planned_element, configured_element)) in
                    planned_elements.iter().zip(configured_elements).enumerate()
                {
                    self.within(path_index(index), |check| {
                        check.attributes(
                            attributes,
                            Values {
                                prior: element(prior, index),
                                configuration: configured_element,
                                planned: planned_element,
                            },
                        );
                    });
                }
            }
            Nesting::Map => {
                let (Value::Object(planned_entries), Value::Object(configured_entries)) =
                    (planned, configuration)
                else {
                    self.problem("count in plan disagrees with count in configuration");
                    return;
                };
                if planned_entries.len() != configured_entries.len() {
                    self.problem(format!(
                        "count in plan ({}) disagrees with count in configuration ({})",
                        planned_entries.len(),
                        configured_entries.len()
                    ));
                    return;
                }
                for (key, planned_element) in planned_entries {
                    let Some(configured_element) = configured_entries.get(key) else {
                        self.problem(format!(
                            "map key {key:?} from plan is not present in configuration"
                        ));
                        continue;
                    };
                    self.within(PathStep::Key(key.clone()), |check| {
                        check.attributes(
                            attributes,
                            Values {
                                prior: member(prior, key),
                                configuration: configured_element,
                                planned: planned_element,
                            },
                        );
                    });
                }
                for key in configured_entries.keys() {
                    if !planned_entries.contains_key(key) {
                        self.problem(format!(
                            "map key {key:?} from configuration is not present in plan"
                        ));
                    }
                }
            }
            Nesting::Set => {
                let (Value::List(planned_elements), Value::List(configured_elements)) =
                    (planned, configuration)
                else {
                    return;
                };
                // A configured set holding unknowns has an unknown length,
                // which Terraform cannot compare.
                if configuration.contains_unknown() {
                    return;
                }
                // cty knows a set's length exactly unless unknown elements
                // might coalesce; then it lies between one and the count.
                let planned_count = planned_elements.len();
                let configured_count = configured_elements.len();
                let exact = planned_count == 1 || !planned.contains_unknown();
                let fits = if exact {
                    planned_count == configured_count
                } else {
                    (1..=planned_count).contains(&configured_count)
                };
                if !fits {
                    self.problem(format!(
                        "count in plan ({planned_count}) disagrees with count in configuration \
                         ({configured_count})"
                    ));
                }
            }
        }
    }
}

const UNKNOWN_BLOCK_ELEMENT: &str = "element representing a nested block must not be unknown \
     itself; set nested attribute values to unknown instead";

// ---------------------------------------------------------------------------
// Apply result compatibility
// ---------------------------------------------------------------------------

/// Terraform's `AssertObjectCompatible`: every way `actual` is not a valid
/// completion of `planned`.
///
/// `actual` is the state a provider returned from applying a change and
/// `planned` the state it planned. Every known planned value must be kept;
/// unknown ones may become anything of their type.
///
/// Unlike Terraform, problems never quote values (they may be secrets);
/// they name the path and what changed.
#[must_use]
pub fn compatibility_problems(block: &Block, planned: &Value, actual: &Value) -> Vec<PlanProblem> {
    let mut check = PlanCheck::default();
    check.compatible_object(block, planned, actual);
    check.problems
}

impl PlanCheck {
    fn compatible_object(&mut self, block: &Block, planned: &Value, actual: &Value) {
        let root = if self.path.is_empty() {
            "root object "
        } else {
            ""
        };
        if planned.is_null() && !actual.is_null() {
            self.problem(format!("{root}was absent, but now present"));
            return;
        }
        if actual.is_null() && !planned.is_null() {
            self.problem(format!("{root}was present, but now absent"));
            return;
        }
        if planned.is_null() {
            return;
        }
        for (name, attribute) in &block.attributes {
            self.within(PathStep::Attribute(name.clone()), |check| {
                let mut inner = Self {
                    path: check.path.clone(),
                    problems: Vec::new(),
                };
                inner.compatible_value(
                    &attribute.value_type,
                    member(planned, name),
                    member(actual, name),
                );
                if attribute.contains_sensitive() && !inner.problems.is_empty() {
                    check.problem("inconsistent values for sensitive attribute");
                } else {
                    check.problems.extend(inner.problems);
                }
            });
        }
        for (name, nested) in &block.blocks {
            let planned_blocks = member(planned, name);
            let actual_blocks = member(actual, name);
            self.within(PathStep::Attribute(name.clone()), |check| {
                check.compatible_blocks(nested, planned_blocks, actual_blocks);
            });
        }
    }

    fn compatible_blocks(&mut self, nested: &NestedBlock, planned: &Value, actual: &Value) {
        let block = &nested.block;
        let unusable = |value: &Value| value.is_unknown() || value.is_null();
        match nested.nesting {
            Nesting::Single | Nesting::Group => {
                // An unknown block placeholder may have become no block.
                if planned.is_unknown() && actual.is_null() {
                    return;
                }
                self.compatible_object(block, planned, actual);
            }
            Nesting::List => {
                if unusable(planned) || unusable(actual) {
                    return;
                }
                let (planned_elements, actual_elements) =
                    (list_elements(planned), list_elements(actual));
                if planned_elements.len() != actual_elements.len() {
                    self.problem(format!(
                        "block count changed from {} to {}",
                        planned_elements.len(),
                        actual_elements.len()
                    ));
                    return;
                }
                for (index, (planned_element, actual_element)) in
                    planned_elements.iter().zip(actual_elements).enumerate()
                {
                    self.within(path_index(index), |check| {
                        check.compatible_object(block, planned_element, actual_element);
                    });
                }
            }
            Nesting::Map if type_contains_dynamic(&block.implied_type()) => {
                // Terraform holds these as objects: keys must match.
                let (planned_entries, actual_entries) = (map_entries(planned), map_entries(actual));
                for (key, planned_element) in &planned_entries {
                    let Some(actual_element) = actual_entries.get(key) else {
                        self.problem(format!("block key {key:?} has vanished"));
                        continue;
                    };
                    self.within(PathStep::Key((*key).clone()), |check| {
                        check.compatible_object(block, planned_element, actual_element);
                    });
                }
                if !planned.is_unknown() {
                    for key in actual_entries.keys() {
                        if !planned_entries.contains_key(key) {
                            self.problem(format!("new block key {key:?} has appeared"));
                        }
                    }
                }
            }
            Nesting::Map => {
                if planned.is_unknown() || planned.is_null() || actual.is_null() {
                    return;
                }
                let (planned_entries, actual_entries) = (map_entries(planned), map_entries(actual));
                if planned_entries.len() != actual_entries.len() {
                    self.problem(format!(
                        "block count changed from {} to {}",
                        planned_entries.len(),
                        actual_entries.len()
                    ));
                    return;
                }
                for (key, planned_element) in &planned_entries {
                    if let Some(actual_element) = actual_entries.get(key) {
                        self.within(PathStep::Key((*key).clone()), |check| {
                            check.compatible_object(block, planned_element, actual_element);
                        });
                    }
                }
            }
            Nesting::Set => {
                if unusable(planned) || unusable(actual) {
                    return;
                }
                let (planned_elements, actual_elements) =
                    (list_elements(planned), list_elements(actual));
                let path = self.path.clone();
                let problems = set_correlation_problems(
                    planned_elements,
                    actual_elements,
                    |planned_element, actual_element| {
                        let mut inner = Self {
                            path: path.clone(),
                            problems: Vec::new(),
                        };
                        inner.compatible_object(block, planned_element, actual_element);
                        inner.problems.is_empty()
                    },
                );
                for problem in problems {
                    self.problem(problem);
                }
                // Equal elements may coalesce once known, but a set never
                // grows.
                if planned_elements.len() < actual_elements.len() {
                    self.problem(format!(
                        "block set length changed from {} to {}",
                        planned_elements.len(),
                        actual_elements.len()
                    ));
                }
            }
        }
    }

    /// Terraform's `assertValueCompatible`.
    fn compatible_value(&mut self, value_type: &Type, planned: &Value, actual: &Value) {
        let value_type = if *value_type == Type::Dynamic {
            // A dynamic value is checked against the type it was planned
            // with; with none (an unknown or null of no type), anything goes.
            let Value::Typed(typed) = planned else {
                return;
            };
            if let Value::Typed(actual_typed) = actual
                && !type_conforms(&actual_typed.value_type, &typed.value_type)
            {
                self.problem("wrong final value type");
                return;
            }
            return self.compatible_value(&typed.value_type, &typed.value, actual.untyped());
        } else {
            value_type
        };
        let (planned, actual) = (planned.untyped(), actual.untyped());
        if planned.is_unknown() {
            // Anything of the right type completes an unknown value.
            return;
        }
        if actual.is_null() {
            if !planned.is_null() {
                self.problem("was known, but now null");
            }
            return;
        }
        if planned.is_null() {
            self.problem("was null, but now has a value");
            return;
        }
        if actual.is_unknown() {
            self.problem("was known, but now unknown");
            return;
        }
        match value_type {
            Type::Boolean | Type::Number | Type::String => {
                if !raw_equal(planned, actual, value_type) {
                    self.problem("planned value changed after apply");
                }
            }
            Type::List(element_type) => {
                let (planned_elements, actual_elements) =
                    (list_elements(planned), list_elements(actual));
                self.compatible_sequence(|_| element_type, planned_elements, actual_elements);
            }
            Type::Tuple(element_types) => {
                let (planned_elements, actual_elements) =
                    (list_elements(planned), list_elements(actual));
                self.compatible_sequence(
                    |index| element_types.get(index).unwrap_or(&Type::Dynamic),
                    planned_elements,
                    actual_elements,
                );
            }
            Type::Map(element_type) => {
                let (planned_entries, actual_entries) = (map_entries(planned), map_entries(actual));
                for (key, planned_element) in &planned_entries {
                    let Some(actual_element) = actual_entries.get(key) else {
                        self.problem(format!("element {key:?} has vanished"));
                        continue;
                    };
                    self.within(PathStep::Key((*key).clone()), |check| {
                        check.compatible_value(element_type, planned_element, actual_element);
                    });
                }
                for key in actual_entries.keys() {
                    if !planned_entries.contains_key(key) {
                        self.problem(format!("new element {key:?} has appeared"));
                    }
                }
            }
            Type::Object(attribute_types) => {
                for (name, attribute_type) in attribute_types {
                    self.within(PathStep::Attribute(name.clone()), |check| {
                        check.compatible_value(
                            attribute_type,
                            member(planned, name),
                            member(actual, name),
                        );
                    });
                }
            }
            Type::Set(element_type) => {
                let (planned_elements, actual_elements) =
                    (list_elements(planned), list_elements(actual));
                let path = self.path.clone();
                let problems = set_correlation_problems(
                    planned_elements,
                    actual_elements,
                    |planned_element, actual_element| {
                        let mut inner = Self {
                            path: path.clone(),
                            problems: Vec::new(),
                        };
                        inner.compatible_value(element_type, planned_element, actual_element);
                        inner.problems.is_empty()
                    },
                );
                for problem in problems {
                    self.problem(problem);
                }
                if planned_elements.len() < actual_elements.len() {
                    self.problem(format!(
                        "length changed from {} to {}",
                        planned_elements.len(),
                        actual_elements.len()
                    ));
                }
            }
            Type::Dynamic => {}
        }
    }

    /// Lists and tuples: every planned element kept, none added.
    fn compatible_sequence<'types>(
        &mut self,
        element_type: impl Fn(usize) -> &'types Type,
        planned: &[Value],
        actual: &[Value],
    ) {
        for (index, planned_element) in planned.iter().enumerate() {
            let Some(actual_element) = actual.get(index) else {
                self.problem(format!("element {index} has vanished"));
                continue;
            };
            self.within(path_index(index), |check| {
                check.compatible_value(element_type(index), planned_element, actual_element);
            });
        }
        for index in planned.len()..actual.len() {
            self.problem(format!("new element {index} has appeared"));
        }
    }
}

/// Terraform's `assertSetValuesCompatible`: every planned element must
/// correlate with some actual element and the other way round. Elements
/// are named by position, never by value.
fn set_correlation_problems(
    planned: &[Value],
    actual: &[Value],
    correlates: impl Fn(&Value, &Value) -> bool,
) -> Vec<String> {
    let mut planned_matched = vec![false; planned.len()];
    let mut actual_matched = vec![false; actual.len()];
    for (planned_index, planned_element) in planned.iter().enumerate() {
        for (actual_index, actual_element) in actual.iter().enumerate() {
            if planned_matched[planned_index] && actual_matched[actual_index] {
                continue;
            }
            if correlates(planned_element, actual_element) {
                planned_matched[planned_index] = true;
                actual_matched[actual_index] = true;
            }
        }
    }
    let unmatched = |matched: &[bool], side: &str, other: &str| {
        matched
            .iter()
            .enumerate()
            .filter(|(_, matched)| !**matched)
            .map(|(index, _)| {
                format!("{side} set element {index} does not correlate with any element in {other}")
            })
            .collect::<Vec<_>>()
    };
    let problems = unmatched(&planned_matched, "planned", "actual");
    if problems.is_empty() {
        unmatched(&actual_matched, "actual", "plan")
    } else {
        problems
    }
}

/// Whether `actual` conforms to `planned` (cty `TestConformance`), where a
/// `dynamic` part of the planned type accepts anything.
fn type_conforms(actual: &Type, planned: &Type) -> bool {
    match (actual, planned) {
        (_, Type::Dynamic) => true,
        (Type::List(actual), Type::List(planned))
        | (Type::Set(actual), Type::Set(planned))
        | (Type::Map(actual), Type::Map(planned)) => type_conforms(actual, planned),
        (Type::Object(actual), Type::Object(planned)) => {
            actual.len() == planned.len()
                && planned.iter().all(|(name, planned_type)| {
                    actual
                        .get(name)
                        .is_some_and(|actual_type| type_conforms(actual_type, planned_type))
                })
        }
        (Type::Tuple(actual), Type::Tuple(planned)) => {
            actual.len() == planned.len()
                && actual
                    .iter()
                    .zip(planned)
                    .all(|(actual_type, planned_type)| type_conforms(actual_type, planned_type))
        }
        (actual, planned) => actual == planned,
    }
}

/// Whether a type has a `dynamic` part, which makes Terraform hold map
/// blocks as objects.
fn type_contains_dynamic(value_type: &Type) -> bool {
    match value_type {
        Type::Dynamic => true,
        Type::List(element) | Type::Set(element) | Type::Map(element) => {
            type_contains_dynamic(element)
        }
        Type::Object(attributes) => attributes.values().any(type_contains_dynamic),
        Type::Tuple(elements) => elements.iter().any(type_contains_dynamic),
        Type::Boolean | Type::Number | Type::String => false,
    }
}

const fn nesting_name(nesting: Nesting) -> &'static str {
    match nesting {
        Nesting::Single => "single",
        Nesting::Group => "group",
        Nesting::List => "list",
        Nesting::Set => "set",
        Nesting::Map => "map",
    }
}

fn list_elements(value: &Value) -> &[Value] {
    match value {
        Value::List(elements) => elements,
        _ => &[],
    }
}

fn map_entries(value: &Value) -> BTreeMap<&String, &Value> {
    match value {
        Value::Object(entries) => entries.iter().collect(),
        _ => BTreeMap::new(),
    }
}

fn path_index(index: usize) -> PathStep {
    PathStep::Index(i64::try_from(index).unwrap_or(i64::MAX))
}

/// Render steps as `rules[0].label` or `tags["team"]`.
#[must_use]
pub fn render_steps(steps: &[PathStep]) -> String {
    let mut rendered = String::new();
    for step in steps {
        match step {
            PathStep::Attribute(name) => {
                if !rendered.is_empty() {
                    rendered.push('.');
                }
                rendered.push_str(name);
            }
            PathStep::Key(key) => {
                let _ = write!(rendered, "[{key:?}]");
            }
            PathStep::Index(index) => {
                let _ = write!(rendered, "[{index}]");
            }
        }
    }
    rendered
}

#[cfg(test)]
mod tests;
