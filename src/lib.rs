pub mod app;
pub mod arm;
pub mod checkpoint;
pub mod cli;
pub mod doctor;
pub mod error;
pub mod fork;
pub mod git;
pub mod handover;
pub mod list;
pub mod mcp;
pub mod model;
pub mod provider;
pub mod runtime;
pub mod session;
pub mod store;
pub mod supervisor;

pub fn run_from<I, T>(args: I) -> crate::error::Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::Parser;
    let cli = crate::cli::Cli::parse_from(args);
    crate::app::run(
        cli,
        &crate::store::Environment::capture(),
        &crate::runtime::SystemRuntime,
    )
}
