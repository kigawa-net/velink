//! Runtime-independent configuration and connection state for a remote desktop.

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VncConnectionState {
    Disconnected,
    Connecting,
    Connected { width: u16, height: u16 },
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteSession {
    pub endpoint: String,
    pub state: VncConnectionState,
}

impl RemoteSession {
    pub fn new(endpoint: impl Into<String>) -> Result<Self, String> {
        let endpoint = endpoint.into();
        // TLS/authentication are intentionally outside this initial implementation.
        if !endpoint.starts_with("ws://") || !endpoint.ends_with("/vnc") {
            return Err("remote endpoint must be ws://host:port/vnc".into());
        }
        Ok(Self {
            endpoint,
            state: VncConnectionState::Disconnected,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesConfig {
    pub namespace: String,
    pub pod_name: Option<String>,
    pub listen_address: std::net::SocketAddr,
    pub display: u16,
    pub width: u16,
    pub height: u16,
    pub vnc_port: u16,
    pub desktop_command: String,
}

impl Default for KubernetesConfig {
    fn default() -> Self {
        Self {
            namespace: "default".into(),
            pod_name: None,
            listen_address: "0.0.0.0:8080".parse().unwrap(),
            display: 99,
            width: 1280,
            height: 720,
            vnc_port: 5900,
            desktop_command: "startxfce4".into(),
        }
    }
}

impl KubernetesConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0
            || self.height == 0
            || u64::from(self.width) * u64::from(self.height) > 16_777_216
        {
            return Err("resolution must be nonzero and at most 16 megapixels".into());
        }
        if self.vnc_port == 0 || self.listen_address.port() == 0 {
            return Err("ports must be nonzero".into());
        }
        if self.desktop_command.trim().is_empty() {
            return Err("desktop command must not be empty".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_rejects_invalid_resolution_and_ports() {
        assert!(KubernetesConfig::default().validate().is_ok());
        for (width, height) in [(0, 720), (1280, 0), (65535, 65535)] {
            let config = KubernetesConfig {
                width,
                height,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
        assert!(
            KubernetesConfig {
                vnc_port: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn session_starts_disconnected_and_requires_websocket_endpoint() {
        assert_eq!(
            RemoteSession::new("ws://localhost:8080/vnc").unwrap().state,
            VncConnectionState::Disconnected
        );
        for endpoint in [
            "http://localhost/vnc",
            "ws://localhost/",
            "wss://localhost/vnc",
        ] {
            assert!(RemoteSession::new(endpoint).is_err());
        }
    }
}
