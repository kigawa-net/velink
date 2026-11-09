mod remote_client;

use std::process::ExitCode;

use velink_core::{BrowserKind, BrowserLaunch, launch_local_browser};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("velink: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let Some(browser) = args.next() else {
        return Err(
            "usage: velink-desktop <firefox|chromium> <url> | remote <ws://host:8080/vnc>".into(),
        );
    };
    let Some(url) = args.next() else {
        return Err("missing URL".into());
    };
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
    }

    if browser == "remote" {
        return remote_client::run(&url);
    }

    let kind = match browser.as_str() {
        "firefox" => BrowserKind::Firefox,
        "chromium" => BrowserKind::Chromium,
        _ => return Err(format!("unsupported browser: {browser}")),
    };
    let launch = BrowserLaunch { kind, url };
    launch
        .validate()
        .map_err(|e| format!("invalid launch request: {e:?}"))?;

    let mut process = launch_local_browser(&launch).map_err(|e| e.to_string())?;
    println!(
        "started {kind:?} process {} for {}",
        process.id(),
        launch.url
    );
    // The shell remains alive to own and monitor the child process. Window
    // capture and embedding are provided by the upcoming Wayland adapter.
    loop {
        match process.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                println!("browser process exited: {status}");
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    }
    Ok(())
}
