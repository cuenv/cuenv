use crate::commands::Command;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "cuenv")]
#[command(
    about = "A modern application build toolchain with typed environments and CUE-powered task orchestration"
)]
#[command(long_about = None)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[arg(
        short = 'l',
        long,
        global = true,
        help = "Set logging level",
        default_value = "warn",
        value_enum
    )]
    pub level: crate::tracing::LogLevel,

    #[arg(long, global = true, help = "Output format", default_value = "auto")]
    pub format: String,

    #[arg(long, global = true, help = "Output logs in JSON format")]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Show version information")]
    Version,
    #[command(about = "Environment variable operations")]
    Env {
        #[command(subcommand)]
        subcommand: EnvCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum EnvCommands {
    #[command(about = "Print environment variables from CUE package")]
    Print {
        #[arg(
            long,
            short = 'p',
            help = "Path to directory containing CUE files",
            default_value = "."
        )]
        path: String,
        #[arg(
            long,
            help = "Name of the CUE package to evaluate",
            default_value = "cuenv"
        )]
        package: String,
        #[arg(long, help = "Output format (env, json)", default_value = "env")]
        format: String,
    },
}

impl From<Commands> for Command {
    fn from(cmd: Commands) -> Self {
        match cmd {
            Commands::Version => Command::Version,
            Commands::Env { subcommand } => match subcommand {
                EnvCommands::Print {
                    path,
                    package,
                    format,
                } => Command::EnvPrint {
                    path,
                    package,
                    format,
                },
            },
        }
    }
}

pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::LogLevel;
    use clap::Parser;

    #[test]
    fn test_cli_default_values() {
        let cli = Cli::try_parse_from(&["cuenv", "version"]).unwrap();

        assert!(matches!(cli.level, LogLevel::Warn)); // Default log level
        assert_eq!(cli.format, "auto"); // Default format
        assert!(!cli.json); // Default JSON is false
        assert!(matches!(cli.command, Commands::Version));
    }

    #[test]
    fn test_cli_log_level_parsing() {
        // Test each level individually
        let cli = Cli::try_parse_from(&["cuenv", "--level", "trace", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Trace));

        let cli = Cli::try_parse_from(&["cuenv", "--level", "debug", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Debug));

        let cli = Cli::try_parse_from(&["cuenv", "--level", "info", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Info));

        let cli = Cli::try_parse_from(&["cuenv", "--level", "warn", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Warn));

        let cli = Cli::try_parse_from(&["cuenv", "--level", "error", "version"]).unwrap();
        assert!(matches!(cli.level, LogLevel::Error));

        // Test short form for a few cases
        let cli_short = Cli::try_parse_from(&["cuenv", "-l", "debug", "version"]).unwrap();
        assert!(matches!(cli_short.level, LogLevel::Debug));

        let cli_short = Cli::try_parse_from(&["cuenv", "-l", "error", "version"]).unwrap();
        assert!(matches!(cli_short.level, LogLevel::Error));
    }

    #[test]
    fn test_cli_json_flag() {
        let cli = Cli::try_parse_from(&["cuenv", "--json", "version"]).unwrap();
        assert!(cli.json);

        let cli_no_json = Cli::try_parse_from(&["cuenv", "version"]).unwrap();
        assert!(!cli_no_json.json);
    }

    #[test]
    fn test_cli_format_option() {
        let cli = Cli::try_parse_from(&["cuenv", "--format", "custom", "version"]).unwrap();
        assert_eq!(cli.format, "custom");
    }

    #[test]
    fn test_cli_combined_flags() {
        let cli = Cli::try_parse_from(&[
            "cuenv",
            "--level",
            "debug",
            "--json",
            "--format",
            "structured",
            "version",
        ])
        .unwrap();

        assert!(matches!(cli.level, LogLevel::Debug));
        assert!(cli.json);
        assert_eq!(cli.format, "structured");
        assert!(matches!(cli.command, Commands::Version));
    }

    #[test]
    fn test_command_conversion() {
        let version_cmd = Commands::Version;
        let command: Command = version_cmd.into();
        assert!(matches!(command, Command::Version));
    }

    #[test]
    fn test_invalid_log_level() {
        let result = Cli::try_parse_from(&["cuenv", "--level", "invalid", "version"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_subcommand() {
        let result = Cli::try_parse_from(&["cuenv"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_help_flag() {
        let result = Cli::try_parse_from(&["cuenv", "--help"]);
        // Help flag should cause an error with help message
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.kind() == clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn test_env_print_command_default() {
        let cli = Cli::try_parse_from(&["cuenv", "env", "print"]).unwrap();

        if let Commands::Env { subcommand } = cli.command {
            let EnvCommands::Print {
                path,
                package,
                format,
            } = subcommand;
            assert_eq!(path, ".");
            assert_eq!(package, "cuenv");
            assert_eq!(format, "env");
        } else {
            panic!("Expected Env command");
        }
    }

    #[test]
    fn test_env_print_command_with_options() {
        let cli = Cli::try_parse_from(&[
            "cuenv",
            "env",
            "print",
            "--path",
            "examples/env-basic",
            "--package",
            "examples",
            "--format",
            "json",
        ])
        .unwrap();

        if let Commands::Env { subcommand } = cli.command {
            let EnvCommands::Print {
                path,
                package,
                format,
            } = subcommand;
            assert_eq!(path, "examples/env-basic");
            assert_eq!(package, "examples");
            assert_eq!(format, "json");
        } else {
            panic!("Expected Env command");
        }
    }

    #[test]
    fn test_env_print_command_short_path() {
        let cli = Cli::try_parse_from(&["cuenv", "env", "print", "-p", "test/path"]).unwrap();

        if let Commands::Env { subcommand } = cli.command {
            let EnvCommands::Print {
                path,
                package,
                format,
            } = subcommand;
            assert_eq!(path, "test/path");
            assert_eq!(package, "cuenv"); // default
            assert_eq!(format, "env"); // default
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
                format: "json".to_string(),
            },
        };
        let command: Command = env_cmd.into();

        if let Command::EnvPrint {
            path,
            package,
            format,
        } = command
        {
            assert_eq!(path, "test");
            assert_eq!(package, "pkg");
            assert_eq!(format, "json");
        } else {
            panic!("Expected EnvPrint command");
        }
    }
}
