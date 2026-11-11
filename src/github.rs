//! "Sign in with GitHub" through the OAuth device flow: no client secret and no local
//! web server, which is the flow GitHub recommends for desktop apps.
//! The token is stored by the user's git credential helper (Keychain on macOS).

use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{config, git};

/// Public id of the Rameau OAuth App (device flow enabled). Set at build time:
/// `RAMEAU_GITHUB_CLIENT_ID=Ov23li… cargo build`.
pub const CLIENT_ID: Option<&str> = option_env!("RAMEAU_GITHUB_CLIENT_ID");
// `repo` to push to private repos, `workflow` to push changes to .github/workflows.
const SCOPE: &str = "repo workflow";

pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

pub struct Account {
    pub login: String,
    pub token: String,
}

fn post(url: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    ureq::post(url)
        .header("Accept", "application/json")
        .send_form(form.iter().copied())
        .map_err(|e| format!("GitHub injoignable : {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())
}

fn str_field(v: &Value, key: &str) -> Result<String, String> {
    v[key].as_str().map(str::to_owned).ok_or_else(|| {
        v["error_description"].as_str().unwrap_or("réponse GitHub inattendue").to_owned()
    })
}

pub fn start(client_id: &str) -> Result<DeviceCode, String> {
    let v = post("https://github.com/login/device/code", &[("client_id", client_id), ("scope", SCOPE)])?;
    Ok(DeviceCode {
        user_code: str_field(&v, "user_code")?,
        verification_uri: str_field(&v, "verification_uri")?,
        device_code: str_field(&v, "device_code")?,
        interval: v["interval"].as_u64().unwrap_or(5),
        expires_in: v["expires_in"].as_u64().unwrap_or(900),
    })
}

/// Blocks (call it off the UI thread) until the user approves in the browser.
pub fn wait_for_token(client_id: &str, code: &DeviceCode) -> Result<Account, String> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval;
    while Instant::now() < deadline {
        sleep(Duration::from_secs(interval));
        let v = post(
            "https://github.com/login/oauth/access_token",
            &[
                ("client_id", client_id),
                ("device_code", &code.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?;
        match v["error"].as_str() {
            None => return account(str_field(&v, "access_token")?),
            Some("authorization_pending") => {}
            Some("slow_down") => interval = v["interval"].as_u64().unwrap_or(interval + 5),
            Some("access_denied") => return Err("Connexion refusée sur GitHub.".into()),
            Some("expired_token") => break,
            Some(_) => return Err(str_field(&v, "error_description").unwrap_or_else(|e| e)),
        }
    }
    Err("Le code a expiré, recommence la connexion.".into())
}

fn account(token: String) -> Result<Account, String> {
    let v: Value = ureq::get("https://api.github.com/user")
        .header("Authorization", &format!("Bearer {token}"))
        .header("User-Agent", "rameau")
        .call()
        .map_err(|e| format!("Token GitHub refusé : {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    Ok(Account { login: str_field(&v, "login")?, token })
}

/// `git credential <action>` through the user's helper (Keychain, Credential Manager, libsecret…).
/// Without any helper (common on Linux) git's `store` helper keeps it in Rameau's config folder,
/// in a file only the user can read.
fn credential(action: &str, input: &str) -> Result<String, String> {
    let home = config::home();
    let has_helper = git::run(&home, &["config", "--get", "credential.helper"]).is_ok_and(|h| !h.trim().is_empty());
    let store = format!("credential.helper=store --file=\"{}\"", config::dir().join("credentials").display());
    let _ = std::fs::create_dir_all(config::dir());
    let mut args = vec![];
    if !has_helper {
        args.extend(["-c", store.as_str()]);
    }
    args.extend(["credential", action]);
    git::exec(&home, &args, Some(input), None, &[])
}

pub fn save(account: &Account) -> Result<(), String> {
    let input = format!("protocol=https\nhost=github.com\nusername={}\npassword={}\n\n", account.login, account.token);
    credential("approve", &input).map(|_| ())
}

/// Account previously stored by the credential helper, if its token still works.
pub fn load() -> Option<Account> {
    let out = credential("fill", "protocol=https\nhost=github.com\n\n").ok()?;
    let token = out.lines().find_map(|l| l.strip_prefix("password="))?;
    account(token.to_owned()).ok()
}

pub fn forget(account: &Account) {
    let input = format!("protocol=https\nhost=github.com\nusername={}\n\n", account.login);
    let _ = credential("reject", &input);
}
