//! Git status for the files sidebar, from `git status --porcelain` run on a
//! worker thread. No git, or no repository, just means no markings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A file's state; the order is the priority a folder shows for its contents.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Mark {
    Ignored,
    /// Untracked.
    New,
    /// Changes staged (modified or deleted in the index).
    Staged,
    /// New file, staged.
    Added,
    Renamed,
    /// Changed in the working tree, not staged.
    Modified,
    /// Staged, then changed again.
    StagedModified,
    Conflict,
}

impl Mark {
    /// Badge letter, CSS class and tooltip.
    pub fn badge(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Mark::Ignored => ("", "git-ignored", "Ignored by git"),
            Mark::New => ("N", "git-new", "New: not tracked by git yet"),
            Mark::Staged => ("S", "git-staged", "Changes staged"),
            Mark::Added => ("A", "git-staged", "New file, staged"),
            Mark::Renamed => ("R", "git-staged", "Renamed, staged"),
            Mark::Modified => ("M", "git-modified", "Modified, not staged"),
            Mark::StagedModified => ("M", "git-modified", "Staged, then modified again"),
            Mark::Conflict => ("!", "git-conflict", "Merge conflict"),
        }
    }
}

#[derive(Default, Debug)]
pub struct Status {
    /// The work tree and git directory (whose index and HEAD are watched).
    pub top: PathBuf,
    pub git_dir: PathBuf,
    files: HashMap<PathBuf, Mark>,
    /// Untracked or ignored folders, reported whole.
    whole: Vec<(PathBuf, Mark)>,
    /// The highest-priority change inside each folder.
    dirs: HashMap<PathBuf, Mark>,
}

/// Status of the repository containing `dir`, or `None` outside one.
pub fn status(dir: &Path) -> Option<Status> {
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
        out.status.success().then_some(out.stdout)
    };
    let rev = String::from_utf8(git(&["rev-parse", "--show-toplevel", "--absolute-git-dir"])?).ok()?;
    let mut lines = rev.lines();
    let (top, git_dir) = (PathBuf::from(lines.next()?), PathBuf::from(lines.next()?));
    let out = git(&["status", "--porcelain=v1", "-z", "--ignored"])?;
    Some(parse(top, git_dir, &out))
}

/// Parse `git status --porcelain=v1 -z --ignored` output. Paths in it are
/// relative to the work tree `top`.
pub fn parse(top: PathBuf, git_dir: PathBuf, out: &[u8]) -> Status {
    let mut st = Status { top, git_dir, ..Default::default() };
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        if f.len() < 4 {
            continue;
        }
        let (x, y) = (f[0], f[1]);
        let rel = String::from_utf8_lossy(&f[3..]);
        let mark = match (x, y) {
            (b'?', b'?') => Mark::New,
            (b'!', b'!') => Mark::Ignored,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Mark::Conflict,
            (b' ', b'M' | b'D' | b'T') => Mark::Modified,
            (_, b'M' | b'D' | b'T') => Mark::StagedModified,
            (b'A', _) => Mark::Added,
            (b'R' | b'C', _) => Mark::Renamed,
            _ => Mark::Staged,
        };
        if matches!(x, b'R' | b'C') {
            // The original path follows as its own field.
            fields.next();
        }
        let path = st.top.join(rel.trim_end_matches('/'));
        if rel.ends_with('/') {
            st.whole.push((path.clone(), mark));
        } else {
            st.files.insert(path.clone(), mark);
        }
        if mark != Mark::Ignored {
            for d in path.ancestors().skip(1) {
                if !d.starts_with(&st.top) {
                    break;
                }
                let e = st.dirs.entry(d.to_path_buf()).or_insert(mark);
                *e = (*e).max(mark);
            }
        }
    }
    st
}

impl Status {
    /// A file's mark; for a folder, the mark of the folder itself
    /// (untracked or ignored) or else of the most important change inside.
    pub fn mark(&self, p: &Path, dir: bool) -> Option<Mark> {
        if let Some(&m) = self.files.get(p) {
            return Some(m);
        }
        if let Some(&(_, m)) = self.whole.iter().find(|(w, _)| p.starts_with(w)) {
            return Some(m);
        }
        if dir { self.dirs.get(p).copied() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain() {
        let out = b" M docs/a.md\0M  b.md\0MM c.md\0A  new.md\0R  to.md\0f\0?? notes/\0?? x.md\0!! target/\0UU d.md\0";
        let st = parse(PathBuf::from("/r"), PathBuf::from("/r/.git"), out);
        let m = |p: &str, dir| st.mark(Path::new(p), dir);
        assert_eq!(m("/r/docs/a.md", false), Some(Mark::Modified));
        assert_eq!(m("/r/b.md", false), Some(Mark::Staged));
        assert_eq!(m("/r/c.md", false), Some(Mark::StagedModified));
        assert_eq!(m("/r/new.md", false), Some(Mark::Added));
        assert_eq!(m("/r/to.md", false), Some(Mark::Renamed));
        assert_eq!(m("/r/f", false), None);
        assert_eq!(m("/r/notes", true), Some(Mark::New));
        assert_eq!(m("/r/notes/deep/y.md", false), Some(Mark::New));
        assert_eq!(m("/r/x.md", false), Some(Mark::New));
        assert_eq!(m("/r/target", true), Some(Mark::Ignored));
        assert_eq!(m("/r/target/debug/z.md", false), Some(Mark::Ignored));
        assert_eq!(m("/r/d.md", false), Some(Mark::Conflict));
        assert_eq!(m("/r/docs", true), Some(Mark::Modified));
        assert_eq!(m("/r/clean.md", false), None);
        // The root folder shows the most important change: the conflict.
        assert_eq!(m("/r", true), Some(Mark::Conflict));
    }
}
