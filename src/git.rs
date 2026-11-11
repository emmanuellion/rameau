//! Write operations go through the git CLI: it handles hooks, signing, LFS filters,
//! ssh config and credential helpers exactly like the user's terminal does.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(token) = token {
        // The empty helper resets the list for github.com, then ours answers with the token.
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

pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    exec(dir, args, None, None, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_helper_answers_github_only() {
        let dir = std::env::temp_dir();
        let gh = exec(&dir, &["credential", "fill"], Some("protocol=https\nhost=github.com\n\n"), Some("s3cret"), &[]);
        assert!(gh.unwrap().contains("password=s3cret"));
        // Other hosts must not receive the token (and fail since prompts are disabled).
        let other = exec(
            &dir,
            &["-c", "credential.helper=", "credential", "fill"],
            Some("protocol=https\nhost=example.com\n\n"),
            Some("s3cret"),
            &[],
        );
        assert!(!other.unwrap_or_default().contains("s3cret"));
    }
}
