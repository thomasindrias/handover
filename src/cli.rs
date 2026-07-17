use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "sesh",
    version,
    about = "Switch coding providers without losing your place"
)]
pub struct Cli {}
