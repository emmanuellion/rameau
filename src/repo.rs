use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use git2::{BranchType, Delta, Diff, DiffOptions, Oid, Repository, RepositoryState, Sort, Status, StatusOptions};

use crate::graph::{self, Row};

// shortcut: history is capped to keep startup instant; load lazily on scroll when big repos matter.
const MAX_COMMITS: usize = 50_000;

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
    /// (ahead, behind) of the HEAD branch vs its upstream; None when it has no upstream.
    pub ahead_behind: Option<(usize, usize)>,
    pub state: RepositoryState,
    pub changes: Changes,
    pub commits: Vec<Commit>,
    pub rows: Vec<Row>,
    pub branches: Vec<Branch>,
    pub max_lanes: usize,
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

pub fn load(repo: &mut Repository) -> Result<History, git2::Error> {
    let mut stashes = Vec::new();
    repo.stash_foreach(|index, message, id| {
        stashes.push(Stash { index, message: message.to_owned(), id: *id });
        true
    })?;
    let repo = &*repo;
    let mut labels: HashMap<Oid, Vec<RefLabel>> = HashMap::new();
    let mut branches = Vec::new();
    let head = repo.head().ok();
    let head_name = head
        .as_ref()
        .and_then(|h| h.shorthand().map(str::to_owned))
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
    let color_names: HashMap<Oid, String> = labels
        .iter()
        .filter_map(|(id, ls)| ls.iter().min_by_key(|l| l.kind as u8).map(|l| (*id, key(&l.name, l.kind))))
        .collect();
    // FNV-1a, high bits: names differing by one letter land on unrelated colors.
    let hash = |bytes: &[u8]| (bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3)) >> 32) as usize;
    let color_of = |id: &Oid| color_names.get(id).map_or_else(|| hash(id.as_bytes()), |n| hash(n.as_bytes()));

    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    for (_, _, target) in &branches {
        walk.push(*target)?;
    }
    if head.is_some() {
        walk.push_head()?;
    }

    let mut commits = Vec::new();
    let mut graph_input = Vec::new();
    for id in walk.take(MAX_COMMITS) {
        let c = repo.find_commit(id?)?;
        graph_input.push((c.id(), c.parent_ids().collect::<Vec<_>>()));
        commits.push(Commit {
            id: c.id(),
            summary: c.summary().unwrap_or("").to_owned(),
            author: c.author().name().unwrap_or("").to_owned(),
            time: c.time().seconds(),
            refs: labels.remove(&c.id()).unwrap_or_default(),
        });
    }
    let rows = graph::layout(&graph_input, color_of);
    let max_lanes = rows.iter().flat_map(|r| r.segments.iter().map(|s| s.from.max(s.to)).chain([r.lane])).max().unwrap_or(0) + 1;

    let index: HashMap<Oid, usize> = commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
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
    Ok(History {
        name,
        stashes,
        head: head_name,
        head_branch,
        ahead_behind,
        state: repo.state(),
        changes: changes(repo)?,
        commits,
        rows,
        branches,
        max_lanes,
    })
}

/// Files changed in the working copy, split like `git status`.
#[derive(Default)]
pub struct Changes {
    pub staged: Vec<(char, String)>,
    pub unstaged: Vec<(char, String)>,
    pub conflicted: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.conflicted.is_empty()
    }
}

fn changes(repo: &Repository) -> Result<Changes, git2::Error> {
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
    Ok(out)
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
}

pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
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
    let partial = diff.deltas().len() == 1 && diff.deltas().all(|d| d.status() == Delta::Modified);
    let mut lines = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_, hunk, l| {
        let kind = match l.origin() {
            ' ' => LineKind::Context,
            '+' => LineKind::Added,
            '-' => LineKind::Removed,
            'H' => LineKind::Hunk,
            'B' => LineKind::Info,
            _ => return true, // file headers and "no newline" markers
        };
        // Raw text (tabs, \r kept): patches built from these lines must match the file exactly.
        let text = String::from_utf8_lossy(l.content()).trim_end_matches('\n').to_owned();
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

/// Full (old, new) contents of the file on both sides of the diff, for syntax highlighting.
pub fn file_versions(repo: &Repository, source: &DiffSource, path: &str) -> (Option<String>, Option<String>) {
    let p = std::path::Path::new(path);
    let blob = |tree: Option<git2::Tree>| -> Option<String> {
        let entry = tree?.get_path(p).ok()?;
        let blob = repo.find_blob(entry.id()).ok()?;
        (!blob.is_binary()).then(|| String::from_utf8_lossy(blob.content()).into_owned())
    };
    let index = || -> Option<String> {
        let entry = repo.index().ok()?.get_path(p, 0)?;
        let blob = repo.find_blob(entry.id).ok()?;
        (!blob.is_binary()).then(|| String::from_utf8_lossy(blob.content()).into_owned())
    };
    let head = || repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    match source {
        DiffSource::Commit(id) => {
            let c = repo.find_commit(*id).ok();
            let parent = c.as_ref().and_then(|c| c.parent(0).ok()).and_then(|p| p.tree().ok());
            (blob(parent), blob(c.and_then(|c| c.tree().ok())))
        }
        DiffSource::Staged => (blob(head()), index()),
        DiffSource::Unstaged => {
            let work = repo.workdir().and_then(|w| std::fs::read(w.join(p)).ok());
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

fn commit_diff<'r>(repo: &'r Repository, id: Oid, opts: &mut DiffOptions) -> Result<Diff<'r>, git2::Error> {
    let c = repo.find_commit(id)?;
    let parent_tree = c.parent(0).ok().map(|p| p.tree()).transpose()?;
    repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&c.tree()?), Some(opts))
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
