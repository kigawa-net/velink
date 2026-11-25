use std::process::ExitCode;

fn main() -> ExitCode {
    match velink_desktop::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("velink: {message}");
            ExitCode::FAILURE
        }
    }
}
