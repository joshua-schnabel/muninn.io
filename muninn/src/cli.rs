//! The command line.
//!
//! The command set is a stable contract (`docs/versioning.md`), so every command
//! is declared here even where its implementation is still to come — `--help`
//! should describe the tool, not the state of the build. The commands that are
//! not implemented say so and name the work package, rather than failing in a
//! way that looks like a bug.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "muninn",
    version,
    about = "Supervisor and configuration layer around Telegraf",
    long_about = "muninn reads one small YAML file, generates a complete Telegraf \
                  configuration from it, has Telegraf verify that configuration, and then \
                  supervises Telegraf as a child process."
)]
pub struct Cli {
    /// Path to the muninn configuration file.
    #[arg(
        short,
        long,
        env = "MUNINN_CONFIG",
        default_value = "/etc/muninn/muninn.yaml",
        global = true
    )]
    pub config: PathBuf,

    /// Log level: trace, debug, info, warn or error. Overrides the file.
    ///
    /// Deliberately an `Option` with no default: with a default, "not given" and
    /// "explicitly the default" become indistinguishable, and the value in the
    /// file could never win.
    #[arg(long, env = "MUNINN_LOG_LEVEL", global = true)]
    pub log_level: Option<String>,

    /// Log format: human or json. Overrides the file.
    #[arg(long, env = "MUNINN_LOG_FORMAT", global = true)]
    pub log_format: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run muninn: generate, validate, start Telegraf and supervise it.
    Run,

    /// Validate the configuration and exit. Starts nothing.
    Validate {
        /// Additionally have Telegraf check the generated configuration.
        ///
        /// Off by default because it needs the Telegraf binary, which in
        /// practice means running inside the image. Static validation catches
        /// everything an operator can actually get wrong in the YAML.
        #[arg(long)]
        with_telegraf: bool,
    },

    /// Print the generated Telegraf configuration. Starts nothing.
    RenderConfig {
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Include real secret values instead of `***`.
        ///
        /// For local debugging only. The output is a credential — do not paste
        /// it anywhere. Without this flag the output is safe to attach to an
        /// issue, which is the point.
        #[arg(long)]
        unsafe_show_secrets: bool,
    },

    /// Check that the host provides what the enabled modules need.
    CheckRuntime,

    /// Query the local health endpoint. For a container HEALTHCHECK.
    Healthcheck,

    /// Print the muninn and Telegraf versions.
    Version,
}

impl Cli {
    /// The selected subcommand; `run` is the default, because that is what the
    /// container does.
    ///
    /// Named `selected` rather than `command` so it does not shadow clap's
    /// `CommandFactory::command`, which the definition test needs.
    pub fn selected(&self) -> &Command {
        self.command.as_ref().unwrap_or(&Command::Run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        <Cli as CommandFactory>::command().debug_assert();
    }

    #[test]
    fn running_with_no_subcommand_means_run() {
        let cli = Cli::parse_from(["muninn"]);
        assert!(matches!(cli.selected(), Command::Run));
    }

    /// The documented precedence is CLI → ENV → YAML → default, which only works
    /// if "not given" is representable.
    #[test]
    fn log_overrides_default_to_none_so_the_file_can_win() {
        let cli = Cli::parse_from(["muninn", "validate"]);
        assert!(cli.log_level.is_none());
        assert!(cli.log_format.is_none());
    }

    #[test]
    fn render_config_redacts_unless_asked_not_to() {
        let cli = Cli::parse_from(["muninn", "render-config"]);
        match cli.selected() {
            Command::RenderConfig {
                unsafe_show_secrets,
                ..
            } => assert!(!unsafe_show_secrets, "redaction must be the default"),
            other => panic!("expected render-config, got {other:?}"),
        }
    }

    #[test]
    fn every_documented_command_parses() {
        for args in [
            vec!["muninn", "run"],
            vec!["muninn", "validate"],
            vec!["muninn", "validate", "--with-telegraf"],
            vec!["muninn", "render-config"],
            vec!["muninn", "render-config", "--output", "/tmp/x.conf"],
            vec!["muninn", "render-config", "--unsafe-show-secrets"],
            vec!["muninn", "check-runtime"],
            vec!["muninn", "healthcheck"],
            vec!["muninn", "version"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?} did not parse: {e}"));
        }
    }

    /// `--config` is global, so it works before or after the subcommand.
    #[test]
    fn the_config_path_can_be_given_on_either_side_of_the_subcommand() {
        let a = Cli::parse_from(["muninn", "--config", "/x.yaml", "validate"]);
        let b = Cli::parse_from(["muninn", "validate", "--config", "/x.yaml"]);
        assert_eq!(a.config, b.config);
    }
}
