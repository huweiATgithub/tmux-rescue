use std::process::ExitCode;

use anstream::{AutoStream, ColorChoice};
use clap::Parser;
use clap::error::ErrorKind;

mod cli;
mod inspect;

use cli::{Cli, EXIT_FAILURE, EXIT_SUCCESS, SystemCliRunner, TerminalColorSupport, dispatch};

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
    let color_support = TerminalColorSupport::new(
        AutoStream::choice(&stdout) != ColorChoice::Never,
        AutoStream::choice(&stderr) != ColorChoice::Never,
    );
    let mut runner = SystemCliRunner::with_color_support(&mut stdout, &mut stderr, color_support);
    ExitCode::from(dispatch(cli, &mut runner))
}
