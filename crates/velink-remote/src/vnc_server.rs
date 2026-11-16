use std::{
    env,
    io,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use velink_core::remote::KubernetesConfig;

/// Own all desktop processes, including cleanup on partially failed startup.
#[derive(Default)]
pub struct VncServer {
    children: Vec<Child>,
}

impl VncServer {
    fn ensure_lease(config: &KubernetesConfig) -> io::Result<()> {
        let token_path = "/var/run/secrets/kubernetes.io/serviceaccount/token";
        if !std::path::Path::new(token_path).exists() {
            return Ok(());
        }
        let mut host = env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|e| io::Error::other(format!("Kubernetes API host: {e}")))?;
        if host.contains(':') && !host.starts_with('[') {
            host = format!("[{host}]");
        }
        let port = env::var("KUBERNETES_SERVICE_PORT_HTTPS").unwrap_or_else(|_| "443".into());
        let api = format!("https://{host}:{port}/apis/coordination.k8s.io/v1/namespaces/{}/leases", config.namespace);
        let lease_url = format!("{api}/velink-remote-leader");
        let ca = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt";
        let token = std::fs::read_to_string(token_path)?;
        let get = Command::new("curl")
            .args(["--silent", "--show-error", "--output", "/dev/null", "--write-out", "%{http_code}",
                   "--cacert", ca, "-H"])
            .arg(format!("Authorization: Bearer {}", token.trim()))
            .arg(&lease_url)
            .output()?;
        if !get.status.success() {
            return Err(io::Error::other(format!("Kubernetes Lease GET failed: {}", String::from_utf8_lossy(&get.stderr))));
        }
        let status = String::from_utf8_lossy(&get.stdout);
        if status.trim() != "404" {
            if status.starts_with('2') {
                return Ok(());
            }
            return Err(io::Error::other(format!("Kubernetes Lease GET returned {status}")));
        }
        let stamp = Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output()?;
        if !stamp.status.success() {
            return Err(io::Error::other("could not read UTC time for Kubernetes Lease"));
        }
        let stamp = String::from_utf8_lossy(&stamp.stdout).trim().to_owned();
        let holder = env::var("HOSTNAME").unwrap_or_else(|_| "velink-remote".into());
        let namespace = &config.namespace;
        let body = format!(r#"{{"apiVersion":"coordination.k8s.io/v1","kind":"Lease","metadata":{{"name":"velink-remote-leader","namespace":"{namespace}"}},"spec":{{"holderIdentity":"{holder}","leaseDurationSeconds":15,"renewTime":"{stamp}","acquireTime":"{stamp}","leaseTransitions":1}}}}"#);
        let post = Command::new("curl")
            .args(["--silent", "--show-error", "--output", "/dev/null", "--write-out", "%{http_code}",
                   "--cacert", ca, "-H"])
            .arg(format!("Authorization: Bearer {}", token.trim()))
            .args(["-H", "Content-Type: application/json", "-X", "POST", "--data"])
            .arg(body)
            .arg(&api)
            .output()?;
        if !post.status.success() || !String::from_utf8_lossy(&post.stdout).starts_with('2') {
            return Err(io::Error::other(format!("Kubernetes Lease POST failed: {} {}", String::from_utf8_lossy(&post.stdout), String::from_utf8_lossy(&post.stderr))));
        }
        Ok(())
    }

    fn spawn(&mut self, command: &mut Command) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        self.children.push(command.stdin(Stdio::null()).spawn()?);
        Ok(())
    }

    pub fn start(config: &KubernetesConfig) -> io::Result<Self> {
        config.validate().map_err(io::Error::other)?;
        Self::ensure_lease(config)?;
        let mut server = Self::default();
        let display = format!(":{}", config.display);
        let geometry = format!("{}x{}x24", config.width, config.height);
        server.spawn(Command::new("Xvfb").args([
            &display,
            "-screen",
            "0",
            &geometry,
            "-nolisten",
            "tcp",
            "-ac",
        ]))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            server.check()?;
            if Command::new("xdpyinfo")
                .env("DISPLAY", &display)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success()
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Xvfb startup timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Debian calls the TigerVNC scraping server x0vncserver.
        server.spawn(Command::new("x0vncserver").args([
            "-fg",
            "-display",
            &display,
            "-rfbport",
            &config.vnc_port.to_string(),
            "-localhost",
            "yes",
            "-SecurityTypes",
            "None",
            "-AlwaysShared",
        ]))?;
        server.spawn(
            Command::new("dbus-run-session")
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg(&config.desktop_command)
                .env("DISPLAY", &display),
        )?;
        let address = format!("127.0.0.1:{}", config.vnc_port).parse().unwrap();
        loop {
            server.check()?;
            if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TigerVNC startup timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(server)
    }

    pub fn check(&mut self) -> io::Result<()> {
        for child in &mut self.children {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "desktop process {} exited: {status}",
                    child.id()
                )));
            }
        }
        Ok(())
    }
}

impl Drop for VncServer {
    fn drop(&mut self) {
        for child in self.children.iter_mut().rev() {
            #[cfg(unix)]
            {
                // Each child leads its own process group. Include XFCE descendants
                // when startup fails or this server shuts down.
                // SAFETY: kill takes integer IDs and does not access Rust memory.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
