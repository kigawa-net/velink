use std::{
    cell::Cell,
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
};

use eframe::egui;

use crate::{
    auth::{self, AuthConfig},
    endpoint,
};

#[derive(Default)]
struct Settings {
    endpoint: String,
    auth: AuthConfig,
}

impl Settings {
    fn load(path: &Path) -> Result<Self, String> {
        let text = endpoint::read_config(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        Ok(Self {
            endpoint: endpoint::config_string(&text, "endpoint").unwrap_or_default(),
            auth: AuthConfig::resolve(Some(&text), None, None),
        })
    }

    fn save(&self, path: &Path) -> Result<String, String> {
        let url = endpoint::validate(self.endpoint.trim())
            .map_err(|error| format!("Invalid endpoint: {error}"))?;
        endpoint::write_config(
            path,
            &[
                ("endpoint", &url),
                ("issuer", self.auth.issuer.trim()),
                ("client_id", self.auth.client_id.trim()),
                ("scope", self.auth.scope.trim()),
            ],
        )
        .map_err(|error| format!("Could not save {}: {error}", path.display()))?;
        Ok(url)
    }
}

fn delete_token(path: &Path) -> Result<&'static str, String> {
    match fs::remove_file(path) {
        Ok(()) => Ok("Saved token deleted."),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok("No saved token."),
        Err(error) => Err(format!("Could not delete {}: {error}", path.display())),
    }
}

struct SettingsWindow {
    settings: Settings,
    config: PathBuf,
    token: PathBuf,
    saved_endpoint: Rc<Cell<Option<String>>>,
    status: Option<Result<&'static str, String>>,
}

impl eframe::App for SettingsWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("velink Settings");
            ui.add_space(12.0);
            let field_width = ui.available_width() - 90.0;
            egui::Grid::new("settings_fields")
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    for (label, value) in [
                        ("Endpoint", &mut self.settings.endpoint),
                        ("Issuer", &mut self.settings.auth.issuer),
                        ("Client ID", &mut self.settings.auth.client_id),
                        ("Scope", &mut self.settings.auth.scope),
                    ] {
                        ui.label(label);
                        ui.add(egui::TextEdit::singleline(value).desired_width(field_width));
                        ui.end_row();
                    }
                });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    match self.settings.save(&self.config) {
                        Ok(url) => {
                            self.saved_endpoint.set(Some(url));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        Err(error) => self.status = Some(Err(error)),
                    }
                }
                if ui.button("Cancel").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Delete saved token").clicked() {
                    self.status = Some(delete_token(&self.token));
                }
            });
            if let Some(status) = &self.status {
                ui.add_space(8.0);
                match status {
                    Ok(message) => ui.label(*message),
                    Err(message) => ui.colored_label(ui.visuals().error_fg_color, message),
                };
            }
        });
    }
}

/// Run the native window on the main thread and return the endpoint only after saving.
pub fn open(config: Option<PathBuf>) -> Result<Option<String>, String> {
    let config = config.ok_or("Cannot determine the config directory.")?;
    let (settings, status) = match Settings::load(&config) {
        Ok(settings) => (settings, None),
        Err(error) => (Settings::default(), Some(Err(error))),
    };
    let token = auth::token_path_from(Some(config.clone())).expect("config path is present");
    let saved_endpoint = Rc::new(Cell::new(None));
    let window = SettingsWindow {
        settings,
        config,
        token,
        saved_endpoint: Rc::clone(&saved_endpoint),
        status,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 360.0])
            .with_min_inner_size([440.0, 260.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "velink Settings",
        options,
        Box::new(|_| Ok(Box::new(window))),
    )
    .map_err(|error| error.to_string())?;
    Ok(saved_endpoint.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_defaults_and_round_trips_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut settings = Settings::load(&path).unwrap();
        assert_eq!(settings.endpoint, "");
        assert_eq!(settings.auth.issuer, auth::DEFAULT_ISSUER);
        assert_eq!(settings.auth.client_id, auth::DEFAULT_CLIENT_ID);
        assert_eq!(settings.auth.scope, "openid offline_access");

        fs::write(&path, "# keep\nother = \"value\"\n").unwrap();
        settings.endpoint = " wss://example.org/vnc ".into();
        settings.auth.issuer = "https://example.org/realms/test".into();
        settings.auth.client_id = "other-client".into();
        settings.auth.scope = "openid".into();
        assert_eq!(settings.save(&path).unwrap(), "wss://example.org/vnc");

        let saved = Settings::load(&path).unwrap();
        assert_eq!(saved.endpoint, "wss://example.org/vnc");
        assert_eq!(saved.auth, settings.auth);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("# keep\nother = \"value\"\n")
        );
    }

    #[test]
    fn invalid_settings_do_not_modify_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let saved = "endpoint = \"ws://example.org/vnc\"\n";
        fs::write(&path, saved).unwrap();
        let mut settings = Settings::load(&path).unwrap();

        settings.endpoint = "https://example.org/vnc".into();
        assert!(
            settings
                .save(&path)
                .unwrap_err()
                .contains("remote endpoint")
        );
        settings.endpoint = "ws:///vnc".into();
        assert!(
            settings
                .save(&path)
                .unwrap_err()
                .contains("Invalid endpoint")
        );
        settings.endpoint = "ws://example.org/vnc".into();
        settings.auth.scope = "openid\nclient_id = \"bad\"".into();
        assert!(settings.save(&path).unwrap_err().contains("scope"));
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
    }

    #[test]
    fn deletes_saved_token_and_reports_absence_or_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        assert_eq!(delete_token(&path).unwrap(), "No saved token.");
        fs::write(&path, "{}").unwrap();
        assert_eq!(delete_token(&path).unwrap(), "Saved token deleted.");
        assert!(!path.exists());
        fs::create_dir(&path).unwrap();
        assert!(delete_token(&path).is_err());
    }
}
