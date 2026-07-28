use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::model::{Provider, Surface};

#[derive(Debug, Parser)]
#[command(
    name = "handover",
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
        #[arg(long)]
        recover_lease: bool,
        #[arg(last = true, allow_hyphen_values = true)]
        provider_args: Vec<OsString>,
    },
    Arm {
        provider: Provider,
        #[arg(long, value_enum, default_value = "auto")]
        surface: Surface,
        #[arg(long, default_value = crate::arm::DEFAULT_TTL)]
        ttl: String,
        #[arg(long)]
        json: bool,
    },
    Claim {
        #[arg(long)]
        arm: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Preview {
        provider: Provider,
        #[arg(long)]
        json: bool,
    },
    Fork {
        provider: Provider,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        worktree: Option<PathBuf>,
        #[arg(last = true, allow_hyphen_values = true)]
        provider_args: Vec<OsString>,
    },
    Checkpoint {
        #[arg(long, value_enum, default_value = "json")]
        format: CheckpointFormat,
        #[arg(long)]
        from_provider: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Log {
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        #[arg(long)]
        json: bool,
    },
    Delete {
        #[arg(long)]
        yes: bool,
    },
    Setup {
        provider: Provider,
    },
    Doctor {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        repair: bool,
    },
    #[command(name = "mcp-server")]
    McpServer,
    #[command(name = "__hook", hide = true)]
    Hook {
        provider: Provider,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CheckpointFormat {
    Json,
}
