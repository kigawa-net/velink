//! Resolution of the remote endpoint: CLI argument > config file > stdin prompt.
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};
use velink_core::remote::RemoteSession;

pub const PROMPT: &str = "velink server URL (ws://host:port/vnc): ";

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

/// Minimal line-based reader for `endpoint = "..."`.
pub fn parse_endpoint(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "endpoint" {
            continue;
        }
        let value = value.trim();
        let value = value.strip_prefix('"')?;
        let end = value.find('"')?;
        return Some(value[..end].to_owned());
    }
    None
}

fn read_config(path: Option<&Path>) -> Result<Option<String>, String> {
    let Some(path) = path else { return Ok(None) };
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_endpoint(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Resolve and validate the endpoint.
pub fn resolve(
    cli: Option<String>,
    config: Option<&Path>,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<String, String> {
    let (endpoint, source) = if let Some(url) = cli {
        (url, "command line".to_owned())
    } else if let Some(url) = read_config(config)? {
        (url, format!("config file {}", config.unwrap().display()))
    } else {
        write!(output, "{PROMPT}")
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        let n = input.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("no endpoint given (pass `remote <ws://host:port/vnc>`, set `endpoint` in the config file, or enter it at the prompt)".into());
        }
        (line.trim().to_owned(), "prompt".to_owned())
    };
    RemoteSession::new(endpoint.as_str())
        .map(|s| s.endpoint)
        .map_err(|e| format!("invalid endpoint {endpoint:?} (from {source}): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("velink-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    fn run(cli: Option<&str>, cfg: Option<&Path>, stdin: &str) -> Result<String, String> {
        resolve(
            cli.map(str::to_owned),
            cfg,
            &mut stdin.as_bytes(),
            &mut Vec::new(),
        )
    }

    #[test]
    fn cli_wins_over_config() {
        let p = tmp("cli", "endpoint = \"ws://cfg:1/vnc\"\n");
        assert_eq!(
            run(Some("ws://cli:2/vnc"), Some(&p), "").unwrap(),
            "ws://cli:2/vnc"
        );
    }

    #[test]
    fn config_then_prompt() {
        let p = tmp("cfg", "# c\nendpoint = \"ws://cfg:1/vnc\"\n");
        assert_eq!(run(None, Some(&p), "").unwrap(), "ws://cfg:1/vnc");
        let missing = p.with_file_name("none.toml");
        assert_eq!(
            run(None, Some(&missing), "ws://in:3/vnc\n").unwrap(),
            "ws://in:3/vnc"
        );
        assert!(run(None, None, "").is_err());
    }

    #[test]
    fn invalid_url_is_rejected() {
        assert!(
            run(Some("http://x/vnc"), None, "")
                .unwrap_err()
                .contains("invalid endpoint")
        );
        let p = tmp("bad", "endpoint = \"ws://h:1/other\"\n");
        assert!(run(None, Some(&p), "").is_err());
        assert!(run(None, None, "nonsense\n").is_err());
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
