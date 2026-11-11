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
/// Username of Rameau's entry in the credential helper. Asking for this exact name means
/// Rameau only ever reads or deletes its own token, never the one the user's git already uses.
const USERNAME: &str = "rameau-oauth";

/// `git credential` input for Rameau's entry, with the token when storing it.
fn entry(token: Option<&str>) -> String {
    let password = token.map_or(String::new(), |t| format!("password={t}\n"));
    format!("protocol=https\nhost=github.com\nusername={USERNAME}\n{password}\n")
}

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
    /// Public profile name, when set: suggested as git `user.name`.
    pub name: Option<String>,
    /// GitHub's private "noreply" address: suggested as git `user.email`.
    pub noreply_email: String,
    /// Profile picture address (None offline).
    pub avatar_url: Option<String>,
}

/// A repository the account can clone.
pub struct RemoteRepo {
    /// `owner/name`.
    pub full_name: String,
    pub clone_url: String,
    pub description: String,
    pub private: bool,
}

/// HTTP client with a time limit: ureq waits forever by default, and a Wi-Fi portal that
/// accepts connections but never answers would leave the sign-in stuck.
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}

fn http() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| agent(Duration::from_secs(30)))
}

fn post(url: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    http()
        .post(url)
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
        let response = post(
            "https://github.com/login/oauth/access_token",
            &[
                ("client_id", client_id),
                ("device_code", &code.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        );
        match poll_step(response, interval) {
            Poll::Wait(next) => interval = next,
            Poll::Token(token) => return account(token),
            Poll::Fail(e) => return Err(e),
            Poll::Expired => break,
        }
    }
    Err("Le code a expiré, recommence la connexion.".into())
}

#[derive(Debug, PartialEq)]
enum Poll {
    /// Not approved yet: ask again after this many seconds.
    Wait(u64),
    Token(String),
    Fail(String),
    Expired,
}

/// What to do with one answer of GitHub's token endpoint. A network error is not fatal:
/// the user may still be approving in the browser, so keep polling until the code expires.
fn poll_step(response: Result<Value, String>, interval: u64) -> Poll {
    let Ok(v) = response else { return Poll::Wait(interval) };
    match v["error"].as_str() {
        None => str_field(&v, "access_token").map_or_else(Poll::Fail, Poll::Token),
        Some("authorization_pending") => Poll::Wait(interval),
        Some("slow_down") => Poll::Wait(v["interval"].as_u64().unwrap_or(interval + 5)),
        Some("access_denied") => Poll::Fail("Connexion refusée sur GitHub.".into()),
        Some("expired_token") => Poll::Expired,
        Some(_) => Poll::Fail(str_field(&v, "error_description").unwrap_or_else(|e| e)),
    }
}

fn account(token: String) -> Result<Account, String> {
    fetch_account("https://api.github.com/user", token).map_err(|(_, e)| e)
}

/// The account a token belongs to. The bool of the error tells whether GitHub refused the token
/// (revoked, expired) rather than not being reachable.
fn fetch_account(url: &str, token: String) -> Result<Account, (bool, String)> {
    let v = api_get(url, &token)?;
    let str_field = |k| str_field(&v, k).map_err(|e| (false, e));
    let login = str_field("login")?;
    let noreply_email = format!("{}+{login}@users.noreply.github.com", v["id"].as_u64().unwrap_or_default());
    let name = v["name"].as_str().filter(|n| !n.trim().is_empty()).map(str::to_owned);
    let avatar_url = v["avatar_url"].as_str().map(str::to_owned);
    Ok(Account { login, token, name, noreply_email, avatar_url })
}

/// GET on the GitHub API. The bool of the error tells whether GitHub refused the token.
fn api_get(url: &str, token: &str) -> Result<Value, (bool, String)> {
    http()
        .get(url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("User-Agent", "rameau")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(401 | 403) => (true, "Token GitHub refusé.".to_owned()),
            e => (false, format!("GitHub injoignable : {e}")),
        })?
        .body_mut()
        .read_json()
        .map_err(|e| (false, e.to_string()))
}

/// Repositories the account owns, collaborates on or reaches through its organizations,
/// most recently pushed first. Blocks: call it off the UI thread.
pub fn repos(token: &str) -> Result<Vec<RemoteRepo>, String> {
    repos_from("https://api.github.com", token)
}

fn repos_from(api: &str, token: &str) -> Result<Vec<RemoteRepo>, String> {
    let mut out = Vec::new();
    // shortcut: stops at 1,000 repositories (10 pages); the search field covers the rest well enough.
    for page in 1..=10 {
        let url = format!("{api}/user/repos?per_page=100&page={page}&sort=pushed&affiliation=owner,collaborator,organization_member");
        let v = api_get(&url, token).map_err(|(_, e)| e)?;
        let list = v.as_array().ok_or("réponse GitHub inattendue")?;
        out.extend(list.iter().filter_map(|r| {
            Some(RemoteRepo {
                full_name: r["full_name"].as_str()?.to_owned(),
                clone_url: r["clone_url"].as_str()?.to_owned(),
                description: r["description"].as_str().unwrap_or("").to_owned(),
                private: r["private"].as_bool().unwrap_or(false),
            })
        }));
        if list.len() < 100 {
            break;
        }
    }
    Ok(out)
}

/// `owner/name` of a github.com remote address (https or ssh), None for any other host.
pub fn repo_slug(url: &str) -> Option<String> {
    let path = ["https://github.com/", "http://github.com/", "ssh://git@github.com/", "git@github.com:"]
        .iter()
        .find_map(|p| url.trim().strip_prefix(p))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty() && parts.next().is_none()).then(|| format!("{owner}/{name}"))
}

/// Result of a branch's CI (GitHub Actions and other checks) on its last pushed commit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Checks {
    Pending,
    Success,
    Failure,
}

/// An open pull request of the repository.
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// Branch it comes from, in this repository.
    pub branch: String,
    pub draft: bool,
}

/// What GitHub knows about the open project: its default branch, open pull requests,
/// and the CI result of each pushed branch.
pub struct RepoInfo {
    pub slug: String,
    pub default_branch: String,
    pub pulls: Vec<PullRequest>,
    pub checks: std::collections::HashMap<String, Checks>,
    /// Profile picture address of the recent authors, by commit e-mail (lower case).
    pub avatars: std::collections::HashMap<String, String>,
}

/// Profile picture of a GitHub "noreply" commit address, which names the account itself:
/// `123+login@users.noreply.github.com` (or the older `login@users.noreply.github.com`).
pub fn noreply_avatar(email: &str) -> Option<String> {
    let user = email.to_lowercase().strip_suffix("@users.noreply.github.com")?.to_owned();
    Some(match user.split_once('+') {
        Some((id, _)) if id.chars().all(|c| c.is_ascii_digit()) => format!("https://avatars.githubusercontent.com/u/{id}"),
        _ => format!("https://avatars.githubusercontent.com/{user}"),
    })
}

/// Reads the repository's information; `branches` are (local branch, sha it has on github.com).
/// Blocks (one request per branch, in parallel): call it off the UI thread.
pub fn repo_info(token: &str, slug: &str, branches: Vec<(String, String)>) -> Result<RepoInfo, String> {
    repo_info_from("https://api.github.com", token, slug, branches)
}

fn repo_info_from(api: &str, token: &str, slug: &str, branches: Vec<(String, String)>) -> Result<RepoInfo, String> {
    let get = |path: &str| api_get(&format!("{api}/repos/{slug}{path}"), token).map_err(|(_, e)| e);
    let repo = get("")?;
    let pulls = get("/pulls?state=open&per_page=100")?;
    // The last 100 commits tell who their authors are on GitHub, with their pictures.
    let avatars = get("/commits?per_page=100")
        .ok()
        .and_then(|v| {
            v.as_array().map(|list| {
                list.iter()
                    .filter_map(|c| Some((c["commit"]["author"]["email"].as_str()?.to_lowercase(), c["author"]["avatar_url"].as_str()?.to_owned())))
                    .collect()
            })
        })
        .unwrap_or_default();
    let pulls = pulls
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|p| p["head"]["repo"]["full_name"].as_str() == Some(slug))
                .filter_map(|p| {
                    Some(PullRequest {
                        number: p["number"].as_u64()?,
                        title: p["title"].as_str()?.to_owned(),
                        url: p["html_url"].as_str()?.to_owned(),
                        branch: p["head"]["ref"].as_str()?.to_owned(),
                        draft: p["draft"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // shortcut: the 20 first branches only (2 requests each); enough for local branches in practice.
    let checks = std::thread::scope(|s| {
        let handles: Vec<_> = branches
            .into_iter()
            .take(20)
            .map(|(branch, sha)| {
                s.spawn(move || {
                    let runs = get(&format!("/commits/{sha}/check-runs?per_page=100")).ok();
                    let status = get(&format!("/commits/{sha}/status")).ok();
                    combine_checks(runs.as_ref(), status.as_ref()).map(|c| (branch, c))
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });
    Ok(RepoInfo {
        slug: slug.to_owned(),
        default_branch: repo["default_branch"].as_str().unwrap_or("main").to_owned(),
        pulls,
        checks,
        avatars,
    })
}

/// One result from GitHub's two CI APIs: check runs (GitHub Actions…) and commit statuses (older
/// services). Any failure fails, else anything running is pending; None when there is no CI.
fn combine_checks(runs: Option<&Value>, status: Option<&Value>) -> Option<Checks> {
    let mut states = Vec::new();
    for run in runs.and_then(|r| r["check_runs"].as_array()).into_iter().flatten() {
        states.push(match (run["status"].as_str(), run["conclusion"].as_str()) {
            (Some("completed"), Some("success" | "neutral" | "skipped")) => Checks::Success,
            (Some("completed"), _) => Checks::Failure,
            _ => Checks::Pending,
        });
    }
    // A commit without any status reports "pending" with a count of 0.
    if let Some(st) = status.filter(|s| s["total_count"].as_u64().unwrap_or(0) > 0) {
        states.push(match st["state"].as_str() {
            Some("success") => Checks::Success,
            Some("pending") => Checks::Pending,
            _ => Checks::Failure,
        });
    }
    [Checks::Failure, Checks::Pending, Checks::Success].into_iter().find(|c| states.contains(c))
}

/// Profile picture, small: (bytes, format). None when it can't be fetched (offline): initials then.
pub fn avatar(url: &str) -> Option<(Vec<u8>, gpui::ImageFormat)> {
    let sep = if url.contains('?') { '&' } else { '?' };
    let bytes = http().get(&format!("{url}{sep}s=96")).header("User-Agent", "rameau").call().ok()?.body_mut().read_to_vec().ok()?;
    let format = match bytes.get(..4)? {
        [0x89, b'P', b'N', b'G'] => gpui::ImageFormat::Png,
        [0xff, 0xd8, ..] => gpui::ImageFormat::Jpeg,
        [b'G', b'I', b'F', _] => gpui::ImageFormat::Gif,
        [b'R', b'I', b'F', b'F'] => gpui::ImageFormat::Webp,
        _ => return None,
    };
    Some((bytes, format))
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
    // Git Credential Manager (Windows) opens its own sign-in window when it has nothing stored;
    // these calls run in the background, so they must never ask anything.
    let quiet = [("GCM_INTERACTIVE".to_owned(), "never".to_owned())];
    git::exec(&home, &args, Some(input), None, &quiet)
}

pub fn save(token: &str) -> Result<(), String> {
    credential("approve", &entry(Some(token))).map(|_| ())
}

/// Account previously stored by the credential helper, if GitHub hasn't refused its token.
/// Offline (train, plane), the token is kept: only the profile details are missing until next time.
pub fn load() -> Option<Account> {
    let out = credential("fill", &entry(None)).ok()?;
    let token = out.lines().find_map(|l| l.strip_prefix("password="))?.to_owned();
    stored_account("https://api.github.com/user", token)
}

fn stored_account(url: &str, token: String) -> Option<Account> {
    match fetch_account(url, token.clone()) {
        Ok(a) => Some(a),
        Err((true, _)) => None,
        Err((false, _)) => Some(Account { login: "GitHub".into(), token, name: None, noreply_email: String::new(), avatar_url: None }),
    }
}

pub fn forget() {
    let _ = credential("reject", &entry(None));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_server_times_out() {
        // Accepts the connection, never answers (like a captive Wi-Fi portal).
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", server.local_addr().unwrap());
        let start = Instant::now();
        assert!(agent(Duration::from_millis(500)).get(&url).call().is_err());
        assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
        drop(server);
    }

    #[test]
    fn offline_start_keeps_the_stored_token() {
        // Nothing listens on this port: no network.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let a = stored_account(&format!("http://127.0.0.1:{port}/user"), "t0k".into()).expect("kept offline");
        assert_eq!(a.token, "t0k");
        // GitHub answers 401: the token was revoked, the account is dropped.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/user", server.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = server.accept().unwrap();
            let _ = s.read(&mut [0; 1024]);
            let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        });
        assert!(stored_account(&url, "t0k".into()).is_none());
    }

    /// Two pages of repositories (100, then 1): both are read, then the listing stops.
    #[test]
    fn repositories_are_read_page_by_page() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", server.local_addr().unwrap());
        let repo = |i: usize| format!(r#"{{"full_name":"a/r{i}","clone_url":"https://github.com/a/r{i}.git","private":{},"description":null}}"#, i == 0);
        let asked = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut pages = vec![];
            for body in [(0..100).map(repo).collect::<Vec<_>>().join(","), repo(100)] {
                let (mut s, _) = server.accept().unwrap();
                let mut req = [0; 2048];
                let n = s.read(&mut req).unwrap();
                pages.push(String::from_utf8_lossy(&req[..n]).lines().next().unwrap().to_owned());
                let body = format!("[{body}]");
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
            pages
        });
        let repos = repos_from(&api, "t0k").unwrap();
        assert_eq!(repos.len(), 101);
        assert!(repos[0].private && !repos[1].private);
        assert_eq!((repos[100].full_name.as_str(), repos[100].description.as_str()), ("a/r100", ""));
        let pages = asked.join().unwrap();
        assert!(pages[0].contains("page=1&") && pages[1].contains("page=2&"), "{pages:?}");
    }

    #[test]
    fn github_addresses_give_owner_and_name() {
        assert_eq!(repo_slug("https://github.com/ada/rameau.git").as_deref(), Some("ada/rameau"));
        assert_eq!(repo_slug("git@github.com:ada/rameau.git").as_deref(), Some("ada/rameau"));
        assert_eq!(repo_slug("ssh://git@github.com/ada/rameau").as_deref(), Some("ada/rameau"));
        assert_eq!(repo_slug("https://github.com/ada/rameau/").as_deref(), Some("ada/rameau"));
        assert_eq!(repo_slug("https://gitlab.com/ada/rameau.git"), None);
        assert_eq!(repo_slug("https://github.com/ada"), None);
    }

    #[test]
    fn ci_results_are_combined() {
        let json = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        let runs = |list: &str| json(&format!(r#"{{"check_runs":[{list}]}}"#));
        let run = |status: &str, conclusion: &str| format!(r#"{{"status":"{status}","conclusion":{conclusion}}}"#);
        let none = json(r#"{"state":"pending","total_count":0}"#);
        // No CI at all: nothing to show (the empty status says "pending").
        assert_eq!(combine_checks(Some(&runs("")), Some(&none)), None);
        let ok = run("completed", r#""success""#);
        assert_eq!(combine_checks(Some(&runs(&ok)), Some(&none)), Some(Checks::Success));
        let running = run("in_progress", "null");
        assert_eq!(combine_checks(Some(&runs(&format!("{ok},{running}"))), None), Some(Checks::Pending));
        let failed = run("completed", r#""failure""#);
        assert_eq!(combine_checks(Some(&runs(&format!("{ok},{running},{failed}"))), None), Some(Checks::Failure));
        // An older CI reporting through commit statuses.
        assert_eq!(combine_checks(None, Some(&json(r#"{"state":"error","total_count":1}"#))), Some(Checks::Failure));
    }

    /// The repository's pull requests and each branch's CI, read from a fake GitHub API.
    #[test]
    fn pull_requests_and_ci_are_read() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let api = format!("http://{}", server.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in server.incoming().flatten() {
                std::thread::spawn(move || {
                    use std::io::{Read, Write};
                    let mut s = stream;
                    let mut req = [0; 4096];
                    let n = s.read(&mut req).unwrap_or(0);
                    let line = String::from_utf8_lossy(&req[..n]).lines().next().unwrap_or("").to_owned();
                    let path = line.split(' ').nth(1).unwrap_or("");
                    let pr = |n: u64, owner: &str| {
                        format!(r#"{{"number":{n},"title":"PR {n}","html_url":"https://github.com/a/r/pull/{n}","draft":false,"head":{{"ref":"feature","repo":{{"full_name":"{owner}/r"}}}}}}"#)
                    };
                    let body = match path {
                        "/repos/a/r" => r#"{"default_branch":"trunk"}"#.to_owned(),
                        p if p.starts_with("/repos/a/r/pulls") => format!("[{},{}]", pr(7, "a"), pr(8, "fork")),
                        p if p.starts_with("/repos/a/r/commits?") => {
                            r#"[{"commit":{"author":{"email":"Ada@Example.com"}},"author":{"avatar_url":"https://avatars.githubusercontent.com/u/1?v=4"}},
                                {"commit":{"author":{"email":"inconnu@example.com"}},"author":null}]"#.to_owned()
                        }
                        p if p.starts_with("/repos/a/r/commits/aaa/check-runs") => r#"{"check_runs":[{"status":"completed","conclusion":"failure"}]}"#.to_owned(),
                        p if p.starts_with("/repos/a/r/commits/bbb/check-runs") => r#"{"check_runs":[{"status":"completed","conclusion":"success"}]}"#.to_owned(),
                        p if p.ends_with("/status") => r#"{"state":"pending","total_count":0}"#.to_owned(),
                        _ => "{}".to_owned(),
                    };
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                });
            }
        });
        let branches = vec![("feature".to_owned(), "aaa".to_owned()), ("trunk".to_owned(), "bbb".to_owned()), ("vieille".to_owned(), "ccc".to_owned())];
        let info = repo_info_from(&api, "t0k", "a/r", branches).unwrap();
        assert_eq!(info.default_branch, "trunk");
        // The fork's pull request has a branch of the same name, but not in this repository.
        let pulls: Vec<(u64, &str)> = info.pulls.iter().map(|p| (p.number, p.branch.as_str())).collect();
        assert_eq!(pulls, [(7, "feature")]);
        assert_eq!(info.checks.get("feature"), Some(&Checks::Failure));
        assert_eq!(info.checks.get("trunk"), Some(&Checks::Success));
        assert_eq!(info.checks.get("vieille"), None, "no CI on that one");
        // Authors with a GitHub account, by e-mail in lower case; the others have no picture.
        assert_eq!(info.avatars.get("ada@example.com").map(String::as_str), Some("https://avatars.githubusercontent.com/u/1?v=4"));
        assert_eq!(info.avatars.len(), 1);
    }

    #[test]
    fn noreply_addresses_name_the_account() {
        assert_eq!(noreply_avatar("583231+Octocat@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/u/583231"));
        assert_eq!(noreply_avatar("octocat@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/octocat"));
        assert_eq!(noreply_avatar("ada@example.com"), None);
    }

    #[test]
    fn polling_survives_network_errors() {
        let json = |s: &str| Ok(serde_json::from_str::<Value>(s).unwrap());
        assert_eq!(poll_step(Err("GitHub injoignable".into()), 5), Poll::Wait(5));
        assert_eq!(poll_step(json(r#"{"error":"authorization_pending"}"#), 5), Poll::Wait(5));
        assert_eq!(poll_step(json(r#"{"error":"slow_down","interval":10}"#), 5), Poll::Wait(10));
        assert_eq!(poll_step(json(r#"{"access_token":"t"}"#), 5), Poll::Token("t".into()));
        assert_eq!(poll_step(json(r#"{"error":"expired_token"}"#), 5), Poll::Expired);
        assert!(matches!(poll_step(json(r#"{"error":"access_denied"}"#), 5), Poll::Fail(_)));
    }

    /// With two github.com entries stored (the user's own token and Rameau's), Rameau must
    /// read and delete only its own. Checked with git's `store` helper on a temp file.
    #[test]
    fn only_rameau_entry_is_read_and_deleted() {
        let dir = std::env::temp_dir().join(format!("rameau-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("store");
        std::fs::write(&file, "https://alice:user-pat@github.com\n").unwrap();
        let helper = format!("credential.helper=store --file=\"{}\"", file.display());
        let git = |action: &str, input: &str| {
            git::exec(&dir, &["-c", "credential.helper=", "-c", &helper, "credential", action], Some(input), None, &[])
        };
        git("approve", &entry(Some("rameau-token"))).unwrap();
        assert!(git("fill", &entry(None)).unwrap().contains("password=rameau-token"));
        git("reject", &entry(None)).unwrap();
        let left = std::fs::read_to_string(&file).unwrap();
        assert!(left.contains("alice:user-pat") && !left.contains("rameau-token"), "{left}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
