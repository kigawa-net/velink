//! Resolution of the remote endpoint: CLI argument > config file > stdin prompt.
use std::{
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use velink_core::remote::RemoteSession;

pub const PROMPT: &str = "velink server URL (ws://host:port/vnc or wss://host/vnc): ";

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

/// Location of `config.toml` given the relevant environment values.
pub fn config_path(
    windows: bool,
    appdata: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    let base = if windows {
        PathBuf::from(non_empty(appdata)?)
    } else if let Some(xdg) = non_empty(xdg_config_home) {
        PathBuf::from(xdg)
    } else {
        Path::new(non_empty(home)?).join(".config")
    };
    Some(base.join("velink").join("config.toml"))
}

pub fn default_config_path() -> Option<PathBuf> {
    let var = |k: &str| std::env::var(k).ok();
    config_path(
        cfg!(windows),
        var("APPDATA").as_deref(),
        var("XDG_CONFIG_HOME").as_deref(),
        var("HOME").as_deref(),
    )
}

fn config_entry(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

/// Read a quoted string from the existing line-based configuration format.
pub fn config_string(text: &str, wanted: &str) -> Option<String> {
    let (_, value) = text
        .lines()
        .filter_map(config_entry)
        .find(|(key, _)| *key == wanted)?;
    let value = value.strip_prefix('"')?;
    let (value, _) = value.split_once('"')?;
    Some(value.to_owned())
}

pub fn read_config(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        result => result,
    }
}

pub fn write_endpoint(path: &Path, url: &str) -> io::Result<()> {
    write_config(path, &[("endpoint", url)])
}

pub fn write_config(path: &Path, updates: &[(&str, &str)]) -> io::Result<()> {
    for (key, value) in updates {
        if value.contains('"') || value.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{key} cannot contain quotes or control characters"),
            ));
        }
    }

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    // Lock a stable file because renaming the config replaces its file handle.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("toml.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;

    let previous = read_config(path)?;
    let mut text = String::with_capacity(previous.len());
    let mut written = vec![false; updates.len()];
    for line in previous.split_inclusive('\n') {
        let update = config_entry(line)
            .and_then(|(key, _)| updates.iter().position(|(name, _)| *name == key));
        match update {
            Some(index) if !written[index] => {
                let (key, value) = updates[index];
                let ending = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
                text.push_str(&format!("{key} = \"{value}\"{ending}"));
                written[index] = true;
            }
            Some(_) => {} // Drop duplicate definitions of updated keys.
            None => text.push_str(line),
        }
    }
    for (&(key, value), written) in updates.iter().zip(written) {
        if !written {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&format!("{key} = \"{value}\"\n"));
        }
    }

    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn validate(endpoint: &str) -> Result<String, String> {
    let session = RemoteSession::new(endpoint)?;
    endpoint
        .into_client_request()
        .map_err(|error| error.to_string())?;
    Ok(session.endpoint)
}

fn validate_from(endpoint: &str, source: &str) -> Result<String, String> {
    validate(endpoint)
        .map_err(|error| format!("invalid endpoint {endpoint:?} (from {source}): {error}"))
}

/// Check the CLI and then the config file for an endpoint.
pub fn configured_endpoint(
    cli: Option<String>,
    config: Option<&Path>,
) -> Result<Option<String>, String> {
    if let Some(url) = cli {
        return validate_from(&url, "command line").map(Some);
    }
    let Some(path) = config else { return Ok(None) };
    let text =
        read_config(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    config_string(&text, "endpoint")
        .filter(|url| !url.trim().is_empty())
        .map(|url| validate_from(&url, &format!("config file {}", path.display())))
        .transpose()
}

pub fn prompt(
    config: Option<&Path>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<String, String> {
    write!(output, "{PROMPT}")
        .and_then(|_| output.flush())
        .map_err(|error| error.to_string())?;
    let mut line = String::new();
    if input
        .read_line(&mut line)
        .map_err(|error| error.to_string())?
        == 0
    {
        return Err("no endpoint given (pass `remote <ws[s]://host/vnc>`, set `endpoint` in the config file, or enter it at the prompt)".into());
    }
    let url = validate_from(line.trim(), "prompt")?;
    if let Some(path) = config {
        if let Err(error) = write_endpoint(path, &url) {
            eprintln!(
                "velink: warning: cannot save endpoint to {}: {error}",
                path.display()
            );
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cli: Option<&str>, cfg: Option<&Path>, stdin: &str) -> Result<String, String> {
        match configured_endpoint(cli.map(str::to_owned), cfg)? {
            Some(url) => Ok(url),
            None => prompt(cfg, &mut stdin.as_bytes(), &mut Vec::new()),
        }
    }

    #[test]
    fn cli_wins_over_config() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "endpoint = \"ws://cfg:1/vnc\"\n").unwrap();
        assert_eq!(
            run(Some("ws://cli:2/vnc"), Some(&p), "").unwrap(),
            "ws://cli:2/vnc"
        );
    }

    #[test]
    fn config_then_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "# c\nendpoint = \"ws://cfg:1/vnc\"\n").unwrap();
        assert_eq!(run(None, Some(&p), "").unwrap(), "ws://cfg:1/vnc");
        let missing = p.with_file_name("none.toml");
        assert_eq!(
            run(None, Some(&missing), "ws://in:3/vnc\n").unwrap(),
            "ws://in:3/vnc"
        );
        assert_eq!(run(None, Some(&missing), "").unwrap(), "ws://in:3/vnc");
        assert!(run(None, None, "").is_err());
    }

    #[test]
    fn invalid_url_is_rejected() {
        assert!(
            run(Some("http://x/vnc"), None, "")
                .unwrap_err()
                .contains("invalid endpoint")
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "endpoint = \"ws://h:1/other\"\n").unwrap();
        assert!(run(None, Some(&p), "").is_err());
        assert!(run(None, None, "nonsense\n").is_err());
    }

    #[test]
    fn wss_is_accepted_and_http_rejected() {
        assert_eq!(
            run(Some("wss://velink.kigawa.net/vnc"), None, "").unwrap(),
            "wss://velink.kigawa.net/vnc"
        );
        assert!(run(Some("https://velink.kigawa.net/vnc"), None, "").is_err());
    }

    #[test]
    fn creates_and_overwrites_endpoint_preserving_other_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        write_endpoint(&path, "ws://first/vnc").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "endpoint = \"ws://first/vnc\"\n"
        );

        let before = "# keep\r\nissuer = \"https://issuer\"\r\nendpoint = \"ws://first/vnc\"\r\nclient_id = \"client\"\nscope = \"openid\"";
        fs::write(&path, before).unwrap();
        write_endpoint(&path, "wss://second/vnc").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            before.replace("ws://first/vnc", "wss://second/vnc")
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_is_private_on_creation_and_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mode = || fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        write_endpoint(&path, "ws://first/vnc").unwrap();
        assert_eq!(mode(), 0o600);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_endpoint(&path, "ws://second/vnc").unwrap();
        assert_eq!(mode(), 0o600);
    }

    #[test]
    fn prompt_returns_valid_endpoint_even_if_config_cannot_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let invalid_url = "http://example/vnc\n";
        assert!(run(None, Some(&path), invalid_url).is_err());
        assert!(!path.exists());

        let parent_is_file = dir.path().join("file");
        fs::write(&parent_is_file, "").unwrap();
        let endpoint = prompt(
            Some(&parent_is_file.join("config.toml")),
            &mut "ws://example/vnc\n".as_bytes(),
            &mut Vec::new(),
        );
        assert_eq!(endpoint.unwrap(), "ws://example/vnc");
    }

    #[test]
    fn paths() {
        assert_eq!(
            config_path(true, Some("C:\\A"), None, None).unwrap(),
            PathBuf::from("C:\\A").join("velink").join("config.toml")
        );
        assert_eq!(
            config_path(false, None, Some("/x"), Some("/h")).unwrap(),
            PathBuf::from("/x/velink/config.toml")
        );
        assert_eq!(
            config_path(false, None, None, Some("/h")).unwrap(),
            PathBuf::from("/h/.config/velink/config.toml")
        );
    }
}
