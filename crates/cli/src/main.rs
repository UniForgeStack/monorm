//! The standalone `monorm` binary; see the crate docs (`lib.rs`) for the commands.

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(monorm_cli::run_blocking(&argv, monorm_cli::Host::Binary) as u8)
}
