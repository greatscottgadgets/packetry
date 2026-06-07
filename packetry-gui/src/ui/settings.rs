use std::path::PathBuf;

use preferences::{AppInfo, Preferences};
use serde::{Serialize, Deserialize};

const APP_INFO: AppInfo = AppInfo {
    name: "Packetry",
    author: "Great Scott Gadgets"
};

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    pub last_used_directory: Option<PathBuf>,
}

impl Settings {
    pub fn load() -> Settings {
        match <Settings as Preferences>::load(&APP_INFO, "settings") {
            Ok(settings) => settings,
            Err(_) => Settings {
                last_used_directory: std::env::current_dir().ok()
            }
        }
    }

    pub fn save(&mut self) {
        let _ = <Settings as Preferences>::save(self, &APP_INFO, "settings");
    }
}
