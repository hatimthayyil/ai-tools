use std::path::Path;

use crate::memory::prov::repo_root;

/// A project: the git repository at or above a directory, and its id, the repository's root
/// commit.
pub struct Project {
    pub repo: gix::Repository,
    pub id: String,
}

impl Project {
    /// The project at or above `dir`; `None` outside a repository or before its first commit.
    pub fn find(dir: &Path) -> Option<Project> {
        let repo = gix::discover(dir).ok()?;
        let id = repo_root(&repo)?;
        Some(Project { repo, id })
    }
}
