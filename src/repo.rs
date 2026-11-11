use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use git2::{BranchType, Delta, Diff, DiffOptions, Oid, Repository, RepositoryState, Sort, Status, StatusOptions};

use crate::graph::{self, Row};

// shortcut: commits past this are not listed at all; raise it if someone scrolls that far.
const MAX_COMMITS: usize = 500_000;
/// Commits read and laid out at a time: only the ones scrolled to take memory.
pub const PAGE: usize = 100;

#[derive(Clone, Copy, PartialEq)]
pub enum RefKind {
    Head,
    Local,
    Remote,
    Tag,
}

#[derive(Clone)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

pub struct Commit {
    pub id: Oid,
    pub summary: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub refs: Vec<RefLabel>,
}

pub struct Branch {
    pub name: String,
    pub kind: RefKind,
    pub row: Option<usize>,
}

pub struct Stash {
    pub index: usize,
    pub message: String,
    pub id: Oid,
}

pub struct History {
    pub name: String,
    pub stashes: Vec<Stash>,
    pub head: String,
    /// Local branch HEAD points to (None when detached).
    pub head_branch: Option<String>,
    /// HEAD points to a commit (false in a repository without commits: nothing to push).
    pub has_commits: bool,
    /// (ahead, behind) of the HEAD branch vs its upstream; None when it has no upstream.
    pub ahead_behind: Option<(usize, usize)>,
    /// The HEAD commit is on a server already: undoing or amending it would rewrite shared history.
    pub head_published: bool,
    pub state: RepositoryState,
    pub changes: Changes,
    /// Every commit of the history, in display order (20 bytes each).
    pub order: Vec<Oid>,
    /// The commits read so far: the first `commits.len()` of `order`, with their graph rows.
    pub commits: Vec<Commit>,
    pub rows: Vec<Row>,
    layout: graph::Layout<Oid>,
    /// Labels of the commits not read yet.
    labels: HashMap<Oid, Vec<RefLabel>>,
    /// Lane color of commits carrying a ref: from the branch name, so it doesn't shift.
    ref_colors: HashMap<Oid, usize>,
    /// Row (index in `order`) of each branch tip.
    pub branches: Vec<Branch>,
    /// Widest graph among the rows read so far.
    pub max_lanes: usize,
    /// `fingerprint` at load time: when it hasn't moved, the history is still up to date.
    pub fingerprint: u64,
}

pub struct Details {
    pub id: String,
    pub message: String,
    pub author: String,
    pub email: String,
    pub date: String,
    pub parents: Vec<String>,
    pub files: Vec<(char, String)>,
}

/// Reads the refs, the status and the order of the whole history, but the commits themselves
/// only up to `at_least` (at least a page): `load_more` reads the next ones when scrolled to.
pub fn load(repo: &mut Repository, at_least: usize) -> Result<History, git2::Error> {
    let mut stashes = Vec::new();
    repo.stash_foreach(|index, message, id| {
        stashes.push(Stash { index, message: message.to_owned(), id: *id });
        true
    })?;
    let repo = &*repo;
    let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
    let mut branches = Vec::new();
    let head = repo.head().ok();
    // A repository without commits has no HEAD commit, but HEAD still names its branch.
    let unborn = || {
        let r = repo.find_reference("HEAD").ok()?;
        r.symbolic_target().map(|t| t.trim_start_matches("refs/heads/").to_owned())
    };
    let head_name = head
        .as_ref()
        .and_then(|h| h.shorthand().map(str::to_owned))
        .or_else(unborn)
        .unwrap_or_else(|| "HEAD".into());
    let detached = repo.head_detached().unwrap_or(false);
    let head_branch = (!detached).then(|| head_name.clone());
    let ahead_behind = head_branch.as_ref().and_then(|name| {
        let local = repo.find_branch(name, BranchType::Local).ok()?;
        let upstream = local.upstream().ok()?;
        repo.graph_ahead_behind(local.get().target()?, upstream.get().target()?).ok()
    });

    for b in repo.branches(None)? {
        let (b, ty) = b?;
        let Some(name) = b.name()?.map(str::to_owned) else { continue };
        if name.ends_with("/HEAD") {
            continue;
        }
        let Some(target) = b.get().peel_to_commit().ok().map(|c| c.id()) else { continue };
        let kind = match ty {
            BranchType::Local if b.is_head() => RefKind::Head,
            BranchType::Local => RefKind::Local,
            BranchType::Remote => RefKind::Remote,
        };
        labels.entry(target).or_default().push(RefLabel { name: name.clone(), kind });
        branches.push((name, kind, target));
    }
    if detached && let Some(id) = head.as_ref().and_then(|h| h.target()) {
        labels.entry(id).or_default().insert(0, RefLabel { name: "HEAD".into(), kind: RefKind::Head });
    }
    repo.tag_foreach(|id, name| {
        let name = String::from_utf8_lossy(name).trim_start_matches("refs/tags/").to_owned();
        // Annotated tags point to a tag object: peel to the commit.
        let id = repo.find_object(id, None).and_then(|o| o.peel_to_commit()).map_or(id, |c| c.id());
        labels.entry(id).or_default().push(RefLabel { name, kind: RefKind::Tag });
        true
    })?;

    // Lane color comes from the branch name (`origin/x` and `x` share it) or the commit id,
    // so it doesn't shift when new commits arrive.
    let key = |name: &str, kind: RefKind| -> String {
        match kind {
            RefKind::Remote => name.split_once('/').map_or(name, |(_, b)| b).to_owned(),
            _ => name.to_owned(),
        }
    };
    let ref_colors: HashMap<Oid, usize> = labels
        .iter()
        .filter_map(|(id, ls)| ls.iter().min_by_key(|l| l.kind as u8).map(|l| (*id, graph::color_index(key(&l.name, l.kind).as_bytes()))))
        .collect();

    let tips: Vec<Oid> = branches.iter().map(|(_, _, target)| *target).collect();
    let with_head = head.as_ref().is_some_and(|h| h.target().is_some());
    let order = match git_order(repo, with_head) {
        Some(order) => order,
        None => walk_order(repo, &tips, with_head)?,
    };
    let index: HashMap<Oid, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut branches: Vec<Branch> = branches
        .into_iter()
        .map(|(name, kind, target)| Branch { name, kind, row: index.get(&target).copied() })
        .collect();
    branches.sort_by(|a, b| (a.kind == RefKind::Remote, &a.name).cmp(&(b.kind == RefKind::Remote, &b.name)));

    let name = repo
        .workdir()
        .unwrap_or(repo.path())
        .file_name()
        .map_or("repo".into(), |n| n.to_string_lossy().into_owned());
    let head_published = match ahead_behind {
        Some((ahead, _)) => ahead == 0,
        // No upstream: any remote branch containing it (the branch was created from main…).
        None => with_head && {
            let args = ["for-each-ref", "--count=1", "--contains", "HEAD", "--format=%(refname)", "refs/remotes"];
            crate::git::run(repo.workdir().unwrap_or(repo.path()), &args).is_ok_and(|out| !out.is_empty())
        },
    };
    let mut history = History {
        name,
        stashes,
        head: head_name,
        head_branch,
        has_commits: with_head,
        ahead_behind,
        head_published,
        state: repo.state(),
        changes: changes(repo)?,
        order,
        commits: Vec::new(),
        rows: Vec::new(),
        layout: graph::Layout::default(),
        labels,
        ref_colors,
        branches,
        max_lanes: 1,
        fingerprint: fingerprint(repo),
    };
    load_more(repo, &mut history, at_least.max(PAGE))?;
    Ok(history)
}

/// Every commit reachable from the branches and HEAD, children before parents, newest first.
/// The git command reads the commit-graph file git keeps up to date when fetching: on cargo's
/// 24,000 commits it answers in 20 ms, where libgit2's walk takes 150.
fn git_order(repo: &Repository, with_head: bool) -> Option<Vec<Oid>> {
    let args = history_args("rev-list", with_head, None);
    let out = crate::git::run(repo.workdir().unwrap_or(repo.path()), &args.iter().map(String::as_str).collect::<Vec<_>>()).ok()?;
    out.lines().map(|l| Oid::from_str(l).ok()).collect()
}

/// `git <cmd>` over the commits the history shows, in its order.
fn history_args(cmd: &str, with_head: bool, format: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [cmd, "--date-order", &format!("--max-count={MAX_COMMITS}"), "--branches", "--remotes"].map(str::to_owned).into();
    args.extend(format.map(|f| format!("--format={f}")));
    if with_head {
        args.push("HEAD".into());
    }
    args
}

/// A commit as the history search sees it.
pub struct Found {
    pub id: Oid,
    pub summary: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    /// Summary and author in lower case, what the search compares with.
    text: String,
}

/// Every commit of the history with what can be searched, read once by `git log` (cargo's 24,000
/// commits: ~100 ms, 3 MB), then searched in memory at each key press.
pub fn search_index(dir: &std::path::Path, with_head: bool) -> Result<Vec<Found>, String> {
    let args = history_args("log", with_head, Some("%H%x00%an%x00%ae%x00%at%x00%s"));
    let out = crate::git::run(dir, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(5, '\0');
            let (id, author, email, time) = (Oid::from_str(f.next()?).ok()?, f.next()?, f.next()?, f.next()?);
            let summary = f.next().unwrap_or("");
            let text = format!("{summary}\n{author}").to_lowercase();
            Some(Found { id, summary: summary.to_owned(), author: author.to_owned(), email: email.to_owned(), time: time.parse().ok()?, text })
        })
        .collect())
}

/// Commits (indexes in `index`) whose summary or author holds every word of the search, or whose
/// hash starts with it.
pub fn search(index: &[Found], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let hash = q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit());
    index
        .iter()
        .enumerate()
        .filter(|(_, f)| (hash && f.id.to_string().starts_with(&q)) || (!words.is_empty() && words.iter().all(|w| f.text.contains(w))))
        .map(|(i, _)| i)
        .collect()
}

/// Same order through libgit2, when the git command can't answer.
fn walk_order(repo: &Repository, tips: &[Oid], with_head: bool) -> Result<Vec<Oid>, git2::Error> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    for tip in tips {
        walk.push(*tip)?;
    }
    if with_head {
        walk.push_head()?;
    }
    walk.take(MAX_COMMITS).collect()
}

/// Reads the commits of `order` up to `upto` (excluded) and lays out their graph rows.
pub fn load_more(repo: &Repository, h: &mut History, upto: usize) -> Result<(), git2::Error> {
    for i in h.commits.len()..upto.min(h.order.len()) {
        let c = repo.find_commit(h.order[i])?;
        let parents: Vec<Oid> = c.parent_ids().collect();
        let colors = &h.ref_colors;
        let color_of = |id: &Oid| colors.get(id).copied().unwrap_or_else(|| graph::color_index(id.as_bytes()));
        let row = h.layout.push(&c.id(), &parents, color_of);
        h.max_lanes = h.max_lanes.max(row.segments.iter().map(|s| s.from.max(s.to) as usize).chain([row.lane]).max().unwrap_or(0) + 1);
        h.rows.push(row);
        h.commits.push(Commit {
            id: c.id(),
            summary: c.summary().unwrap_or("").to_owned(),
            author: c.author().name().unwrap_or("").to_owned(),
            email: c.author().email().unwrap_or("").to_owned(),
            time: c.time().seconds(),
            refs: h.labels.remove(&c.id()).unwrap_or_default(),
        });
    }
    Ok(())
}

impl History {
    /// More commits to read below the last one shown.
    pub fn has_more(&self) -> bool {
        self.commits.len() < self.order.len()
    }
}

/// Files changed in the working copy, split like `git status`.
#[derive(Default)]
pub struct Changes {
    pub staged: Vec<(char, String)>,
    pub unstaged: Vec<(char, String)>,
    pub conflicted: Vec<String>,
    /// The same files once each, as the changes list shows them.
    pub files: Vec<FileEntry>,
}

/// How much of a file goes into the next commit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Check {
    All,
    Part,
    None,
}

impl FileEntry {
    /// The diff shown when the file is clicked: what's left to check, else what's in the commit.
    pub fn diff_source(&self) -> DiffSource {
        if self.check == Check::All { DiffSource::Staged } else { DiffSource::Unstaged }
    }
}

/// A changed file of the working copy: conflicts first, then by path.
#[derive(Clone, PartialEq, Debug)]
pub struct FileEntry {
    pub path: String,
    /// A (added), D, R, M, or U (conflict).
    pub status: char,
    pub check: Check,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.conflicted.is_empty()
    }
}

pub fn changes(repo: &Repository) -> Result<Changes, git2::Error> {
    let mut out = Changes::default();
    if repo.is_bare() {
        return Ok(out);
    }
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    for e in repo.statuses(Some(&mut opts))?.iter() {
        let s = e.status();
        let path = e.path().unwrap_or("").to_owned();
        if s.is_conflicted() {
            out.conflicted.push(path);
            continue;
        }
        let letter = |new, deleted, renamed| match () {
            _ if s.intersects(new) => 'A',
            _ if s.intersects(deleted) => 'D',
            _ if s.intersects(renamed) => 'R',
            _ => 'M',
        };
        if s.intersects(Status::INDEX_NEW | Status::INDEX_MODIFIED | Status::INDEX_DELETED | Status::INDEX_RENAMED | Status::INDEX_TYPECHANGE) {
            out.staged.push((letter(Status::INDEX_NEW, Status::INDEX_DELETED, Status::INDEX_RENAMED), path.clone()));
        }
        if s.intersects(Status::WT_NEW | Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_RENAMED | Status::WT_TYPECHANGE) {
            out.unstaged.push((letter(Status::WT_NEW, Status::WT_DELETED, Status::WT_RENAMED), path));
        }
    }
    out.files = file_entries(&out);
    Ok(out)
}

fn file_entries(ch: &Changes) -> Vec<FileEntry> {
    let unstaged: HashMap<&str, char> = ch.unstaged.iter().map(|(s, p)| (p.as_str(), *s)).collect();
    let staged: HashMap<&str, char> = ch.staged.iter().map(|(s, p)| (p.as_str(), *s)).collect();
    let mut files: Vec<FileEntry> = ch.staged.iter().map(|(_, p)| p).chain(ch.unstaged.iter().map(|(_, p)| p).filter(|p| !staged.contains_key(p.as_str()))).map(|path| {
        // What the commit will record wins: a file added then edited again stays "A".
        let (s, u) = (staged.get(path.as_str()), unstaged.get(path.as_str()));
        let check = match (s, u) {
            (Some(_), None) => Check::All,
            (Some(_), Some(_)) => Check::Part,
            _ => Check::None,
        };
        FileEntry { path: path.clone(), status: *s.or(u).unwrap_or(&'M'), check }
    }).collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let conflicts = ch.conflicted.iter().map(|path| FileEntry { path: path.clone(), status: 'U', check: Check::None });
    conflicts.chain(files).collect()
}

/// Which version of a file the diff view shows.
#[derive(Clone, PartialEq)]
pub enum DiffSource {
    Commit(Oid),
    Staged,
    Unstaged,
}

#[derive(Clone, Copy, PartialEq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Hunk,
    Info,
    /// "\ No newline at end of file", about the line just above.
    NoNewline,
    /// Conflict marker line (`<<<<<<<`, `=======`, `>>>>>>>`) of a file being merged.
    Marker,
}

pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

impl DiffLine {
    /// A message line in the diff view (no line numbers).
    pub fn info(text: &str) -> Self {
        DiffLine { kind: LineKind::Info, old: None, new: None, text: text.to_owned() }
    }
}

/// Lines of the diff of one file. The bool tells whether lines can be staged one by one
/// (the file exists on both sides; added/deleted/binary files go whole).
pub fn file_diff(repo: &Repository, source: &DiffSource, path: &str) -> Result<(Vec<DiffLine>, bool), git2::Error> {
    let mut opts = DiffOptions::new();
    opts.pathspec(path).disable_pathspec_match(true);
    let diff = match source {
        DiffSource::Commit(id) => commit_diff(repo, *id, &mut opts)?,
        DiffSource::Staged => {
            let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            repo.diff_tree_to_index(head.as_ref(), None, Some(&mut opts))?
        }
        DiffSource::Unstaged => {
            opts.include_untracked(true).recurse_untracked_dirs(true).show_untracked_content(true);
            repo.diff_index_to_workdir(None, Some(&mut opts))?
        }
    };
    // A conflicted file has no diff: show it as it is on disk, markers included.
    if *source == DiffSource::Unstaged && diff.deltas().any(|d| d.status() == Delta::Conflicted) {
        return Ok((conflict_lines(repo, path), false));
    }
    let mut partial = diff.deltas().len() == 1 && diff.deltas().all(|d| d.status() == Delta::Modified);
    let mut lines = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_, hunk, l| {
        // Line patches are rebuilt from text: a non-UTF-8 file (Latin-1…) would get "�" written
        // into it. Such files are then staged or discarded whole, byte for byte.
        partial &= std::str::from_utf8(l.content()).is_ok();
        let kind = match l.origin() {
            ' ' => LineKind::Context,
            '+' => LineKind::Added,
            '-' => LineKind::Removed,
            'H' => LineKind::Hunk,
            'B' => LineKind::Info,
            '=' | '>' | '<' => LineKind::NoNewline,
            _ => return true, // file headers
        };
        // Raw text (tabs, \r kept): patches built from these lines must match the file exactly.
        let text = match kind {
            // Shown in the diff only; patches write git's own marker.
            LineKind::NoNewline => "Pas de retour à la ligne à la fin du fichier".to_owned(),
            _ => String::from_utf8_lossy(l.content()).trim_end_matches('\n').to_owned(),
        };
        let (old, new) = match (kind, hunk) {
            (LineKind::Hunk, Some(h)) => (Some(h.old_start()), Some(h.new_start())),
            _ => (l.old_lineno(), l.new_lineno()),
        };
        lines.push(DiffLine { kind, old, new, text });
        true
    })?;
    if lines.is_empty() {
        lines.push(DiffLine { kind: LineKind::Info, old: None, new: None, text: "Aucune différence affichable (fichier binaire ou vide).".into() });
    }
    Ok((lines, partial))
}

/// The working file of a conflicted path, line by line, markers flagged, with how to resolve it.
fn conflict_lines(repo: &Repository, path: &str) -> Vec<DiffLine> {
    let info = DiffLine::info;
    let file = repo.workdir().map(|w| w.join(path));
    let Some(bytes) = file.and_then(|f| std::fs::read(f).ok()) else {
        return vec![info("Fichier en conflit, absent du disque : il a été supprimé d'un côté et modifié de l'autre.")];
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = vec![info("Garde la bonne version entre les marqueurs dans ton éditeur, enregistre, puis coche le fichier.")];
    lines.extend(text.split_inclusive('\n').enumerate().map(|(i, l)| {
        let l = l.trim_end_matches('\n');
        let marker = ["<<<<<<<", "|||||||", "=======", ">>>>>>>"].iter().any(|m| l.starts_with(m));
        DiffLine { kind: if marker { LineKind::Marker } else { LineKind::Context }, old: None, new: Some(i as u32 + 1), text: l.to_owned() }
    }));
    lines
}

/// Full (old, new) contents of the file on both sides of the diff, for syntax highlighting.
/// Files over this size get no syntax colors (generated, minified, dumps): they are not even read.
pub const MAX_HIGHLIGHT_BYTES: usize = 1 << 20;

pub fn file_versions(repo: &Repository, source: &DiffSource, path: &str) -> (Option<String>, Option<String>) {
    let p = std::path::Path::new(path);
    // Size from the object header first: loading a 2 GB blob just to skip it is what we avoid.
    let small = |id: Oid| repo.odb().and_then(|o| o.read_header(id)).is_ok_and(|(size, _)| size <= MAX_HIGHLIGHT_BYTES);
    let text = |id: Oid| -> Option<String> {
        let blob = small(id).then(|| repo.find_blob(id).ok()).flatten()?;
        (!blob.is_binary()).then(|| String::from_utf8_lossy(blob.content()).into_owned())
    };
    let blob = |tree: Option<git2::Tree>| -> Option<String> { text(tree?.get_path(p).ok()?.id()) };
    let index = || -> Option<String> { text(repo.index().ok()?.get_path(p, 0)?.id) };
    let head = || repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    match source {
        DiffSource::Commit(id) => {
            let c = repo.find_commit(*id).ok();
            let parent = c.as_ref().and_then(|c| c.parent(0).ok()).and_then(|p| p.tree().ok());
            (blob(parent), blob(c.and_then(|c| c.tree().ok())))
        }
        DiffSource::Staged => (blob(head()), index()),
        DiffSource::Unstaged => {
            let file = repo.workdir().map(|w| w.join(p));
            // The file itself, never what a symlink points to (`/dev/zero` reports a size of 0).
            let fits = file
                .as_ref()
                .and_then(|f| std::fs::symlink_metadata(f).ok())
                .is_some_and(|m| m.is_file() && m.len() <= MAX_HIGHLIGHT_BYTES as u64);
            let work = file.filter(|_| fits).and_then(|f| std::fs::read(f).ok());
            (index(), work.map(|b| String::from_utf8_lossy(&b).into_owned()))
        }
    }
}

/// Commits from `from` (included) up to HEAD, oldest first, for an interactive rebase.
/// Returns the base to pass to `git rebase -i` (None: rebase from the root commit).
/// Each commit is (id, summary, full message).
pub fn rebase_range(repo: &Repository, from: Oid) -> Result<(Option<Oid>, Vec<(Oid, String, String)>), String> {
    let head = repo.head().ok().and_then(|h| h.target()).ok_or("Pas de commit HEAD.")?;
    if head != from && !repo.graph_descendant_of(head, from).unwrap_or(false) {
        return Err("Ce commit n'est pas dans l'historique de la branche actuelle.".into());
    }
    let from_commit = repo.find_commit(from).map_err(|e| e.message().to_owned())?;
    let base = from_commit.parent_id(0).ok();
    let mut walk = repo.revwalk().map_err(|e| e.message().to_owned())?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE).map_err(|e| e.message().to_owned())?;
    walk.push(head).map_err(|e| e.message().to_owned())?;
    if let Some(base) = base {
        walk.hide(base).map_err(|e| e.message().to_owned())?;
    }
    let mut out = Vec::new();
    for id in walk {
        let c = repo.find_commit(id.map_err(|e| e.message().to_owned())?).map_err(|e| e.message().to_owned())?;
        if c.parent_count() > 1 {
            return Err("La plage contient des commits de merge : le rebase interactif ne les gère pas.".into());
        }
        out.push((c.id(), c.summary().unwrap_or("").to_owned(), c.message().unwrap_or("").trim().to_owned()));
    }
    Ok((base, out))
}

/// HEAD no longer points where it did (commit made meanwhile in a terminal or an editor).
pub fn head_moved(git_dir: &std::path::Path, expected: Oid) -> bool {
    Repository::open(git_dir).ok().and_then(|r| r.head().ok()?.target()) != Some(expected)
}

fn commit_diff<'r>(repo: &'r Repository, id: Oid, opts: &mut DiffOptions) -> Result<Diff<'r>, git2::Error> {
    let c = repo.find_commit(id)?;
    let parent_tree = c.parent(0).ok().map(|p| p.tree()).transpose()?;
    repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&c.tree()?), Some(opts))
}

/// Cheap summary of everything the history view shows: each ref and where it points (branches,
/// tags, remotes, stash), where HEAD points and the ongoing operation. Reading it takes a few ms,
/// against a full history load for large repositories.
pub fn fingerprint(repo: &Repository) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            (r.name_bytes(), r.target(), r.symbolic_target_bytes()).hash(&mut h);
        }
    }
    if let Ok(head) = repo.find_reference("HEAD") {
        (head.target(), head.symbolic_target_bytes()).hash(&mut h);
    }
    // Stashes live in the reflog of refs/stash: dropping stash@{1} leaves the ref itself unchanged,
    // yet shifts every stash index the sidebar shows.
    if let Ok(log) = repo.reflog("refs/stash") {
        log.iter().for_each(|e| e.id_new().hash(&mut h));
    }
    (repo.state() as u32).hash(&mut h);
    h.finish()
}

impl Details {
    /// What the panel shows for a commit that can't be read.
    pub fn unreadable(id: Oid, error: &str) -> Self {
        Details {
            id: id.to_string(),
            message: format!("Impossible de lire ce commit\n{error}"),
            author: String::new(),
            email: String::new(),
            date: String::new(),
            parents: vec![],
            files: vec![],
        }
    }
}

pub fn details(repo: &Repository, id: Oid) -> Result<Details, git2::Error> {
    let c = repo.find_commit(id)?;
    let diff = commit_diff(repo, id, &mut DiffOptions::new())?;
    let files = diff
        .deltas()
        .map(|d| {
            let status = match d.status() {
                Delta::Added => 'A',
                Delta::Deleted => 'D',
                Delta::Renamed => 'R',
                _ => 'M',
            };
            let path = d.new_file().path().or(d.old_file().path());
            (status, path.map_or(String::new(), |p| p.to_string_lossy().into_owned()))
        })
        .collect();
    let author = c.author();
    Ok(Details {
        id: id.to_string(),
        message: c.message().unwrap_or("").trim().to_owned(),
        author: author.name().unwrap_or("").to_owned(),
        email: author.email().unwrap_or("").to_owned(),
        date: relative_time(author.when().seconds()),
        parents: c.parent_ids().map(|p| p.to_string()[..7].to_owned()).collect(),
        files,
    })
}

pub fn relative_time(t: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let s = (now - t).max(0);
    let (n, unit) = match s {
        0..60 => return "à l'instant".into(),
        60..3600 => (s / 60, "min"),
        3600..86400 => (s / 3600, "h"),
        86400..2_592_000 => (s / 86400, "j"),
        2_592_000..31_536_000 => (s / 2_592_000, "mois"),
        _ => (s / 31_536_000, "an"),
    };
    let plural = if unit == "an" && n > 1 { "s" } else { "" };
    format!("il y a {n} {unit}{plural}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{git, temp};

    #[test]
    fn changes_are_split_like_git_status() {
        let dir = temp("status");
        git(&dir, &["init", "-q", "-b", "main"]);
        for f in ["a", "b", "c", "k"] {
            std::fs::write(dir.join(f), "1\n").unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        // A conflict on k: changed differently on two branches, then merged.
        git(&dir, &["switch", "-qc", "x"]);
        std::fs::write(dir.join("k"), "x\n").unwrap();
        git(&dir, &["commit", "-qam", "x"]);
        git(&dir, &["switch", "-q", "main"]);
        std::fs::write(dir.join("k"), "main\n").unwrap();
        git(&dir, &["commit", "-qam", "main"]);
        // With an identity: on machines without one, git stops before even trying to merge.
        assert!(crate::git::run(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "x"]).is_err());
        // Then every other kind of change.
        std::fs::write(dir.join("a"), "2\n").unwrap(); // modified, not staged
        std::fs::write(dir.join("b"), "2\n").unwrap(); // modified and staged
        git(&dir, &["add", "b"]);
        std::fs::remove_file(dir.join("c")).unwrap(); // deleted, not staged
        std::fs::write(dir.join("n"), "new\n").unwrap(); // untracked
        std::fs::write(dir.join("s"), "new\n").unwrap(); // new and staged
        git(&dir, &["add", "s"]);

        let ch = changes(&Repository::open(&dir).unwrap()).unwrap();
        let mut staged = ch.staged.clone();
        let mut unstaged = ch.unstaged.clone();
        staged.sort();
        unstaged.sort();
        assert_eq!(staged, [('A', "s".to_owned()), ('M', "b".to_owned())]);
        assert_eq!(unstaged, [('A', "n".to_owned()), ('D', "c".to_owned()), ('M', "a".to_owned())]);
        assert_eq!(ch.conflicted, ["k"]);
        // One line per file, conflicts first; b is half in the commit after a new edit.
        std::fs::write(dir.join("b"), "3\n").unwrap();
        let ch = changes(&Repository::open(&dir).unwrap()).unwrap();
        let files: Vec<(&str, char, Check)> = ch.files.iter().map(|f| (f.path.as_str(), f.status, f.check)).collect();
        assert_eq!(
            files,
            [("k", 'U', Check::None), ("a", 'M', Check::None), ("b", 'M', Check::Part), ("c", 'D', Check::None), ("n", 'A', Check::None), ("s", 'A', Check::All)]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_moves_with_history_only() {
        let dir = temp("fingerprint");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f"), "1").unwrap();
        git(&dir, &["add", "f"]);
        git(&dir, &["commit", "-qm", "c1"]);
        let fp = || fingerprint(&Repository::open(&dir).unwrap());
        let start = fp();
        // Editing or staging a file: same history.
        std::fs::write(dir.join("f"), "2").unwrap();
        git(&dir, &["add", "f"]);
        assert_eq!(fp(), start);
        // Everything that changes what the history shows moves it.
        let mut seen = vec![start];
        let mut moved = |label: &str| {
            let now = fp();
            assert!(!seen.contains(&now), "{label} did not change the fingerprint");
            seen.push(now);
        };
        git(&dir, &["commit", "-qm", "c2"]);
        moved("commit");
        git(&dir, &["branch", "side"]);
        moved("new branch");
        git(&dir, &["tag", "v1"]);
        moved("tag");
        git(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
        moved("fetch");
        std::fs::write(dir.join("f"), "3").unwrap();
        git(&dir, &["stash", "-q"]);
        moved("stash");
        std::fs::write(dir.join("f"), "4").unwrap();
        git(&dir, &["stash", "-q"]);
        moved("second stash");
        git(&dir, &["stash", "drop", "-q", "stash@{1}"]);
        moved("drop of an older stash");
        git(&dir, &["switch", "-q", "--detach", "HEAD~1"]);
        moved("checkout");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latin1_files_are_never_patched_line_by_line() {
        let dir = temp("latin1");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("f.txt"), b"a\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        std::fs::write(dir.join("f.txt"), b"a\ncaf\xe9\n").unwrap();
        std::fs::write(dir.join("g.txt"), b"x\n").unwrap();
        let repo = Repository::open(&dir).unwrap();
        let (_, partial) = file_diff(&repo, &DiffSource::Unstaged, "f.txt").unwrap();
        assert!(!partial, "a line patch would turn \\xe9 into U+FFFD");
        // Staging the whole file keeps the exact bytes.
        git(&dir, &["add", "--", "f.txt"]);
        assert_eq!(std::process::Command::new("git").current_dir(&dir).args(["show", ":f.txt"]).output().unwrap().stdout, b"a\ncaf\xe9\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflicted_file_shows_its_markers() {
        let dir = temp("conflict-view");
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("k"), "a\nb\nc\n").unwrap();
        git(&dir, &["add", "k"]);
        git(&dir, &["commit", "-qm", "i"]);
        git(&dir, &["switch", "-qc", "x"]);
        std::fs::write(dir.join("k"), "a\nX\nc\n").unwrap();
        git(&dir, &["commit", "-qam", "x"]);
        git(&dir, &["switch", "-q", "main"]);
        std::fs::write(dir.join("k"), "a\nM\nc\n").unwrap();
        git(&dir, &["commit", "-qam", "m"]);
        assert!(crate::git::run(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "merge", "x"]).is_err());
        let (lines, selectable) = file_diff(&Repository::open(&dir).unwrap(), &DiffSource::Unstaged, "k").unwrap();
        assert!(!selectable);
        assert_eq!(lines.iter().filter(|l| l.kind == LineKind::Marker).count(), 3);
        // With Git for Windows' default CRLF the lines end with \r (the view drops it when drawing).
        let has = |t: &str| lines.iter().any(|l| l.kind == LineKind::Context && l.text.trim_end_matches('\r') == t);
        assert!(has("M") && has("X"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parents_always_come_after_children() {
        let dir = temp("skew");
        git(&dir, &["init", "-q", "-b", "main"]);
        // c2's clock was behind: it claims to be older than its parent c1.
        let dated = |msg: &str, date: &str| {
            let env = [("GIT_COMMITTER_DATE", date), ("GIT_AUTHOR_DATE", date), ("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t"), ("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t")];
            let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            crate::git::exec(&dir, &["commit", "-q", "--allow-empty", "-m", msg], None, None, &env).unwrap();
        };
        dated("c0", "2020-01-01T00:00:00");
        dated("c1", "2022-01-01T00:00:00");
        dated("c2", "2021-01-01T00:00:00");
        git(&dir, &["switch", "-qc", "side", "HEAD~2"]);
        dated("s1", "2023-01-01T00:00:00");
        let h = load(&mut Repository::open(&dir).unwrap(), 0).unwrap();
        let pos: HashMap<Oid, usize> = h.commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        let repo = Repository::open(&dir).unwrap();
        for c in &h.commits {
            for p in repo.find_commit(c.id).unwrap().parent_ids() {
                assert!(pos[&p] > pos[&c.id], "{} listed after its parent", c.summary);
            }
        }
        assert_eq!(h.commits.len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_is_read_page_by_page() {
        let dir = temp("pages");
        git(&dir, &["init", "-q", "-b", "main"]);
        // 250 commits on main, a side branch leaving at the 5th and merged at the 150th
        // (fast-import: one git process instead of 250).
        let mut stream = String::new();
        let mut commit = |mark: usize, branch: &str, msg: &str, from: &[usize]| {
            stream += &format!("commit refs/heads/{branch}\nmark :{mark}\ncommitter t <t@t> {} +0000\ndata {}\n{msg}\n", 1_700_000_000 + mark, msg.len());
            for (i, f) in from.iter().enumerate() {
                stream += &format!("{} :{f}\n", if i == 0 { "from" } else { "merge" });
            }
        };
        for i in 1..=250 {
            match i {
                150 => commit(i, "main", "merge", &[149, 1002]),
                i => commit(i, "main", &format!("c{i}"), &(1..i).rev().take(1).collect::<Vec<_>>()),
            }
            if i == 5 {
                commit(1001, "side", "s1", &[5]);
                commit(1002, "side", "s2", &[1001]);
            }
        }
        crate::git::exec(&dir, &["fast-import", "--quiet"], Some(&stream), None, &[]).unwrap();
        git(&dir, &["branch", "old", ":/^c3$"]);

        let repo = Repository::open(&dir).unwrap();
        let mut h = load(&mut Repository::open(&dir).unwrap(), 0).unwrap();
        assert_eq!((h.commits.len(), h.order.len()), (PAGE, 252), "only the first page is read");
        assert!(h.has_more());
        // A branch far down still knows its row, before it is read.
        let old = h.branches.iter().find(|b| b.name == "old").unwrap().row.unwrap();
        assert!(old > PAGE);
        load_more(&repo, &mut h, old + 1).unwrap();
        assert_eq!(h.commits[old].summary, "c3");
        load_more(&repo, &mut h, usize::MAX).unwrap();
        assert!(!h.has_more());
        // Read in pages or at once: same commits, same graph.
        let whole = load(&mut Repository::open(&dir).unwrap(), usize::MAX).unwrap();
        let ids = |h: &History| h.commits.iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(ids(&h), ids(&whole));
        assert_eq!(format!("{:?}", h.rows), format!("{:?}", whole.rows));
        assert_eq!(h.max_lanes, 2);
        // git's order and libgit2's (the fallback) list the same commits, children first.
        let tips: Vec<Oid> = ["main", "side", "old"].iter().map(|b| repo.revparse_single(b).unwrap().id()).collect();
        let (by_git, by_walk) = (git_order(&repo, true).unwrap(), walk_order(&repo, &tips, true).unwrap());
        let sorted = |v: &[Oid]| {
            let mut v = v.to_vec();
            v.sort();
            v
        };
        assert_eq!(sorted(&by_git), sorted(&by_walk));
        assert_eq!(by_git, ids(&whole));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_search_finds_message_author_and_hash() {
        let dir = temp("search");
        git(&dir, &["init", "-q", "-b", "main"]);
        let commit = |author: &str, msg: &str| {
            let who = format!("user.name={author}");
            crate::git::run(&dir, &["-c", &who, "-c", "user.email=a@a", "commit", "-q", "--allow-empty", "-m", msg]).unwrap();
        };
        commit("Ada Lovelace", "Corrige le calcul des marges");
        commit("Bob", "Ajoute l'export PDF\n\nCorps du message : marges");
        commit("Ada Lovelace", "Export CSV");
        let index = search_index(&dir, true).unwrap();
        let found = |q: &str| search(&index, q).iter().map(|&i| index[i].summary.clone()).collect::<Vec<_>>();
        // Newest first, like the history; words in any case, from the summary or the author.
        assert_eq!(found("export"), ["Export CSV", "Ajoute l'export PDF"]);
        assert_eq!(found("ADA export"), ["Export CSV"]);
        assert_eq!(found("marges"), ["Corrige le calcul des marges"], "the summary only, not the body");
        let hash = index[2].id.to_string();
        assert_eq!(found(&hash[..7]), ["Corrige le calcul des marges"]);
        assert!(found("   ").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn head_is_published_once_on_a_server() {
        let root = temp("published");
        git(&root, &["init", "-q", "--bare", "-b", "main", "server.git"]);
        git(&root, &["clone", "-q", "server.git", "work"]);
        let work = root.join("work");
        let published = || load(&mut Repository::open(&work).unwrap(), 0).unwrap().head_published;
        git(&work, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        assert!(!published(), "not pushed yet");
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        assert!(published());
        // A new branch without upstream, still on main's pushed commit…
        git(&work, &["switch", "-qc", "feature"]);
        assert!(published());
        // …then with a commit of its own.
        git(&work, &["commit", "-q", "--allow-empty", "-m", "c2"]);
        assert!(!published());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn head_moved_sees_new_commits() {
        let dir = temp("head-moved");
        git(&dir, &["init", "-q"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "c1"]);
        let open = || Repository::open(&dir).unwrap();
        let head = open().head().unwrap().target().unwrap();
        assert!(!head_moved(open().path(), head));
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "c2"]);
        assert!(head_moved(open().path(), head));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_for_colors() {
        let dir = temp("symlink");
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("cible.rs"), "fn main() {}").unwrap();
        std::os::unix::fs::symlink(dir.join("cible.rs"), dir.join("lien.rs")).unwrap();
        let repo = Repository::open(&dir).unwrap();
        assert_eq!(file_versions(&repo, &DiffSource::Unstaged, "lien.rs").1, None);
        assert!(file_versions(&repo, &DiffSource::Unstaged, "cible.rs").1.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn big_files_are_not_read_for_colors() {
        let dir = temp("big-file");
        git(&dir, &["init", "-q"]);
        let big = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        std::fs::write(dir.join("big.rs"), &big).unwrap();
        std::fs::write(dir.join("small.rs"), "fn main() {}").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        std::fs::write(dir.join("big.rs"), format!("{big}y")).unwrap();
        std::fs::write(dir.join("small.rs"), "fn main() { 1 }").unwrap();
        let repo = Repository::open(&dir).unwrap();
        // Over the limit: neither the index blob nor the working file is loaded.
        assert_eq!(file_versions(&repo, &DiffSource::Unstaged, "big.rs"), (None, None));
        let (old, new) = file_versions(&repo, &DiffSource::Unstaged, "small.rs");
        assert_eq!((old.as_deref(), new.as_deref()), (Some("fn main() {}"), Some("fn main() { 1 }")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_repository_shows_its_branch() {
        let dir = temp("unborn");
        git(&dir, &["init", "-q", "-b", "trunk"]);
        let h = load(&mut Repository::open(&dir).unwrap(), 0).unwrap();
        assert_eq!(h.head, "trunk");
        assert_eq!(h.head_branch.as_deref(), Some("trunk"));
        assert!(!h.has_commits && h.commits.is_empty());
        std::fs::write(dir.join("f"), "1").unwrap();
        git(&dir, &["add", "f"]);
        git(&dir, &["commit", "-qm", "first"]);
        assert!(load(&mut Repository::open(&dir).unwrap(), 0).unwrap().has_commits);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebase_range_picks_commits_and_refuses_merges() {
        let dir = temp("rebase-range");
        git(&dir, &["init", "-q", "-b", "main"]);
        let commit = |name: &str| {
            std::fs::write(dir.join(name), name).unwrap();
            git(&dir, &["add", name]);
            git(&dir, &["commit", "-qm", name]);
            git(&dir, &["rev-parse", "HEAD"])
        };
        let oid = |s: String| Oid::from_str(s.trim()).unwrap();
        let c1 = oid(commit("c1"));
        let c2 = oid(commit("c2"));
        commit("c3");
        let repo = || Repository::open(&dir).unwrap();

        let (base, list) = rebase_range(&repo(), c2).unwrap();
        assert_eq!(base, Some(c1));
        let names: Vec<_> = list.iter().map(|(_, s, _)| s.as_str()).collect();
        assert_eq!(names, ["c2", "c3"], "oldest first");
        // From the very first commit: rebase from the root.
        assert_eq!(rebase_range(&repo(), c1).unwrap().0, None);

        // A commit outside the current branch is refused.
        git(&dir, &["switch", "-qc", "other", "HEAD~2"]);
        let other = oid(commit("o"));
        git(&dir, &["switch", "-q", "main"]);
        assert!(rebase_range(&repo(), other).is_err());
        // A range containing a merge is refused.
        git(&dir, &["merge", "-q", "--no-edit", "other"]);
        assert!(rebase_range(&repo(), c2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
