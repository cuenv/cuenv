use super::*;

fn attribute(value_type: Type, presence: Presence) -> Attribute {
    Attribute {
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
        plan_problems(&block, &Value::Null, &configuration, &planned),
        Vec::new()
    );
}

#[test]
fn nested_attribute_problems_name_the_nested_path() {
    let block = resource_with_rules(Nesting::List);
    let configuration = resource(Value::Null, vec![rule("a", Value::Null)]);
    let planned = resource(Value::Unknown, vec![rule("secret-b", Value::Unknown)]);
    let problems = plan_problems(&block, &Value::Null, &configuration, &planned);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].path, "rules[0].label");
    assert!(!problems[0].to_string().contains("secret-b"));

    let planned = resource(
        Value::Unknown,
        vec![rule("a", Value::Unknown), rule("b", Value::Unknown)],
    );
    let problems = plan_problems(&block, &Value::Null, &configuration, &planned);
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
    assert!(plan_problems(&block, &Value::Null, &configuration, &unknown_elements).is_empty());
    let known_elements = resource(
        text("id"),
        vec![
            rule("a", text("1")),
            rule("b", text("2")),
            rule("c", text("3")),
        ],
    );
    assert_eq!(
        plan_problems(&block, &Value::Null, &configuration, &known_elements).len(),
        1
    );
}

fn resource_with_rule_blocks() -> Block {
    Block {
        attributes: BTreeMap::from([(
            "name".to_string(),
            attribute(Type::String, Presence::Required),
        )]),
        blocks: BTreeMap::from([(
            "rule".to_string(),
            NestedBlock {
                block: Block {
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
    assert!(plan_problems(&block, &Value::Null, &configuration, &planned).is_empty());

    let too_many = object(&[
        ("name", text("example")),
        (
            "rule",
            Value::List(vec![rule("a", Value::Unknown), rule("b", Value::Unknown)]),
        ),
    ]);
    let problems = plan_problems(&block, &Value::Null, &configuration, &too_many);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].message.contains("block count"));

    let null_blocks = object(&[("name", text("example")), ("rule", Value::Null)]);
    let problems = plan_problems(&block, &Value::Null, &configuration, &null_blocks);
    assert!(
        problems[0].message.contains("must be empty"),
        "{problems:?}"
    );
}

fn named_block(presence: Presence) -> Block {
    Block {
        attributes: BTreeMap::from([("name".to_string(), attribute(Type::String, presence))]),
        blocks: BTreeMap::new(),
    }
}

#[test]
fn a_semantically_equal_prior_value_may_replace_the_configured_one() {
    let block = named_block(Presence::Optional);
    let prior = object(&[("name", text("Hello"))]);
    let configuration = object(&[("name", text("HELLO"))]);
    assert!(plan_problems(&block, &prior, &configuration, &prior).is_empty());

    let invented = object(&[("name", text("hello"))]);
    let problems = plan_problems(&block, &prior, &configuration, &invented);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("nor the prior value"));

    // Only when there is a prior value: a create must match configuration.
    let problems = plan_problems(&block, &Value::Null, &configuration, &prior);
    assert_eq!(problems.len(), 1);
}

#[test]
fn providers_may_not_invent_values_for_non_computed_attributes() {
    let block = named_block(Presence::Optional);
    let configuration = object(&[("name", Value::Null)]);
    let planned = object(&[("name", text("invented"))]);
    let problems = plan_problems(&block, &Value::Null, &configuration, &planned);
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("non-computed"));

    let computed = named_block(Presence::OptionalComputed);
    assert!(plan_problems(&computed, &Value::Null, &configuration, &planned).is_empty());
}

#[test]
fn absent_planned_objects_are_problems() {
    let block = named_block(Presence::Optional);
    let problems = plan_problems(
        &block,
        &Value::Null,
        &object(&[("name", text("x"))]),
        &Value::Null,
    );
    assert!(problems[0].message.contains("planned for absence"));
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
            block: Block::default(),
            nesting: Nesting::Single,
        },
    );
    block.blocks.insert(
        "labels".to_string(),
        NestedBlock {
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
