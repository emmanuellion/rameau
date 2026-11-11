//! Write operations go through the git CLI: it handles hooks, signing, LFS filters,
//! ssh config and credential helpers exactly like the user's terminal does.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// PATH of the user's shell. Apps opened from the Finder, the Dock or a Linux menu get a minimal
/// PATH (/usr/bin:/bin…): hooks calling `npx` (husky) or Homebrew's git-lfs would not be found.
static SHELL_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Reads the shell PATH in the background at startup; git commands use it once known.
pub fn load_shell_path() {
    #[cfg(unix)]
    std::thread::spawn(|| {
        let _ = SHELL_PATH.set(shell_path(std::time::Duration::from_secs(5)));
    });
}

/// Asks an interactive login shell (where nvm, Homebrew… are set up) for its PATH.
#[cfg(unix)]
fn shell_path(timeout: std::time::Duration) -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
    // Markers: shell startup files may print their own text around it.
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", "printf '<<rameau>>%s<<rameau>>' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    while child.try_wait().ok()?.is_none() {
        if std::time::Instant::now() > deadline {
            // A startup file waiting for input must not hang anything: keep the current PATH.
            let _ = child.kill();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = String::from_utf8_lossy(&child.wait_with_output().ok()?.stdout).into_owned();
    marked(&out)
}

/// The PATH between the markers, followed by the app's own: git stays findable even if the shell
/// answered something odd. An answer that isn't a list of absolute folders (a shell where `$PATH`
/// isn't a variable, like Nushell, prints it as is) is ignored.
#[cfg_attr(not(unix), allow(dead_code))]
fn marked(out: &str) -> Option<String> {
    let path = out.split("<<rameau>>").nth(1)?;
    if path.is_empty() || !path.split(':').all(|d| d.starts_with('/')) {
        return None;
    }
    let own = std::env::var("PATH").unwrap_or_default();
    Some(if own.is_empty() { path.to_owned() } else { format!("{path}:{own}") })
}

/// Runs `git <args>` in `dir`, never blocking on a hidden prompt.
/// `input` is written to stdin (patches, credentials). `token` (GitHub OAuth) is handed to git
/// through a credential helper reading an env var, so it never shows up in argv.
/// Errors carry git's own message.
pub fn exec(
    dir: &Path,
    args: &[&str],
    input: Option<&str>,
    token: Option<&str>,
    env: &[(String, String)],
) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .envs(SHELL_PATH.get().cloned().flatten().map(|p| ("PATH", p)))
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The user's own github.com login (an organization's, another account's) comes first. Only when
    // they have none does Rameau's token answer, as the only helper: after a push git asks every
    // helper to store the credential used, and theirs must not keep a copy of Rameau's token.
    if let Some(token) = token.filter(|_| !user_has_github_login(dir, env)) {
        cmd.env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "credential.https://github.com.helper")
            .env("GIT_CONFIG_VALUE_0", "")
            .env("GIT_CONFIG_KEY_1", "credential.https://github.com.helper")
            .env(
                "GIT_CONFIG_VALUE_1",
                r#"!f() { test "$1" = get && printf 'username=oauth\npassword=%s\n' "$RAMEAU_TOKEN"; }; f"#,
            )
            .env("RAMEAU_TOKEN", token);
    }
    #[cfg(windows)]
    {
        // A GUI app spawning a console program flashes a console window unless told not to.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("git introuvable : {e}"))?;
    if let Some(input) = input {
        // Dropping stdin right after writing closes it, so git sees EOF.
        child.stdin.take().unwrap().write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(match (stderr.is_empty(), stdout.is_empty()) {
            (true, _) => stdout,
            (false, true) => stderr,
            (false, false) => format!("{stdout}\n{stderr}"),
        })
    }
}

/// The user's credential helpers (Keychain, Credential Manager, store…) already know a github.com
/// login. Asked without any prompt or window: nothing stored means no.
fn user_has_github_login(dir: &Path, env: &[(String, String)]) -> bool {
    let mut env = env.to_vec();
    env.push(("GCM_INTERACTIVE".into(), "never".into()));
    exec(dir, &["credential", "fill"], Some("protocol=https\nhost=github.com\n\n"), None, &env).is_ok_and(|out| out.contains("password="))
}

/// A file path as a git pathspec matching exactly that file. Plain, git reads `[id]` as "i or d"
/// and `:!x` as "everything but x": `restore`/`clean`/`add` would then touch other files.
pub fn literal(path: &str) -> String {
    format!(":(literal){path}")
}

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    exec(dir, args, None, None, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_path_is_read_from_noisy_output() {
        let found = marked("Bienvenue !\n<<rameau>>/opt/homebrew/bin:/usr/bin<<rameau>>").unwrap();
        assert!(found.starts_with("/opt/homebrew/bin:/usr/bin"), "the shell's folders come first: {found}");
        assert!(found.ends_with(&std::env::var("PATH").unwrap()), "the app's own PATH stays: {found}");
        assert_eq!(marked("pas de marqueur"), None);
        // Nushell prints `$PATH` literally: replacing PATH with it would hide git itself.
        assert_eq!(marked("<<rameau>>$PATH<<rameau>>"), None);
        // A real shell answers with a PATH that includes the system folders
        // (minimal environments, like some CI containers, have no SHELL: nothing to ask then).
        #[cfg(unix)]
        if std::env::var_os("SHELL").is_some_and(|s| !s.is_empty()) {
            let path = shell_path(std::time::Duration::from_secs(10)).expect("shell PATH");
            assert!(path.split(':').any(|d| d == "/usr/bin"), "{path}");
        }
    }

    #[test]
    fn paths_are_not_patterns() {
        let dir = crate::app::tests::temp("literal");
        let git = |a: &[&str]| crate::app::tests::git(&dir, a);
        git(&["init", "-q"]);
        std::fs::create_dir_all(dir.join("app/[id]")).unwrap();
        std::fs::create_dir_all(dir.join("app/i")).unwrap();
        for f in ["app/[id]/page.tsx", "app/i/page.tsx"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "i"]);
        for f in ["app/[id]/page.tsx", "app/i/page.tsx"] {
            std::fs::write(dir.join(f), "2").unwrap();
        }
        // Checking one file stages only that file…
        git(&["add", "--", &literal("app/[id]/page.tsx")]);
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "app/[id]/page.tsx");
        // …and discarding one keeps the other's changes.
        git(&["reset", "-q", "--", &literal("app/[id]/page.tsx")]);
        git(&["restore", "--", &literal("app/[id]/page.tsx")]);
        assert_eq!(std::fs::read_to_string(dir.join("app/i/page.tsx")).unwrap(), "2");
        // Deleting a new file named like an exclusion pattern deletes nothing else
        // (Windows forbids ':' in file names).
        #[cfg(unix)]
        {
            std::fs::write(dir.join(":!x"), "").unwrap();
            std::fs::write(dir.join("autre.txt"), "").unwrap();
            git(&["clean", "-f", "--", &literal(":!x")]);
            assert!(!dir.join(":!x").exists() && dir.join("autre.txt").exists());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_helper_answers_github_only() {
        let dir = crate::app::tests::temp("token-helper");
        // The user's git config, in place of the real system and global ones.
        let global = dir.join("gitconfig");
        let env = vec![
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ("GIT_CONFIG_GLOBAL".to_owned(), global.display().to_string()),
        ];
        let fill = |host: &str| {
            let input = format!("protocol=https\nhost={host}\n\n");
            exec(&dir, &["credential", "fill"], Some(&input), Some("s3cret"), &env)
        };
        // No helper of the user's: Rameau's token answers, for github.com only.
        std::fs::write(&global, "").unwrap();
        assert!(fill("github.com").unwrap().contains("password=s3cret"));
        assert!(!fill("example.com").unwrap_or_default().contains("s3cret"));
        // The user's own helper has a github.com credential: it wins.
        let theirs = r#"!f() { test "$1" = get && printf 'username=moi\npassword=mon-pat\n'; }; f"#;
        exec(&dir, &["config", "--global", "credential.helper", theirs], None, None, &env).unwrap();
        let out = fill("github.com").unwrap();
        assert!(out.contains("password=mon-pat") && !out.contains("s3cret"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After a push, git asks every helper to store the credential it used: the user's own helper
    /// must never end up with Rameau's token (it would outlive "Se déconnecter").
    #[test]
    fn token_is_never_copied_into_the_users_helper() {
        let dir = crate::app::tests::temp("token-leak");
        let store = dir.join("store-utilisateur");
        let global = dir.join("gitconfig");
        let env = vec![
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ("GIT_CONFIG_GLOBAL".to_owned(), global.display().to_string()),
        ];
        // Written by git itself: a hand-written config file would read the `\` of Windows paths
        // (C:\Users\…) as escapes. Single quotes keep them for the shell that runs the helper.
        let helper = format!("store --file='{}'", store.display());
        exec(&dir, &["config", "--global", "credential.helper", &helper], None, None, &env).unwrap();
        // What a push does: ask for a credential, then report it as working.
        let cred = exec(&dir, &["credential", "fill"], Some("protocol=https\nhost=github.com\n\n"), Some("s3cret"), &env).unwrap();
        assert!(cred.contains("password=s3cret"), "the user's helper is empty: Rameau's token answers");
        exec(&dir, &["credential", "approve"], Some(&format!("{cred}\n\n")), Some("s3cret"), &env).unwrap();
        let stored = std::fs::read_to_string(&store).unwrap_or_default();
        assert!(!stored.contains("s3cret"), "token copied into the user's helper: {stored}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
