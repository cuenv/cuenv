use super::*;

fn attribute(value_type: Type, presence: Presence) -> Attribute {
    Attribute {
        write_only: false,
        documentation: crate::schema::Documentation::default(),
        value_type,
        nested: None,
        presence,
        sensitive: false,
    }
}

fn nested_attribute(
    nesting: Nesting,
    attributes: BTreeMap<String, Attribute>,
    presence: Presence,
) -> Attribute {
    let object = NestedAttributes::object_type_of(&attributes);
    let value_type = match nesting {
        Nesting::Single | Nesting::Group => object,
        Nesting::List => Type::List(Box::new(object)),
        Nesting::Set => Type::Set(Box::new(object)),
        Nesting::Map => Type::Map(Box::new(object)),
    };
    Attribute {
        write_only: false,
        documentation: crate::schema::Documentation::default(),
        value_type,
        nested: Some(NestedAttributes {
            attributes,
            nesting,
        }),
        presence,
        sensitive: false,
    }
}

fn object(entries: &[(&str, Value)]) -> Value {
    Value::Object(
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect(),
    )
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

/// [`plan_problems`] of prior, configuration and planned values.
fn check(block: &Block, [prior, configuration, planned]: [&Value; 3]) -> Vec<PlanProblem> {
    plan_problems(
        block,
        PlanValues {
            prior,
            configuration,
            planned,
        },
    )
}

/// A rule with a configured `label` and a provider-computed `rule_id`.
fn rule_attributes() -> BTreeMap<String, Attribute> {
    BTreeMap::from([
        (
            "label".to_string(),
            attribute(Type::String, Presence::Required),
        ),
        (
            "rule_id".to_string(),
            attribute(Type::String, Presence::Computed),
        ),
    ])
}

fn resource_with_rules(nesting: Nesting) -> Block {
    Block {
        documentation: crate::schema::Documentation::default(),
        attributes: BTreeMap::from([
            (
                "id".to_string(),
                attribute(Type::String, Presence::Computed),
            ),
            (
                "name".to_string(),
                attribute(Type::String, Presence::Required),
            ),
            (
                "rules".to_string(),
                nested_attribute(nesting, rule_attributes(), Presence::Optional),
            ),
        ]),
        blocks: BTreeMap::new(),
    }
}

fn rule(label: &str, rule_id: Value) -> Value {
    object(&[("label", text(label)), ("rule_id", rule_id)])
}

fn resource(id: Value, rules: Vec<Value>) -> Value {
    object(&[
        ("id", id),
        ("name", text("example")),
        ("rules", Value::List(rules)),
    ])
}

#[test]
fn nested_attributes_with_computed_children_are_valid_plans() {
    let block = resource_with_rules(Nesting::List);
    let configuration = resource(Value::Null, vec![rule("a", Value::Null)]);
    let planned = resource(Value::Unknown, vec![rule("a", Value::Unknown)]);
    assert_eq!(
        check(&block, [&Value::Null, &configuration, &planned]),
        Vec::new()
    );
}

#[test]
fn nested_attribute_problems_name_the_nested_path() {
    let block = resource_with_rules(Nesting::List);
    let configuration = resource(Value::Null, vec![rule("a", Value::Null)]);
    let planned = resource(Value::Unknown, vec![rule("secret-b", Value::Unknown)]);
    let problems = check(&block, [&Value::Null, &configuration, &planned]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].path, "rules[0].label");
    assert!(!problems[0].to_string().contains("secret-b"));

    let planned = resource(
        Value::Unknown,
        vec![rule("a", Value::Unknown), rule("b", Value::Unknown)],
    );
    let problems = check(&block, [&Value::Null, &configuration, &planned]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].path, "rules");
    assert!(problems[0].message.contains("count in plan (2)"));
}

#[test]
fn nested_attribute_sets_allow_coalescing_unknown_elements() {
    let block = resource_with_rules(Nesting::Set);
    let configuration = resource(
        Value::Null,
        vec![rule("a", Value::Null), rule("b", Value::Null)],
    );
    let unknown_elements = resource(
        Value::Unknown,
        vec![
            rule("a", Value::Unknown),
            rule("b", Value::Unknown),
            rule("c", Value::Unknown),
        ],
    );
    assert!(check(&block, [&Value::Null, &configuration, &unknown_elements]).is_empty());
    let known_elements = resource(
        text("id"),
        vec![
            rule("a", text("1")),
            rule("b", text("2")),
            rule("c", text("3")),
        ],
    );
    assert_eq!(
        check(&block, [&Value::Null, &configuration, &known_elements]).len(),
        1
    );
}

fn resource_with_rule_blocks() -> Block {
    Block {
        documentation: crate::schema::Documentation::default(),
        attributes: BTreeMap::from([(
            "name".to_string(),
            attribute(Type::String, Presence::Required),
        )]),
        blocks: BTreeMap::from([(
            "rule".to_string(),
            NestedBlock {
                minimum_items: 0,
                maximum_items: 0,
                block: Block {
                    documentation: crate::schema::Documentation::default(),
                    attributes: rule_attributes(),
                    blocks: BTreeMap::new(),
                },
                nesting: Nesting::List,
            },
        )]),
    }
}

#[test]
fn nested_blocks_with_computed_children_are_valid_plans() {
    let block = resource_with_rule_blocks();
    let configuration = object(&[
        ("name", text("example")),
        ("rule", Value::List(vec![rule("a", Value::Null)])),
    ]);
    let planned = object(&[
        ("name", text("example")),
        ("rule", Value::List(vec![rule("a", Value::Unknown)])),
    ]);
    assert!(check(&block, [&Value::Null, &configuration, &planned]).is_empty());

    let too_many = object(&[
        ("name", text("example")),
        (
            "rule",
            Value::List(vec![rule("a", Value::Unknown), rule("b", Value::Unknown)]),
        ),
    ]);
    let problems = check(&block, [&Value::Null, &configuration, &too_many]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].message.contains("block count"));

    let null_blocks = object(&[("name", text("example")), ("rule", Value::Null)]);
    let problems = check(&block, [&Value::Null, &configuration, &null_blocks]);
    assert!(
        problems[0].message.contains("must be empty"),
        "{problems:?}"
    );
}

fn named_block(presence: Presence) -> Block {
    Block {
        documentation: crate::schema::Documentation::default(),
        attributes: BTreeMap::from([("name".to_string(), attribute(Type::String, presence))]),
        blocks: BTreeMap::new(),
    }
}

#[test]
fn a_semantically_equal_prior_value_may_replace_the_configured_one() {
    let block = named_block(Presence::Optional);
    let prior = object(&[("name", text("Hello"))]);
    let configuration = object(&[("name", text("HELLO"))]);
    assert!(check(&block, [&prior, &configuration, &prior]).is_empty());

    let invented = object(&[("name", text("hello"))]);
    let problems = check(&block, [&prior, &configuration, &invented]);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("nor the prior value"));

    // Only when there is a prior value: a create must match configuration.
    let problems = check(&block, [&Value::Null, &configuration, &prior]);
    assert_eq!(problems.len(), 1);
}

#[test]
fn providers_may_not_invent_values_for_non_computed_attributes() {
    let block = named_block(Presence::Optional);
    let configuration = object(&[("name", Value::Null)]);
    let planned = object(&[("name", text("invented"))]);
    let problems = check(&block, [&Value::Null, &configuration, &planned]);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("non-computed"));

    let computed = named_block(Presence::OptionalComputed);
    assert!(check(&computed, [&Value::Null, &configuration, &planned]).is_empty());
}

#[test]
fn absent_planned_objects_are_problems() {
    let block = named_block(Presence::Optional);
    let problems = check(
        &block,
        [&Value::Null, &object(&[("name", text("x"))]), &Value::Null],
    );
    assert!(problems[0].message.contains("planned for absence"));
}

#[test]
fn optional_nested_attributes_holding_only_computed_values_were_not_configured() {
    // Terraform counts only leaf attributes: a nested attribute whose
    // children are all computed says nothing about configuration, even
    // when the nested attribute itself is optional.
    let inner = BTreeMap::from([(
        "token".to_string(),
        attribute(Type::String, Presence::Computed),
    )]);
    let settings = BTreeMap::from([(
        "inner".to_string(),
        nested_attribute(Nesting::Single, inner, Presence::Optional),
    )]);
    let block = Block {
        documentation: crate::schema::Documentation::default(),
        attributes: BTreeMap::from([(
            "settings".to_string(),
            nested_attribute(Nesting::Single, settings, Presence::OptionalComputed),
        )]),
        blocks: BTreeMap::new(),
    };
    let prior = object(&[(
        "settings",
        object(&[("inner", object(&[("token", text("t"))]))]),
    )]);
    let configuration = object(&[("settings", Value::Null)]);
    assert_eq!(proposed_new(&block, &prior, &configuration), prior);
}

#[test]
fn set_counts_are_not_compared_with_a_configuration_holding_unknowns() {
    let block = resource_with_rules(Nesting::Set);
    let configuration = resource(
        Value::Null,
        vec![rule("a", Value::Null), Value::Unknown, Value::Unknown],
    );
    let planned = resource(Value::Unknown, vec![rule("a", Value::Unknown)]);
    assert!(check(&block, [&Value::Null, &configuration, &planned]).is_empty());
}

fn compatible(block: &Block, planned: &Value, actual: &Value) -> Vec<String> {
    compatibility_problems(block, planned, actual)
        .iter()
        .map(PlanProblem::to_string)
        .collect()
}

#[test]
fn apply_results_must_keep_every_known_planned_value() {
    let block = resource_with_rules(Nesting::List);
    let planned = resource(Value::Unknown, vec![rule("a", Value::Unknown)]);
    // Unknowns may become anything.
    let actual = resource(text("id-1"), vec![rule("a", text("r-a"))]);
    assert_eq!(compatible(&block, &planned, &actual), Vec::<String>::new());

    // A known value that changed is a provider bug; the value is not shown.
    let drifted = object(&[
        ("id", text("id-1")),
        ("name", text("secret-drifted")),
        ("rules", Value::List(vec![rule("a", text("r-a"))])),
    ]);
    let problems = compatible(&block, &planned, &drifted);
    assert_eq!(problems, vec!["name: planned value changed after apply"]);

    // Elements may neither vanish nor appear.
    let extra = resource(
        text("id-1"),
        vec![rule("a", text("r-a")), rule("b", text("r-b"))],
    );
    assert_eq!(
        compatible(&block, &planned, &extra),
        vec!["rules: new element 1 has appeared"]
    );
    let none = resource(text("id-1"), Vec::new());
    assert_eq!(
        compatible(&block, &planned, &none),
        vec!["rules: element 0 has vanished"]
    );

    // Known values must stay known and present.
    let known = resource(text("id-1"), vec![rule("a", text("r-a"))]);
    let nulled = object(&[
        ("id", Value::Null),
        ("name", text("example")),
        ("rules", Value::List(vec![rule("a", text("r-a"))])),
    ]);
    assert_eq!(
        compatible(&block, &known, &nulled),
        vec!["id: was known, but now null"]
    );
    assert_eq!(
        compatible(&block, &known, &Value::Null),
        vec!["root object was present, but now absent"]
    );
}

#[test]
fn apply_results_of_sets_and_blocks_are_correlated() {
    let block = resource_with_rules(Nesting::Set);
    let planned = resource(
        text("id"),
        vec![rule("a", Value::Unknown), rule("b", Value::Unknown)],
    );
    let reordered = resource(text("id"), vec![rule("b", text("2")), rule("a", text("1"))]);
    assert!(compatible(&block, &planned, &reordered).is_empty());
    let renamed = resource(text("id"), vec![rule("a", text("1")), rule("c", text("3"))]);
    let problems = compatible(&block, &planned, &renamed);
    assert_eq!(
        problems,
        vec!["rules: planned set element 1 does not correlate with any element in actual"]
    );

    let block = resource_with_rule_blocks();
    let planned = object(&[
        ("name", text("example")),
        ("rule", Value::List(vec![rule("a", Value::Unknown)])),
    ]);
    let actual = object(&[
        ("name", text("example")),
        ("rule", Value::List(vec![rule("changed", text("r"))])),
    ]);
    assert_eq!(
        compatible(&block, &planned, &actual),
        vec!["rule[0].label: planned value changed after apply"]
    );
}

#[test]
fn sensitive_attributes_report_only_that_they_are_inconsistent() {
    let mut block = named_block(Presence::Required);
    if let Some(attribute) = block.attributes.get_mut("name") {
        attribute.sensitive = true;
    }
    let problems = compatible(
        &block,
        &object(&[("name", text("a"))]),
        &object(&[("name", text("b"))]),
    );
    assert_eq!(
        problems,
        vec!["name: inconsistent values for sensitive attribute"]
    );
}

#[test]
fn dynamic_apply_results_keep_their_planned_type() {
    let block = Block {
        documentation: crate::schema::Documentation::default(),
        attributes: BTreeMap::from([(
            "data".to_string(),
            attribute(Type::Dynamic, Presence::Computed),
        )]),
        blocks: BTreeMap::new(),
    };
    let list = |items: &[&str]| {
        Value::typed(
            Type::List(Box::new(Type::String)),
            Value::List(items.iter().map(|item| text(item)).collect()),
        )
    };
    let planned = object(&[("data", list(&["a"]))]);
    assert!(compatible(&block, &planned, &object(&[("data", list(&["a"]))])).is_empty());
    let tuple = Value::typed(
        Type::Tuple(vec![Type::String]),
        Value::List(vec![text("a")]),
    );
    assert_eq!(
        compatible(&block, &planned, &object(&[("data", tuple)])),
        vec!["data: wrong final value type"]
    );
    // An unknown of no type may become anything.
    let unknown = object(&[("data", Value::Unknown)]);
    assert!(compatible(&block, &unknown, &object(&[("data", list(&["x"]))])).is_empty());
}

#[test]
fn proposed_new_carries_computed_children_of_nested_attributes() {
    let block = resource_with_rules(Nesting::List);
    let prior = resource(
        text("id-1"),
        vec![rule("a", text("r-a")), rule("b", text("r-b"))],
    );
    let configuration = resource(
        Value::Null,
        vec![
            rule("a", Value::Null),
            rule("b", Value::Null),
            rule("c", Value::Null),
        ],
    );
    let proposed = proposed_new(&block, &prior, &configuration);
    assert_eq!(
        proposed,
        resource(
            text("id-1"),
            vec![
                rule("a", text("r-a")),
                rule("b", text("r-b")),
                rule("c", Value::Null)
            ]
        )
    );
}

#[test]
fn proposed_new_correlates_set_elements_by_their_configured_values() {
    let block = resource_with_rules(Nesting::Set);
    let prior = resource(
        text("id-1"),
        vec![rule("a", text("r-a")), rule("b", text("r-b"))],
    );
    let configuration = resource(
        Value::Null,
        vec![rule("b", Value::Null), rule("new", Value::Null)],
    );
    let proposed = proposed_new(&block, &prior, &configuration);
    assert_eq!(
        proposed,
        resource(
            text("id-1"),
            vec![rule("b", text("r-b")), rule("new", Value::Null)]
        )
    );
}

#[test]
fn proposed_new_correlates_list_blocks_by_index() {
    let block = resource_with_rule_blocks();
    let prior = object(&[
        ("name", text("example")),
        ("rule", Value::List(vec![rule("a", text("r-a"))])),
    ]);
    let configuration = object(&[
        ("name", text("renamed")),
        ("rule", Value::List(vec![rule("a", Value::Null)])),
    ]);
    assert_eq!(
        proposed_new(&block, &prior, &configuration),
        object(&[
            ("name", text("renamed")),
            ("rule", Value::List(vec![rule("a", text("r-a"))])),
        ])
    );
}

#[test]
fn removed_optional_nested_values_are_not_kept_from_prior_state() {
    let settings = BTreeMap::from([
        (
            "mode".to_string(),
            attribute(Type::String, Presence::Optional),
        ),
        (
            "token".to_string(),
            attribute(Type::String, Presence::Computed),
        ),
    ]);
    let block = Block {
        documentation: crate::schema::Documentation::default(),
        attributes: BTreeMap::from([(
            "settings".to_string(),
            nested_attribute(Nesting::Single, settings, Presence::OptionalComputed),
        )]),
        blocks: BTreeMap::new(),
    };
    let configuration = object(&[("settings", Value::Null)]);

    // The prior value holds a configured `mode`, so the configuration used
    // to set `settings` and has now removed it.
    let configured_before = object(&[(
        "settings",
        object(&[("mode", text("fast")), ("token", text("t"))]),
    )]);
    assert_eq!(
        proposed_new(&block, &configured_before, &configuration),
        configuration
    );

    // Only computed values: the provider chose them, so keep them.
    let computed_before = object(&[(
        "settings",
        object(&[("mode", Value::Null), ("token", text("t"))]),
    )]);
    assert_eq!(
        proposed_new(&block, &computed_before, &configuration),
        computed_before
    );
}

#[test]
fn empty_values_follow_nesting_modes() {
    let mut block = resource_with_rule_blocks();
    block.blocks.insert(
        "single".to_string(),
        NestedBlock {
            minimum_items: 0,
            maximum_items: 0,
            block: Block::default(),
            nesting: Nesting::Single,
        },
    );
    block.blocks.insert(
        "labels".to_string(),
        NestedBlock {
            minimum_items: 0,
            maximum_items: 0,
            block: Block::default(),
            nesting: Nesting::Map,
        },
    );
    assert_eq!(
        block.empty_value(),
        object(&[
            ("labels", Value::Object(BTreeMap::new())),
            ("name", Value::Null),
            ("rule", Value::List(Vec::new())),
            ("single", Value::Null),
        ])
    );
}

#[test]
fn paths_render_like_terraform() {
    assert_eq!(
        render_steps(&[
            PathStep::Attribute("rules".into()),
            PathStep::Index(0),
            PathStep::Attribute("tags".into()),
            PathStep::Key("team".into()),
        ]),
        r#"rules[0].tags["team"]"#
    );
}
