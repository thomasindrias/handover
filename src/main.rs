use std::process::ExitCode;

fn main() -> ExitCode {
    match handover::run_from(std::env::args_os()) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("handover: {error}");
            ExitCode::FAILURE
        }
    }
}
