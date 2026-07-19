use std::ffi::OsString;

use clap::{Parser, Subcommand, ValueEnum};

use crate::model::Provider;

#[derive(Debug, Parser)]
#[command(
    name = "sesh",
    version,
    about = "Switch coding providers without losing your place"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run {
        provider: Provider,
        #[arg(last = true, allow_hyphen_values = true)]
        provider_args: Vec<OsString>,
    },
    Switch {
        provider: Provider,
        #[arg(last = true, allow_hyphen_values = true)]
        provider_args: Vec<OsString>,
    },
    Checkpoint {
        #[arg(long, value_enum, default_value = "json")]
        format: CheckpointFormat,
        #[arg(long)]
        from_provider: bool,
    },
    #[command(name = "__hook", hide = true)]
    Hook { provider: Provider },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CheckpointFormat {
    Json,
}
