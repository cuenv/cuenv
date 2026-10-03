use super::*;
use crate::commands::Command;
use crate::tracing::LogLevel;
use clap::Parser;

#[test]
fn test_cli_default_values() {
    let cli = Cli::try_parse_from(["cuenv", "version"]).unwrap();

    assert!(matches!(cli.level, LogLevel::Warn)); // Default log level
    assert!(!cli.json); // Default JSON is false
    assert!(!cli.llms); // Default llms is false
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Text);
    } else {
        panic!("Expected Version command");
    }
}

#[test]
fn test_cli_log_level_parsing() {
    // Test each level individually
    let cli = Cli::try_parse_from(["cuenv", "--level", "trace", "version"]).unwrap();
    assert!(matches!(cli.level, LogLevel::Trace));

    let cli = Cli::try_parse_from(["cuenv", "--level", "debug", "version"]).unwrap();
    assert!(matches!(cli.level, LogLevel::Debug));

    let cli = Cli::try_parse_from(["cuenv", "--level", "info", "version"]).unwrap();
    assert!(matches!(cli.level, LogLevel::Info));

    let cli = Cli::try_parse_from(["cuenv", "--level", "warn", "version"]).unwrap();
    assert!(matches!(cli.level, LogLevel::Warn));

    let cli = Cli::try_parse_from(["cuenv", "--level", "error", "version"]).unwrap();
    assert!(matches!(cli.level, LogLevel::Error));

    // Test short form for a few cases
    let cli_short = Cli::try_parse_from(["cuenv", "-L", "debug", "version"]).unwrap();
    assert!(matches!(cli_short.level, LogLevel::Debug));

    let cli_short = Cli::try_parse_from(["cuenv", "-L", "error", "version"]).unwrap();
    assert!(matches!(cli_short.level, LogLevel::Error));
}

#[test]
fn test_cli_json_flag() {
    let cli = Cli::try_parse_from(["cuenv", "--json", "version"]).unwrap();
    assert!(cli.json);

    let cli_no_json = Cli::try_parse_from(["cuenv", "version"]).unwrap();
    assert!(!cli_no_json.json);
}

#[test]
fn test_cli_format_option() {
    let cli = Cli::try_parse_from(["cuenv", "version", "--output", "json"]).unwrap();
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Json);
    } else {
        panic!("Expected Version command");
    }
}

#[test]
fn test_cli_combined_flags() {
    let cli = Cli::try_parse_from([
        "cuenv", "--level", "debug", "--json", "version", "--output", "env",
    ])
    .unwrap();

    assert!(matches!(cli.level, LogLevel::Debug));
    assert!(cli.json);
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Env);
    } else {
        panic!("Expected Version command");
    }
}

#[test]
fn test_command_conversion() {
    let version_cmd = Commands::Version {
        output_format: OutputFormat::Text,
    };
    let command: Command = version_cmd.into_command(None);
    match command {
        Command::Version { format } => assert_eq!(format, "text"),
        _ => panic!("Expected Command::Version"),
    }
}

#[test]
fn test_infrastructure_command_conversion() {
    use crate::commands::infrastructure::{
        ConfirmationPolicy, InfrastructureAction, SeparateState, StateAction, UnlockScope,
    };

    let cli = Cli::try_parse_from(["cuenv", "i", "destroy", "--yes", "-p", "infra"]).unwrap();
    let command = cli.command.unwrap().into_command(None);
    let Command::Infrastructure { path, action, .. } = command else {
        panic!("Expected Command::Infrastructure");
    };
    assert_eq!(path, "infra");
    assert_eq!(
        action,
        InfrastructureAction::Destroy {
            confirmation: ConfirmationPolicy::AssumeYes
        }
    );

    let apply_action = |arguments: &[&str]| {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let Command::Infrastructure { action, .. } = cli.command.unwrap().into_command(None) else {
            panic!("Expected Command::Infrastructure");
        };
        action
    };
    assert_eq!(
        apply_action(&["cuenv", "i", "apply", "--yes"]),
        InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::AssumeYes,
            separate_state: SeparateState::Refuse,
        }
    );
    assert_eq!(
        apply_action(&["cuenv", "i", "apply", "--allow-separate-state"]),
        InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::Prompt,
            separate_state: SeparateState::Allow,
        }
    );
    // Only `apply` creates, so only it has the flag.
    assert!(Cli::try_parse_from(["cuenv", "i", "destroy", "--allow-separate-state"]).is_err());

    let state_action = |arguments: &[&str]| {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let Command::Infrastructure { path, action, .. } = cli.command.unwrap().into_command(None)
        else {
            panic!("Expected Command::Infrastructure");
        };
        (path, action)
    };
    assert_eq!(
        state_action(&["cuenv", "i", "state"]),
        (
            ".".to_string(),
            InfrastructureAction::State(StateAction::List)
        )
    );
    assert_eq!(
        state_action(&["cuenv", "i", "state", "list", "-p", "infra"]),
        (
            "infra".to_string(),
            InfrastructureAction::State(StateAction::List)
        )
    );
    assert_eq!(
        state_action(&[
            "cuenv",
            "i",
            "state",
            "-p",
            "infra",
            "remove",
            "random_pet.pet"
        ]),
        (
            "infra".to_string(),
            InfrastructureAction::State(StateAction::Remove {
                address: "random_pet.pet".to_string()
            })
        )
    );
    assert_eq!(
        state_action(&["cuenv", "i", "state", "recover"]).1,
        InfrastructureAction::State(StateAction::Recover {
            overrides: cuenv_infrastructure::RecoverOverrides::default()
        })
    );
    assert_eq!(
        state_action(&["cuenv", "i", "state", "recover", "--force"]).1,
        InfrastructureAction::State(StateAction::Recover {
            overrides: cuenv_infrastructure::RecoverOverrides {
                changed_record: cuenv_infrastructure::ChangedRecord::Overwrite,
                backend: cuenv_infrastructure::BackendMismatch::Refuse,
            }
        })
    );
    assert_eq!(
        state_action(&["cuenv", "i", "state", "recover", "--accept-backend"]).1,
        InfrastructureAction::State(StateAction::Recover {
            overrides: cuenv_infrastructure::RecoverOverrides {
                changed_record: cuenv_infrastructure::ChangedRecord::Refuse,
                backend: cuenv_infrastructure::BackendMismatch::Accept,
            }
        })
    );
    assert_eq!(
        state_action(&["cuenv", "i", "state", "locks", "-p", "infra"]),
        (
            "infra".to_string(),
            InfrastructureAction::State(StateAction::Locks)
        )
    );
    assert_eq!(
        state_action(&["cuenv", "i", "state", "adopt", "-p", "infra"]),
        (
            "infra".to_string(),
            InfrastructureAction::State(StateAction::Adopt)
        )
    );

    let cli = Cli::try_parse_from(["cuenv", "infrastructure", "unlock", "abc"]).unwrap();
    let Command::Infrastructure { action, .. } = cli.command.unwrap().into_command(None) else {
        panic!("Expected Command::Infrastructure");
    };
    assert_eq!(
        action,
        InfrastructureAction::Unlock {
            lock_identifier: Some("abc".to_string()),
            scope: UnlockScope::default(),
        }
    );
    let cli = Cli::try_parse_from([
        "cuenv",
        "infrastructure",
        "unlock",
        "abc",
        "--module",
        "example.com/m",
        "--project",
        "api",
    ])
    .unwrap();
    let Command::Infrastructure { action, .. } = cli.command.unwrap().into_command(None) else {
        panic!("Expected Command::Infrastructure");
    };
    assert_eq!(
        action,
        InfrastructureAction::Unlock {
            lock_identifier: Some("abc".to_string()),
            scope: UnlockScope {
                module_path: Some("example.com/m".to_string()),
                project: Some("api".to_string()),
            },
        }
    );
}

#[test]
fn infrastructure_command_receives_global_environment_selector() {
    let cli = Cli::try_parse_from(["cuenv", "--env", "Prod", "i", "plan"]).unwrap();
    let command = cli.command.unwrap().into_command(cli.environment);
    let Command::Infrastructure { environment, .. } = command else {
        panic!("Expected Command::Infrastructure");
    };
    assert_eq!(environment.as_deref(), Some("Prod"));

    let cli = Cli::try_parse_from(["cuenv", "i", "plan"]).unwrap();
    let command = cli.command.unwrap().into_command(cli.environment);
    let Command::Infrastructure { environment, .. } = command else {
        panic!("Expected Command::Infrastructure");
    };
    assert_eq!(environment, None);
}

#[test]
fn test_invalid_log_level() {
    let result = Cli::try_parse_from(["cuenv", "--level", "invalid", "version"]);
    assert!(result.is_err());
}

#[test]
fn test_missing_subcommand() {
    // With Optional command, missing subcommand parses successfully
    let cli = Cli::try_parse_from(["cuenv"]).unwrap();
    assert!(cli.command.is_none());
}

#[test]
fn test_llms_flag() {
    let cli = Cli::try_parse_from(["cuenv", "--llms"]).unwrap();
    assert!(cli.llms);
    assert!(cli.command.is_none());

    // --llms with a subcommand also works
    let cli = Cli::try_parse_from(["cuenv", "--llms", "version"]).unwrap();
    assert!(cli.llms);
    assert!(cli.command.is_some());
}

#[test]
fn test_help_flag() {
    let result = Cli::try_parse_from(["cuenv", "--help"]);
    // Help flag should cause an error with help message
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
}

#[test]
fn test_env_print_command_default() {
    let cli = Cli::try_parse_from(["cuenv", "env", "print"]).unwrap();

    if let Some(Commands::Env { subcommand }) = cli.command {
        if let EnvCommands::Print {
            path,
            package,
            output_format,
        } = subcommand
        {
            assert_eq!(path, ".");
            assert_eq!(package, "cuenv");
            assert!(matches!(output_format, OutputFormat::Env));
        } else {
            panic!("Expected EnvCommands::Print");
        }
    } else {
        panic!("Expected Env command");
    }
}

#[test]
fn test_env_print_command_with_options() {
    let cli = Cli::try_parse_from([
        "cuenv",
        "env",
        "print",
        "--path",
        "examples/env-basic",
        "--package",
        "examples",
        "--output",
        "json",
    ])
    .unwrap();

    if let Some(Commands::Env { subcommand }) = cli.command {
        match subcommand {
            EnvCommands::Print {
                path,
                package,
                output_format,
            } => {
                assert_eq!(path, "examples/env-basic");
                assert_eq!(package, "examples");
                assert!(matches!(output_format, OutputFormat::Json));
            }
            _ => panic!("Expected EnvCommands::Print"),
        }
    } else {
        panic!("Expected Env command");
    }
}

#[test]
fn test_env_print_command_short_path() {
    let cli = Cli::try_parse_from(["cuenv", "env", "print", "-p", "test/path"]).unwrap();

    if let Some(Commands::Env { subcommand }) = cli.command {
        match subcommand {
            EnvCommands::Print {
                path,
                package,
                output_format,
            } => {
                assert_eq!(path, "test/path");
                assert_eq!(package, "cuenv"); // default
                assert!(matches!(output_format, OutputFormat::Env)); // default
            }
            _ => panic!("Expected EnvCommands::Print"),
        }
    } else {
        panic!("Expected Env command");
    }
}

#[test]
fn test_env_command_conversion() {
    let env_cmd = Commands::Env {
        subcommand: EnvCommands::Print {
            path: "test".to_string(),
            package: "pkg".to_string(),
            output_format: OutputFormat::Json,
        },
    };
    let command: Command = env_cmd.into_command(Some("production".to_string()));

    if let Command::EnvPrint {
        path,
        package,
        format,
        environment,
    } = command
    {
        assert_eq!(path, "test");
        assert_eq!(package, "pkg");
        assert_eq!(format, "json");
        assert_eq!(environment, Some("production".to_string()));
    } else {
        panic!("Expected EnvPrint command");
    }
}

#[test]
fn test_output_format_enum() {
    assert_eq!(OutputFormat::default(), OutputFormat::Text);

    // Test serialization/deserialization
    let json_fmt = OutputFormat::Json;
    let serialized = serde_json::to_string(&json_fmt).unwrap();
    assert_eq!(serialized, "\"Json\"");

    let deserialized: OutputFormat = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized, OutputFormat::Json);
}

#[test]
fn test_ok_envelope() {
    let data = "test data";
    let envelope = OkEnvelope::new(data);

    assert_eq!(envelope.status, "ok");
    assert_eq!(envelope.data, "test data");

    // Test serialization
    let json = serde_json::to_string(&envelope).unwrap();
    assert!(json.contains("\"status\":\"ok\""));
    assert!(json.contains("\"data\":\"test data\""));
}

#[test]
fn test_error_envelope() {
    let error = "test error";
    let envelope = ErrorEnvelope::new(error);

    assert_eq!(envelope.status, "error");
    assert_eq!(envelope.error, "test error");

    // Test serialization
    let json = serde_json::to_string(&envelope).unwrap();
    assert!(json.contains("\"status\":\"error\""));
    assert!(json.contains("\"error\":\"test error\""));
}

#[test]
fn test_output_format_value_enum() {
    // Test that the formats work with clap
    let cli = Cli::try_parse_from(["cuenv", "version", "--output", "text"]).unwrap();
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Text);
    } else {
        panic!("Expected Version command");
    }

    let cli = Cli::try_parse_from(["cuenv", "version", "--output", "env"]).unwrap();
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Env);
    } else {
        panic!("Expected Version command");
    }

    let cli = Cli::try_parse_from(["cuenv", "version", "--output", "json"]).unwrap();
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Json);
    } else {
        panic!("Expected Version command");
    }

    // Test short form -o
    let cli = Cli::try_parse_from(["cuenv", "version", "-o", "rich"]).unwrap();
    if let Some(Commands::Version { output_format }) = cli.command {
        assert_eq!(output_format, OutputFormat::Rich);
    } else {
        panic!("Expected Version command");
    }
}

#[test]
fn test_invalid_output_format() {
    let result = Cli::try_parse_from(["cuenv", "version", "--output", "invalid"]);
    assert!(result.is_err());
}

#[test]
fn test_cli_error_types() {
    let config_err = CliError::config("test config error");
    assert!(matches!(config_err, CliError::Config { .. }));
    assert_eq!(exit_code_for(&config_err), EXIT_CLI);

    let eval_err = CliError::eval("test eval error");
    assert!(matches!(eval_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&eval_err), EXIT_EVAL);

    let other_err = CliError::other("test other error");
    assert!(matches!(other_err, CliError::Other { .. }));
    assert_eq!(exit_code_for(&other_err), EXIT_EVAL);
}

#[test]
fn test_cli_error_with_help() {
    let config_err = CliError::config_with_help("config problem", "try this fix");
    if let CliError::Config { message, help } = config_err {
        assert_eq!(message, "config problem");
        assert_eq!(help, Some("try this fix".to_string()));
    } else {
        panic!("Expected Config error");
    }

    let eval_err = CliError::eval_with_help("eval problem", "check your CUE files");
    if let CliError::Eval { message, help } = eval_err {
        assert_eq!(message, "eval problem");
        assert_eq!(help, Some("check your CUE files".to_string()));
    } else {
        panic!("Expected Eval error");
    }
}

#[test]
fn test_exit_codes() {
    assert_eq!(EXIT_OK, 0);
    assert_eq!(EXIT_CLI, 2);
    assert_eq!(EXIT_EVAL, 3);

    // Test exit code mapping
    let config_err = CliError::config("test");
    assert_eq!(exit_code_for(&config_err), 2);

    let eval_err = CliError::eval("test");
    assert_eq!(exit_code_for(&eval_err), 3);

    let other_err = CliError::other("test");
    assert_eq!(exit_code_for(&other_err), 3);
}

#[test]
fn test_infrastructure_error_exit_codes_and_json_codes() {
    assert_eq!(EXIT_LOCKED, 4);
    assert_eq!(EXIT_INFRASTRUCTURE, 5);

    let locked = CliError::infrastructure("held", None, InfrastructureFailureKind::Locked);
    assert_eq!(exit_code_for(&locked), EXIT_LOCKED);
    assert_eq!(error_code_for(&locked), "infrastructure_locked");

    let cancelled = CliError::infrastructure(
        "apply cancelled",
        None,
        InfrastructureFailureKind::Cancelled,
    );
    assert_eq!(EXIT_CANCELLED, 1);
    assert_eq!(exit_code_for(&cancelled), EXIT_CANCELLED);
    assert_eq!(error_code_for(&cancelled), "infrastructure_cancelled");

    let interrupted =
        CliError::infrastructure("interrupted", None, InfrastructureFailureKind::Interrupted);
    assert_eq!(EXIT_INTERRUPTED, 130);
    assert_eq!(exit_code_for(&interrupted), EXIT_INTERRUPTED);
    assert_eq!(error_code_for(&interrupted), "infrastructure_interrupted");

    let failed = CliError::infrastructure("provider", None, InfrastructureFailureKind::Failed);
    assert_eq!(exit_code_for(&failed), EXIT_INFRASTRUCTURE);
    assert_eq!(error_code_for(&failed), "infrastructure");

    assert_eq!(error_code_for(&CliError::config("c")), "config");
    assert_eq!(error_code_for(&CliError::eval("e")), "eval");
    assert_eq!(error_code_for(&CliError::other("o")), "other");
}

#[test]
fn test_infrastructure_error_with_help_keeps_its_kind() {
    let error = CliError::infrastructure("held", None, InfrastructureFailureKind::Locked)
        .with_help("wait for the other run");
    let CliError::Infrastructure {
        message,
        help,
        kind,
        lock,
        ..
    } = &error
    else {
        panic!("Expected Infrastructure error");
    };
    assert_eq!(message, "held");
    assert_eq!(help.as_deref(), Some("wait for the other run"));
    assert_eq!(*kind, InfrastructureFailureKind::Locked);
    assert!(lock.is_none());
    assert_eq!(exit_code_for(&error), EXIT_LOCKED);
    assert!(format!("{error}").contains("Infrastructure error: held"));
}

#[test]
fn test_error_envelope_carries_help_and_lock() {
    let error = CliError::infrastructure(
        "interrupted",
        Some("release it with `cuenv infrastructure unlock abc`".to_string()),
        InfrastructureFailureKind::Interrupted,
    )
    .with_lock(LockStatus {
        identifier: "abc".to_string(),
        released: false,
    })
    .with_help("release it with `cuenv infrastructure unlock abc`");
    let envelope = serde_json::to_value(error_envelope(&error)).unwrap();
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "infrastructure_interrupted");
    assert_eq!(
        envelope["error"]["help"],
        "release it with `cuenv infrastructure unlock abc`"
    );
    assert_eq!(envelope["error"]["lockIdentifier"], "abc");
    assert_eq!(envelope["error"]["lockReleased"], false);

    let plain = serde_json::to_value(error_envelope(&CliError::config("bad"))).unwrap();
    assert_eq!(plain["error"]["code"], "config");
    assert!(plain["error"].get("help").is_none());
    assert!(plain["error"].get("lockIdentifier").is_none());
    // A lock only attaches to infrastructure errors.
    let config = CliError::config("bad").with_lock(LockStatus {
        identifier: "abc".to_string(),
        released: true,
    });
    assert!(matches!(config, CliError::Config { .. }));
}

#[test]
fn the_infrastructure_command_is_marked_experimental_in_help_and_keeps_its_alias() {
    use clap::CommandFactory;
    let command = Cli::command();
    let infrastructure = command.find_subcommand("infrastructure").unwrap();
    let about = infrastructure.get_about().unwrap().to_string();
    assert!(about.contains("experimental"), "{about}");
    let long = infrastructure.get_long_about().unwrap().to_string();
    assert!(long.contains("EXPERIMENTAL"), "{long}");
    assert!(infrastructure.get_all_aliases().any(|alias| alias == "i"));
}

/// Render an error through miette's graphical handler at a narrow width, so
/// a long secret has to wrap.
fn wrapped_report(error: &CliError) -> String {
    let mut text = String::new();
    miette::GraphicalReportHandler::new()
        .with_width(36)
        .render_report(&mut text, error)
        .unwrap();
    text
}

#[test]
fn errors_are_redacted_when_built_so_wrapping_cannot_split_a_secret() {
    let secret = "LONGSECRET-aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-END9";
    cuenv_events::register_secret(secret);
    let message = format!("cannot create object: open /nonexistent/{secret}/ordered-parent");
    let help = format!("check {secret} before retrying");
    let errors = [
        CliError::config(message.clone()),
        CliError::config_with_help(message.clone(), help.clone()),
        CliError::eval(message.clone()),
        CliError::eval_with_help(message.clone(), help.clone()),
        CliError::other(message.clone()),
        CliError::other_with_help(message.clone(), help.clone()),
        CliError::infrastructure(
            message.clone(),
            Some(help.clone()),
            InfrastructureFailureKind::Failed,
        ),
        CliError::config("plain").with_help(help),
    ];
    for error in &errors {
        let text = wrapped_report(error);
        assert!(!text.contains("LONGSECRET"), "wrapped report leaks: {text}");
        assert!(!text.contains("END9"), "wrapped report leaks: {text}");
        assert!(!error.to_string().contains("LONGSECRET"));
        assert!(!error.help().unwrap_or_default().contains("LONGSECRET"));
    }
}

#[test]
fn a_secret_registered_after_the_error_was_built_is_still_redacted_when_shown() {
    let error = CliError::infrastructure(
        "apply failed: open /x/late-secret-VALUE-zzzz/y",
        Some("see late-secret-VALUE-zzzz".to_string()),
        InfrastructureFailureKind::Failed,
    );
    cuenv_events::register_secret("late-secret-VALUE-zzzz");
    let text = error_report_text(&error);
    assert!(!text.contains("late-secret"), "{text}");
    let envelope = serde_json::to_string(&error_envelope(&error)).unwrap();
    assert!(!envelope.contains("late-secret"), "{envelope}");
}

#[test]
fn json_error_envelopes_redact_secrets_that_json_escapes() {
    // The envelope text holds the secret escaped (quo\"te\\back), so a search
    // for the raw secret in the serialized text would never find it.
    let error = CliError::infrastructure(
        "open /x/quo\"te\\back-QQQQ-escaped/y",
        Some("line\none quo\"te\\back-QQQQ-escaped".to_string()),
        InfrastructureFailureKind::Failed,
    );
    cuenv_events::register_secret("quo\"te\\back-QQQQ-escaped");
    let envelope = serde_json::to_string(&error_envelope(&error)).unwrap();
    assert!(!envelope.contains("QQQQ"), "{envelope}");
    let value: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/x/*_*/y")
    );
}

#[test]
fn deleted_not_recreated_replacements_reach_the_text_help_and_the_json_envelope() {
    let error = CliError::infrastructure(
        "apply failed",
        Some("fix the failure".to_string()),
        InfrastructureFailureKind::Failed,
    )
    .with_deleted_not_recreated(vec!["local_file.a".to_string(), "random_pet.b".to_string()]);
    let help = error.help().unwrap();
    assert!(help.starts_with("fix the failure "), "{help}");
    assert!(
        help.contains("Deleted but NOT recreated: local_file.a, random_pet.b"),
        "{help}"
    );
    let envelope = serde_json::to_value(error_envelope(&error)).unwrap();
    assert_eq!(
        envelope["error"]["deletedNotRecreated"],
        serde_json::json!(["local_file.a", "random_pet.b"])
    );
    // Nothing to report, nothing added; and only infrastructure errors carry it.
    let none = CliError::infrastructure("x", None, InfrastructureFailureKind::Failed)
        .with_deleted_not_recreated(Vec::new());
    let envelope = serde_json::to_value(error_envelope(&none)).unwrap();
    assert!(envelope["error"].get("deletedNotRecreated").is_none());
    let config = CliError::config("x").with_deleted_not_recreated(vec!["a.b".to_string()]);
    assert!(matches!(config, CliError::Config { .. }));
}

#[test]
fn test_error_display() {
    let config_err = CliError::config("test config message");
    let display = format!("{config_err}");
    assert!(display.contains("CLI/configuration error"));
    assert!(display.contains("test config message"));

    let eval_err = CliError::eval("test eval message");
    let display = format!("{eval_err}");
    assert!(display.contains("Evaluation/FFI error"));
    assert!(display.contains("test eval message"));
}

#[test]
fn test_cuenv_core_error_conversion() {
    // Configuration errors should map to Config (exit code 2)
    // and extract just the message (not the full "Configuration error: X")
    let config_err = cuenv_core::Error::configuration("Task 'foo' not found");
    let cli_err: CliError = config_err.into();
    assert!(matches!(cli_err, CliError::Config { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_CLI);
    // Verify we don't have redundant prefix
    let display = format!("{cli_err}");
    assert!(!display.contains("Configuration error: Configuration error"));
    assert!(display.contains("Task 'foo' not found"));

    // FFI errors should map to Eval (exit code 3)
    let ffi_err = cuenv_core::Error::ffi("evaluate", "FFI bridge failed");
    let cli_err: CliError = ffi_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

    // CUE parse errors should map to Eval (exit code 3)
    let cue_err = cuenv_core::Error::cue_parse(std::path::Path::new("/test"), "parse failed");
    let cli_err: CliError = cue_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

    // Validation errors should map to Eval (exit code 3)
    let validation_err = cuenv_core::Error::validation("schema validation failed");
    let cli_err: CliError = validation_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

    // I/O errors should map to Other (exit code 3)
    let io_err = cuenv_core::Error::io(
        "read",
        std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
    );
    let cli_err: CliError = io_err.into();
    assert!(matches!(cli_err, CliError::Other { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

    // Timeout errors should map to Other (exit code 3)
    let timeout_err = cuenv_core::Error::timeout(30);
    let cli_err: CliError = timeout_err.into();
    assert!(matches!(cli_err, CliError::Other { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

    // Execution errors should map to Eval (exit code 3)
    let exec_err = cuenv_core::Error::execution("Dagger execution failed");
    let cli_err: CliError = exec_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);
    // Verify message extraction
    let display = format!("{cli_err}");
    assert!(display.contains("Dagger execution failed"));
    assert!(!display.contains("Task execution failed: Task execution failed"));

    // Task graph errors should map to Config (exit code 2)
    let graph_err = cuenv_core::Error::task_graph("cycle detected");
    let cli_err: CliError = graph_err.into();
    assert!(matches!(cli_err, CliError::Config { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_CLI);

    // Task failures should map to Eval (exit code 3)
    let failed_err = cuenv_core::Error::task_failed("build", 1, "", "boom");
    let cli_err: CliError = failed_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);
    let display = format!("{cli_err}");
    assert!(display.contains("Task 'build' failed with exit code 1"));
    assert!(display.contains("boom"));

    // Secret resolution errors should map to Eval (exit code 3)
    let secret_err = cuenv_core::Error::secret_resolution("provider unavailable");
    let cli_err: CliError = secret_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);
}

#[test]
fn test_output_format_display() {
    assert_eq!(OutputFormat::Json.to_string(), "json");
    assert_eq!(OutputFormat::Env.to_string(), "env");
    assert_eq!(OutputFormat::Text.to_string(), "text");
    assert_eq!(OutputFormat::Rich.to_string(), "rich");
}

#[test]
fn test_output_format_as_ref() {
    assert_eq!(OutputFormat::Json.as_ref(), "json");
    assert_eq!(OutputFormat::Env.as_ref(), "env");
    assert_eq!(OutputFormat::Text.as_ref(), "text");
    assert_eq!(OutputFormat::Rich.as_ref(), "rich");
}

#[test]
fn test_status_format_display() {
    assert_eq!(StatusFormat::Text.to_string(), "text");
    assert_eq!(StatusFormat::Short.to_string(), "short");
    assert_eq!(StatusFormat::Starship.to_string(), "starship");
}

#[test]
fn test_status_format_default() {
    assert_eq!(StatusFormat::default(), StatusFormat::Text);
}

#[test]
fn test_cli_error_with_help_method() {
    // Test adding help to Config error
    let config_err = CliError::config("original config error");
    let with_help = config_err.with_help("try running with --fix");
    if let CliError::Config { message, help } = with_help {
        assert_eq!(message, "original config error");
        assert_eq!(help, Some("try running with --fix".to_string()));
    } else {
        panic!("Expected Config error");
    }

    // Test adding help to Eval error
    let eval_err = CliError::eval("eval error");
    let with_help = eval_err.with_help("check your CUE syntax");
    if let CliError::Eval { message, help } = with_help {
        assert_eq!(message, "eval error");
        assert_eq!(help, Some("check your CUE syntax".to_string()));
    } else {
        panic!("Expected Eval error");
    }

    // Test adding help to Other error
    let other_err = CliError::other("other error");
    let with_help = other_err.with_help("contact support");
    if let CliError::Other { message, help } = with_help {
        assert_eq!(message, "other error");
        assert_eq!(help, Some("contact support".to_string()));
    } else {
        panic!("Expected Other error");
    }
}

#[test]
fn test_cli_error_other_with_help() {
    let err = CliError::other_with_help("something went wrong", "try again later");
    if let CliError::Other { message, help } = err {
        assert_eq!(message, "something went wrong");
        assert_eq!(help, Some("try again later".to_string()));
    } else {
        panic!("Expected Other error");
    }
}

#[test]
fn test_cuenv_core_io_error_with_path() {
    // Test I/O error with a path
    let io_err = cuenv_core::Error::io_with_path(
        "write",
        std::path::Path::new("/etc/secrets"),
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied"),
    );
    let cli_err: CliError = io_err.into();
    let display = format!("{cli_err}");
    assert!(display.contains("I/O write failed"));
    assert!(display.contains("/etc/secrets"));
}

#[test]
fn test_cuenv_core_tool_resolution_error_without_help() {
    let tool_err = cuenv_core::Error::tool_resolution("tool not found");
    let cli_err: CliError = tool_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    let display = format!("{cli_err}");
    assert!(display.contains("tool not found"));
}

#[test]
fn test_cuenv_core_tool_resolution_error_with_help() {
    let tool_err =
        cuenv_core::Error::tool_resolution_with_help("tool not found", "install via brew");
    let cli_err: CliError = tool_err.into();
    if let CliError::Eval { message, help } = cli_err {
        assert_eq!(message, "tool not found");
        assert_eq!(help, Some("install via brew".to_string()));
    } else {
        panic!("Expected Eval error");
    }
}

#[test]
fn test_cuenv_core_platform_error() {
    let platform_err = cuenv_core::Error::platform("unsupported architecture");
    let cli_err: CliError = platform_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    let display = format!("{cli_err}");
    assert!(display.contains("unsupported architecture"));
}

#[test]
fn test_cuenv_core_utf8_error() {
    let mut invalid_bytes = Vec::from("valid");
    invalid_bytes[0] = 0xff;
    let utf8_error = std::str::from_utf8(&invalid_bytes).unwrap_err();
    let utf8_err = cuenv_core::Error::from(utf8_error);
    let cli_err: CliError = utf8_err.into();
    assert!(matches!(cli_err, CliError::Other { .. }));
}

#[test]
fn test_commands_package_method() {
    // Test commands that have package parameter
    let task_cmd = Commands::Task {
        name: Some("build".to_string()),
        path: ".".to_string(),
        package: "mypackage".to_string(),
        labels: vec![],
        output_format: Some(OutputFormat::Text),
        materialize_outputs: None,
        show_cache_path: false,
        backend: None,
        tui: false,
        interactive: false,
        help: false,
        skip_dependencies: false,
        continue_on_error: false,
        dry_run: false,
        task_args: vec![],
    };
    assert_eq!(task_cmd.package(), "mypackage");

    // Test commands without package parameter
    let version_cmd = Commands::Version {
        output_format: OutputFormat::Text,
    };
    assert_eq!(version_cmd.package(), "cuenv"); // default
}

#[test]
fn test_task_command_with_labels() {
    let cli = Cli::try_parse_from(["cuenv", "task", "--label", "ci", "--label", "test", "build"])
        .unwrap();

    if let Some(Commands::Task { labels, name, .. }) = cli.command {
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"ci".to_string()));
        assert!(labels.contains(&"test".to_string()));
        assert_eq!(name, Some("build".to_string()));
    } else {
        panic!("Expected Task command");
    }
}

#[test]
fn test_task_command_interactive_flag() {
    let cli = Cli::try_parse_from(["cuenv", "task", "-i"]).unwrap();

    if let Some(Commands::Task { interactive, .. }) = cli.command {
        assert!(interactive);
    } else {
        panic!("Expected Task command");
    }
}

#[test]
fn test_sync_lock_update_flag() {
    // Test -u alone (update all)
    let cli = Cli::try_parse_from(["cuenv", "sync", "lock", "-u"]).unwrap();
    if let Some(Commands::Sync {
        subcommand: Some(SyncCommands::Lock { update, .. }),
        ..
    }) = cli.command
    {
        // -u alone should give Some(vec![]) or Some(vec![""]) depending on clap behavior
        assert!(update.is_some());
    } else {
        panic!("Expected Sync Lock command");
    }
}

#[test]
fn test_release_binaries_command() {
    let cli = Cli::try_parse_from([
        "cuenv",
        "release",
        "binaries",
        "--dry-run",
        "--build-only",
        "--target",
        "x86_64-unknown-linux-gnu,aarch64-apple-darwin",
    ])
    .unwrap();

    if let Some(Commands::Release {
        subcommand:
            ReleaseCommands::Binaries {
                dry_run,
                build_only,
                target,
                ..
            },
    }) = cli.command
    {
        assert!(dry_run);
        assert!(build_only);
        assert!(target.is_some());
        let targets = target.unwrap();
        assert_eq!(targets.len(), 2);
    } else {
        panic!("Expected Release Binaries command");
    }
}

#[test]
fn test_changeset_add_package_parsing() {
    let cmd = Commands::Changeset {
        subcommand: ChangesetCommands::Add {
            path: ".".to_string(),
            summary: Some("test summary".to_string()),
            description: None,
            packages: vec![
                "pkg-a:minor".to_string(),
                "pkg-b:patch".to_string(),
                "invalid".to_string(), // no colon, should be filtered
            ],
        },
    };

    let command = cmd.into_command(None);
    if let Command::ChangesetAdd { packages, .. } = command {
        assert_eq!(packages.len(), 2); // invalid one filtered out
        assert!(packages.contains(&("pkg-a".to_string(), "minor".to_string())));
        assert!(packages.contains(&("pkg-b".to_string(), "patch".to_string())));
    } else {
        panic!("Expected ChangesetAdd command");
    }
}

#[test]
fn test_infrastructure_provider_command_conversion() {
    use crate::commands::infrastructure::ProviderAction;

    let convert = |arguments: &[&str]| {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let Command::InfrastructureProvider { path, action } =
            cli.command.unwrap().into_command(None)
        else {
            panic!("Expected Command::InfrastructureProvider");
        };
        (path, action)
    };
    assert_eq!(
        convert(&["cuenv", "i", "provider", "add", "hashicorp/random@3.9.1"]),
        (
            ".".to_string(),
            ProviderAction::Add {
                release: "hashicorp/random@3.9.1".to_string()
            }
        )
    );
    assert_eq!(
        convert(&[
            "cuenv",
            "infrastructure",
            "provider",
            "remove",
            "hashicorp/random",
            "-p",
            "infra"
        ]),
        (
            "infra".to_string(),
            ProviderAction::Remove {
                source: "hashicorp/random".to_string()
            }
        )
    );
    assert!(Cli::try_parse_from(["cuenv", "i", "provider", "add"]).is_err());
}

#[test]
fn test_sync_infrastructure_command_conversion() {
    use crate::commands::sync::SyncMode;

    let cli = Cli::try_parse_from(["cuenv", "sync", "infrastructure", "--check"]).unwrap();
    let Command::Sync {
        subcommand, mode, ..
    } = cli.command.unwrap().into_command(None)
    else {
        panic!("Expected Command::Sync");
    };
    assert_eq!(subcommand.as_deref(), Some("infrastructure"));
    assert_eq!(mode, SyncMode::Check);
}
