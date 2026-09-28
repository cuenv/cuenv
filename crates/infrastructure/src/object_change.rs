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

fn holds_non_computed(nested: &NestedAttributes, value: &Value) -> bool {
    let object_holds = |object: &Value| {
        nested.attributes.iter().any(|(name, attribute)| {
            let member_value = member(object, name);
            if member_value.is_null() || matches!(member_value, Value::Unknown) {
                return false;
            }
            !attribute.presence.is_computed()
                || attribute
                    .nested
                    .as_ref()
                    .is_some_and(|inner| holds_non_computed(inner, member_value))
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
            ElementSchema::Object(&nested.attributes),
            nested.nesting,
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
        ElementSchema::Block(&nested.block),
        nested.nesting,
        prior,
        configuration,
    )
}

fn collection_derives_from(
    schema: ElementSchema<'_>,
    nesting: Nesting,
    prior: &Value,
    configuration: &Value,
) -> bool {
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

/// Terraform's `AssertPlanValid`: every way `planned` departs from what a
/// provider may plan for `configuration` given `prior` (null for a create).
#[must_use]
pub fn plan_problems(
    block: &Block,
    prior: &Value,
    configuration: &Value,
    planned: &Value,
) -> Vec<PlanProblem> {
    let mut check = PlanCheck::default();
    check.block(
        block,
        Values {
            prior,
            configuration,
            planned,
        },
    );
    check.problems
}

/// The prior, configured and planned values at one point of the walk.
#[derive(Debug, Clone, Copy)]
struct Values<'value> {
    prior: &'value Value,
    configuration: &'value Value,
    planned: &'value Value,
}

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
