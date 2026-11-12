//! Windows: installation hooks and automatic updates, read from the GitHub releases (Velopack).
//! Elsewhere, and for a copy not installed by the setup (cargo run, portable zip): nothing.

/// Where the releases are published: `releases.win.json` and the `.nupkg` files of each release.
#[cfg(windows)]
const REPO: &str = "https://github.com/emmanuellion/rameau";

/// First thing in `main`: the setup runs the exe with hook arguments (install, update, uninstall)
/// and expects it to quit right away. Also applies an update downloaded during a previous run.
pub fn startup() {
    #[cfg(windows)]
    velopack::VelopackApp::build().run();
}

/// An update downloaded and ready to install.
#[cfg(windows)]
pub struct Ready {
    manager: velopack::UpdateManager,
    info: Box<velopack::UpdateInfo>,
}

#[cfg(not(windows))]
pub enum Ready {}

/// Looks for a newer release and downloads it. Blocking: call it off the UI thread.
/// None when up to date, offline, or not an installed copy.
pub fn download() -> Option<Ready> {
    #[cfg(windows)]
    {
        use velopack::{UpdateCheck, UpdateManager, sources::GithubSource};
        // Fails for a copy that wasn't installed by the setup: nothing to update.
        let manager = UpdateManager::new(GithubSource::new(REPO, None, false), None, None).ok()?;
        let info = match manager.check_for_updates() {
            Ok(UpdateCheck::UpdateAvailable(info)) => info,
            Ok(_) => return None,
            Err(e) => {
                eprintln!("rameau: recherche de mise à jour : {e}");
                return None;
            }
        };
        if let Err(e) = manager.download_updates(&info, None) {
            eprintln!("rameau: téléchargement de la mise à jour : {e}");
            return None;
        }
        Some(Ready { manager, info })
    }
    #[cfg(not(windows))]
    None
}

impl Ready {
    pub fn version(&self) -> String {
        #[cfg(windows)]
        return self.info.TargetFullRelease.Version.clone();
        #[cfg(not(windows))]
        match *self {}
    }

    /// Quits, installs the update, and starts the new version. Returns only on failure.
    pub fn restart(&self) -> String {
        #[cfg(windows)]
        return match self.manager.apply_updates_and_restart(&*self.info) {
            Ok(()) => String::new(),
            Err(e) => e.to_string(),
        };
        #[cfg(not(windows))]
        match *self {}
    }
}
