#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    match velink_desktop::open_settings() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("velink-settings: {error}");
            ExitCode::FAILURE
        }
    }
}
