use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

mod cli;

use cli::{Cli, EXIT_FAILURE, EXIT_SUCCESS, SystemCliRunner, dispatch};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let status = match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => EXIT_SUCCESS,
                _ => EXIT_FAILURE,
            };
            let _ = error.print();
            return ExitCode::from(status);
        }
    };
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();
    let mut runner = SystemCliRunner::new(&mut stdout, &mut stderr);
    ExitCode::from(dispatch(cli, &mut runner))
}
