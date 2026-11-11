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
    pub files_width: Option<f32>,
    /// Diffs shown side by side (old left, new right) rather than in one column.
    pub split_diff: bool,
}

/// `~/Library/Application Support/rameau` on macOS, `$XDG_CONFIG_HOME/rameau` (or `~/.config/rameau`)
/// on Linux, `%APPDATA%\rameau` on Windows.
pub fn dir() -> PathBuf {
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
        Self::load_from(&dir().join("config.json"))
    }

    fn load_from(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else { return Self::default() };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // Hand-edited with a typo, or written by another version: keep it aside instead of
            // letting the next save replace the project list with an empty one.
            eprintln!("rameau: configuration illisible ({e}), mise de côté en config.json.bak");
            let _ = std::fs::rename(path, path.with_extension("json.bak"));
            Self::default()
        })
    }

    pub fn save(&self) {
        if let Err(e) = self.save_to(&dir().join("config.json")) {
            eprintln!("rameau: impossible d'enregistrer la configuration : {e}");
        }
    }

    /// Writes a temp file, then renames it over the config: if the app dies mid-write,
    /// the previous config is still whole (a half-written one would be read as empty).
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// Forgets projects deleted from disk since last time and lists each folder once,
    /// whatever way its path was written (trailing slash, symlink…).
    pub fn tidy(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.projects = self.projects.iter().filter_map(|p| crate::app::canonical(p)).filter(|p| seen.insert(p.clone())).collect();
        self.last = self.last.as_ref().and_then(|p| crate::app::canonical(p)).or_else(|| self.projects.first().cloned());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_lists_each_project_once() {
        let dir = crate::app::tests::temp("tidy");
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut config = Config {
            // Same folder twice, not side by side, plus a deleted one.
            projects: vec![a.join("."), b.clone(), PathBuf::from(format!("{}/", a.display())), dir.join("gone")],
            last: Some(dir.join("gone")),
            ..Config::default()
        };
        config.tidy();
        let canon = |p: &Path| crate::app::canonical(p).unwrap();
        assert_eq!(config.projects, [canon(&a), canon(&b)]);
        assert_eq!(config.last, Some(canon(&a)), "a deleted last project falls back to the first one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_config_is_kept_aside() {
        let dir = crate::app::tests::temp("config-bad");
        let path = dir.join("config.json");
        let text = "{\"projects\": [\"/projets/a\",], }";
        std::fs::write(&path, text).unwrap();
        let config = Config::load_from(&path);
        assert!(config.projects.is_empty());
        assert_eq!(std::fs::read_to_string(path.with_extension("json.bak")).unwrap(), text, "the user's list is still there");
        // A missing file is just a first launch: nothing to keep aside.
        let _ = Config::load_from(&dir.join("absent.json"));
        assert!(!dir.join("absent.json.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_replaces_the_file_whole() {
        let dir = crate::app::tests::temp("config");
        let path = dir.join("config.json");
        let mut config = Config::default();
        config.add(Path::new("/projets/a"));
        config.light = true;
        config.save_to(&path).unwrap();
        // A write interrupted earlier left garbage in the temp file: the saved config is unaffected.
        std::fs::write(path.with_extension("json.tmp"), "{\"projects\": [").unwrap();
        let back = Config::load_from(&path);
        assert_eq!(back.projects, [PathBuf::from("/projets/a")]);
        assert!(back.light);
        // And the next save goes through.
        config.add(Path::new("/projets/b"));
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).projects.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
