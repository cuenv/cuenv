use super::*;

#[test]
fn test_value_has_provider_interpolated_with_exec_secret() {
    use cuenv_core::environment::{EnvPart, EnvValue};
    use cuenv_core::secrets::Secret;

    let secret = Secret::new("echo".to_string(), vec!["test".to_string()]);
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
    ];
    let value = EnvValue::Interpolated(parts);

    // exec secret should NOT match onepassword provider
    assert!(!Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_interpolated_with_onepassword_secret() {
    use cuenv_core::environment::{EnvPart, EnvValue};
    use cuenv_core::secrets::Secret;

    let secret = Secret::onepassword("op://vault/item/field");
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
    ];
    let value = EnvValue::Interpolated(parts);

    // onepassword secret SHOULD match onepassword provider
    assert!(Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_with_infisical_secret() {
    use cuenv_core::environment::EnvValue;
    use cuenv_core::secrets::Secret;
    use serde_json::json;
    use std::collections::HashMap;

    let mut extra = HashMap::new();
    extra.insert("projectId".to_string(), json!("project"));
    extra.insert("environment".to_string(), json!("prod"));
    extra.insert("secretName".to_string(), json!("API_KEY"));
    let value = EnvValue::Secret(Secret {
        resolver: "infisical".to_string(),
        command: String::new(),
        args: Vec::new(),
        op_ref: None,
        extra,
    });

    assert!(Compiler::value_has_provider(
        &value,
        &["infisical".to_string()]
    ));
    assert!(!Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_with_policies_infisical_secret() {
    use cuenv_core::environment::{EnvValue, EnvValueSimple, EnvVarWithPolicies};
    use cuenv_core::secrets::Secret;
    use serde_json::json;
    use std::collections::HashMap;

    let mut extra = HashMap::new();
    extra.insert("projectId".to_string(), json!("project"));
    extra.insert("environment".to_string(), json!("prod"));
    extra.insert("secretName".to_string(), json!("API_KEY"));
    let value = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::Secret(Secret {
            resolver: "infisical".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: None,
            extra,
        }),
        policies: None,
    });

    assert!(Compiler::value_has_provider(
        &value,
        &["infisical".to_string()]
    ));
}

#[test]
fn test_value_has_provider_interpolated_only_literals() {
    use cuenv_core::environment::{EnvPart, EnvValue};

    let parts = vec![
        EnvPart::Literal("hello".to_string()),
        EnvPart::Literal("world".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);

    // No secrets = no provider match
    assert!(!Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_interpolated_with_op_uri_in_literal() {
    use cuenv_core::environment::{EnvPart, EnvValue};

    // A literal string containing op:// should match onepassword provider
    let parts = vec![
        EnvPart::Literal("op://vault/item/field".to_string()),
        EnvPart::Literal("-suffix".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);

    assert!(Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_with_policies_interpolated() {
    use cuenv_core::environment::{EnvPart, EnvValue, EnvValueSimple, EnvVarWithPolicies};
    use cuenv_core::secrets::Secret;

    let secret = Secret::onepassword("op://vault/item/field");
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
    ];

    let value = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::Interpolated(parts),
        policies: None,
    });

    assert!(Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_parts_have_provider_op_uri_in_literal() {
    use cuenv_core::environment::EnvPart;

    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Literal("op://vault/item/password".to_string()),
    ];

    // op:// URI in literal should match onepassword
    assert!(Compiler::parts_have_provider(
        &parts,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_parts_have_provider_op_uri_not_matching_other_providers() {
    use cuenv_core::environment::EnvPart;

    let parts = vec![EnvPart::Literal("op://vault/item/password".to_string())];

    // op:// should NOT match other providers like "aws" or "vault"
    assert!(!Compiler::parts_have_provider(&parts, &["aws".to_string()]));
    assert!(!Compiler::parts_have_provider(
        &parts,
        &["vault".to_string()]
    ));
}
