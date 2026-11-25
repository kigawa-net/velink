mod auth;
mod endpoint;
mod platform;
mod remote_client;
mod settings_gui;

use velink_core::{BrowserKind, BrowserLaunch, launch_local_browser};

pub fn run() -> Result<(), String> {
    platform::enable_dpi_awareness();
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("settings") {
        if args.next().is_some() {
            return Err("unexpected extra arguments".into());
        }
        return open_settings();
    }
    if first.is_none() || first.as_deref() == Some("remote") {
        let cli = args.next();
        if args.next().is_some() {
            return Err("unexpected extra arguments".into());
        }
        let config = endpoint::default_config_path();
        let url = match endpoint::configured_endpoint(cli, config.as_deref())? {
            Some(url) => url,
            None => match settings_gui::open(config.clone()) {
                Ok(Some(url)) => url,
                Ok(None) => return Ok(()),
                Err(error) => {
                    eprintln!("velink: could not open settings: {error}");
                    endpoint::prompt(
                        config.as_deref(),
                        &mut std::io::stdin().lock(),
                        &mut std::io::stdout(),
                    )?
                }
            },
        };
        // Only TLS endpoints are authenticated; ws:// stays unauthenticated (LAN use).
        let credentials = if url.starts_with("wss://") {
            let cfg = auth::AuthConfig::from_environment(config.as_deref());
            let token_path = auth::token_path_from(config)
                .ok_or("cannot determine the config directory for token.json")?;
            let access_token =
                auth::obtain_access_token(&cfg, &token_path, &mut std::io::stdout())?;
            Some(remote_client::Credentials {
                access_token,
                token_path: Some(token_path),
            })
        } else {
            None
        };
        return remote_client::run(&url, credentials);
    }
    let browser = first.unwrap();
    let Some(url) = args.next() else {
        return Err("missing URL (usage: velink-desktop [remote [ws[s]://host/vnc]] | settings | <firefox|chromium> <url>)".into());
    };
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
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

/// Open settings without starting a remote session.
pub fn open_settings() -> Result<(), String> {
    platform::enable_dpi_awareness();
    settings_gui::open(endpoint::default_config_path()).map(|_| ())
}
