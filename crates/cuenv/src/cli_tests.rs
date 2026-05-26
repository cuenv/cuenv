use super::*;
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
    assert!(err.kind() == clap::error::ErrorKind::DisplayHelp);
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
    let io_err = cuenv_core::Error::Io {
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"),
        path: None,
        operation: "read".to_string(),
    };
    let cli_err: CliError = io_err.into();
    assert!(matches!(cli_err, CliError::Other { .. }));
    assert_eq!(exit_code_for(&cli_err), EXIT_EVAL);

    // Timeout errors should map to Other (exit code 3)
    let timeout_err = cuenv_core::Error::Timeout { seconds: 30 };
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
    let io_err = cuenv_core::Error::Io {
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied"),
        path: Some(std::path::Path::new("/etc/secrets").into()),
        operation: "write".to_string(),
    };
    let cli_err: CliError = io_err.into();
    let display = format!("{cli_err}");
    assert!(display.contains("I/O write failed"));
    assert!(display.contains("/etc/secrets"));
}

#[test]
fn test_cuenv_core_tool_resolution_error_without_help() {
    let tool_err = cuenv_core::Error::ToolResolution {
        message: "tool not found".to_string(),
        help: None,
    };
    let cli_err: CliError = tool_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    let display = format!("{cli_err}");
    assert!(display.contains("tool not found"));
}

#[test]
fn test_cuenv_core_tool_resolution_error_with_help() {
    let tool_err = cuenv_core::Error::ToolResolution {
        message: "tool not found".to_string(),
        help: Some("install via brew".to_string()),
    };
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
    let platform_err = cuenv_core::Error::Platform {
        message: "unsupported architecture".to_string(),
    };
    let cli_err: CliError = platform_err.into();
    assert!(matches!(cli_err, CliError::Eval { .. }));
    let display = format!("{cli_err}");
    assert!(display.contains("unsupported architecture"));
}

#[test]
#[allow(invalid_from_utf8)]
fn test_cuenv_core_utf8_error() {
    // Create an actual UTF-8 error by parsing invalid bytes
    let invalid_bytes = [0xff, 0xfe];
    let utf8_error = std::str::from_utf8(&invalid_bytes).unwrap_err();
    let utf8_err = cuenv_core::Error::Utf8 {
        source: utf8_error,
        file: None,
    };
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
