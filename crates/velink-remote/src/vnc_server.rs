use std::{
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
