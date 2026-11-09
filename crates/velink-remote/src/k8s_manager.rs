//! Pod configuration via environment and the Kubernetes downward API.
use std::env;
use velink_core::remote::KubernetesConfig;

pub fn config_from_env() -> Result<KubernetesConfig, String> {
    let mut config = KubernetesConfig::default();
    config.namespace = env::var("POD_NAMESPACE").unwrap_or(config.namespace);
    config.pod_name = env::var("POD_NAME").ok();
    if let Ok(value) = env::var("VELINK_LISTEN") {
        config.listen_address = value.parse().map_err(|e| format!("VELINK_LISTEN: {e}"))?;
    }
    for (name, target) in [
        ("VELINK_DISPLAY", &mut config.display),
        ("VELINK_WIDTH", &mut config.width),
        ("VELINK_HEIGHT", &mut config.height),
        ("VELINK_VNC_PORT", &mut config.vnc_port),
    ] {
        if let Ok(value) = env::var(name) {
            *target = value.parse().map_err(|e| format!("{name}: {e}"))?;
        }
    }
    if let Ok(command) = env::var("VELINK_DESKTOP_COMMAND") {
        config.desktop_command = command;
    }
    config.validate()?;
    Ok(config)
}
