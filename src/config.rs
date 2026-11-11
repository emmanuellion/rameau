//! Imported projects and preferences, saved as JSON in the platform's config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub projects: Vec<PathBuf>,
    pub last: Option<PathBuf>,
    pub light: bool,
    /// Sidebar sections folded by the user ("branches", "remotes", "stashes").
    pub collapsed: Vec<String>,
    /// Widths chosen by dragging the panel edges (None: default).
    pub sidebar_width: Option<f32>,
    pub panel_width: Option<f32>,
}

/// `~/Library/Application Support/rameau` on macOS, `$XDG_CONFIG_HOME/rameau` (or `~/.config/rameau`)
/// on Linux, `%APPDATA%\rameau` on Windows.
pub fn dir() -> PathBuf {
    let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let base = if cfg!(target_os = "macos") {
        home().join("Library/Application Support")
    } else if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default())
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(".config"))
    };
    base.join("rameau")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default()
}

impl Config {
    pub fn load() -> Self {
        std::fs::read(dir().join("config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = dir();
        let result = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join("config.json"), serde_json::to_vec_pretty(self).unwrap_or_default()));
        if let Err(e) = result {
            eprintln!("rameau: impossible d'enregistrer la configuration : {e}");
        }
    }

    pub fn add(&mut self, path: &Path) {
        if !self.projects.iter().any(|p| p == path) {
            self.projects.push(path.to_owned());
        }
        self.last = Some(path.to_owned());
    }

    pub fn remove(&mut self, path: &Path) {
        self.projects.retain(|p| p != path);
        if self.last.as_deref() == Some(path) {
            self.last = self.projects.first().cloned();
        }
    }
}
