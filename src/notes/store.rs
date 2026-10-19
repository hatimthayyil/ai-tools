use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use gix::{ObjectId, objs::tree::EntryKind};

use crate::{
    file::{self, AtPath, pretty},
    notes::{
        leaf::{Leaf, parse_name},
        summary::Summary,
        topic::{Topic, is_slug},
    },
};

pub const ME: &str = "ai notes";
pub const SUMMARY: &str = "_summary.md";
const GIT: &str = ".git";
const LOCK: &str = "ai-notes.lock";
const SUMMARISE_LOCK: &str = "ai-notes-summarise.lock";

/// A topic as read from disk: its summary, its notes and its subtopics, in name order.
#[derive(Debug, Default)]
pub struct Node {
    pub topic: Topic,
    pub summary: Option<Summary>,
    pub leaves: Vec<Leaf>,
    pub kids: Vec<Node>,
}

impl Node {
    pub fn find(&self, t: &Topic) -> Option<&Node> {
        if self.topic == *t {
            return Some(self);
        }
        let k = self.kids.iter().find(|k| t.within(&k.topic))?;
        k.find(t)
    }

    /// Every node, parents before children.
    pub fn walk<'a>(&'a self, each: &mut impl FnMut(&'a Node)) {
        each(self);
        for k in &self.kids {
            k.walk(each);
        }
    }

    pub fn leaf(&self, id: &str) -> Option<&Leaf> {
        let mut found = None;
        self.walk(&mut |n| found = found.or_else(|| n.leaves.iter().find(|l| l.id == id)));
        found
    }

    pub fn ids(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        self.walk(&mut |n| ids.extend(n.leaves.iter().map(|l| l.id.clone())));
        ids
    }

    /// Valid notes in this subtree.
    pub fn count(&self) -> usize {
        let mut n = 0;
        self.walk(&mut |k| n += k.leaves.iter().filter(|l| l.is_valid()).count());
        n
    }
}

/// The notes: a git repository of Markdown files, one commit per change.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: &Path) -> Store {
        Store {
            dir: dir.to_path_buf(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn git_dir(&self) -> PathBuf {
        self.dir.join(GIT)
    }

    pub fn exists(&self) -> bool {
        self.git_dir().is_dir()
    }

    pub fn path(&self, rel: &Path) -> PathBuf {
        self.dir.join(rel)
    }

    /// The tree as on disk now; empty before the first note.
    pub fn load(&self) -> Result<Node> {
        if !self.dir.is_dir() {
            return Ok(Node::default());
        }
        self.load_dir(Topic::default())
    }

    fn load_dir(&self, topic: Topic) -> Result<Node> {
        let dir = self.path(&topic.rel());
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .at(&dir)?
            .collect::<Result<_, _>>()
            .at(&dir)?;
        entries.sort_by_key(|e| e.file_name());
        let mut node = Node {
            topic: topic.clone(),
            ..Node::default()
        };
        for e in entries {
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let kind = e.file_type().at(&e.path())?;
            if kind.is_dir() && is_slug(&name) {
                node.kids.push(self.load_dir(topic.child(&name))?);
            } else if kind.is_file() && name == SUMMARY && !topic.is_root() {
                let text = fs::read_to_string(e.path()).at(&e.path())?;
                let s = Summary::parse(&text).with_context(|| pretty(&e.path()))?;
                node.summary = Some(s);
            } else if kind.is_file() && parse_name(&name).is_some() {
                let text = fs::read_to_string(e.path()).at(&e.path())?;
                let leaf = Leaf::parse(&topic, &name, &text).with_context(|| pretty(&e.path()))?;
                node.leaves.push(leaf);
            }
        }
        Ok(node)
    }

    /// Holds the write lock while `change` edits the files of the tree it is given, then commits
    /// every file as one commit `msg`. The first change creates the repository.
    pub fn mutate<R>(&self, msg: &str, change: impl FnOnce(&Node) -> Result<R>) -> Result<R> {
        self.create()?;
        let _lock = file::lock(&self.git_dir().join(LOCK))?;
        let r = change(&self.load()?)?;
        self.commit(msg)?;
        Ok(r)
    }

    /// The summariser lock, unless another summariser holds it.
    pub fn summarise_lock(&self) -> Result<Option<file::Lock>> {
        file::try_lock(&self.git_dir().join(SUMMARISE_LOCK))
    }

    fn create(&self) -> Result<()> {
        if self.exists() {
            return Ok(());
        }
        if fs::read_dir(&self.dir).is_ok_and(|mut d| d.next().is_some()) && !self.exists() {
            bail!(
                "{} holds files but no notes. Point AI_NOTES_DIR elsewhere.",
                pretty(&self.dir)
            );
        }
        fs::create_dir_all(&self.dir).at(&self.dir)?;
        let up = self.dir.parent().filter(|p| !p.as_os_str().is_empty());
        let tmp = tempfile::Builder::new()
            .prefix(".ai-notes-init")
            .tempdir_in(up.unwrap_or(Path::new(".")))
            .context("Cannot create a temporary directory.")?;
        gix::init(tmp.path())?;
        match fs::rename(tmp.path().join(GIT), self.git_dir()) {
            Err(_) if self.exists() => Ok(()),
            r => r.at(&self.git_dir()),
        }
    }

    /// Commits every file of the work tree, then makes the index match; nothing if unchanged.
    fn commit(&self, msg: &str) -> Result<()> {
        let mut repo = gix::open(&self.dir)?;
        repo.committer_or_set_fallback(ME, "ai@localhost")?;
        let mut ed = repo.edit_tree(ObjectId::empty_tree(repo.object_hash()))?;
        for (rel, abs) in files(&self.dir)? {
            let id = repo.write_blob(fs::read(&abs).at(&abs)?)?;
            ed.upsert(&rel, EntryKind::Blob, id)?;
        }
        let tree = ed.write()?.detach();
        let parent = repo.head()?.id().map(|id| id.detach());
        if let Some(p) = parent
            && repo.find_commit(p)?.tree_id()? == tree
        {
            return Ok(());
        }
        let me = repo.committer().context("No committer.")??;
        repo.commit_as(me, me, "HEAD", msg, tree, parent)?;
        repo.index_from_tree(&tree)?.write(Default::default())?;
        Ok(())
    }

    pub fn write(&self, rel: &Path, text: &str) -> Result<()> {
        let p = self.path(rel);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).at(d)?;
        }
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, text).at(&tmp)?;
        fs::rename(&tmp, &p).at(&p)
    }

    /// Writes `l` under the name its title gives it, removing its old file if that differs.
    pub fn put_leaf(&self, l: &Leaf) -> Result<()> {
        let rel = l.topic.rel().join(l.file_name());
        self.write(&rel, &l.render())?;
        if !l.name.is_empty() && rel != l.rel() && self.path(&l.rel()).exists() {
            self.remove(&l.rel())?;
        }
        Ok(())
    }

    pub fn remove(&self, rel: &Path) -> Result<()> {
        let p = self.path(rel);
        fs::remove_file(&p).at(&p)
    }

    pub fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let (a, b) = (self.path(from), self.path(to));
        if let Some(d) = b.parent() {
            fs::create_dir_all(d).at(d)?;
        }
        fs::rename(&a, &b).at(&a)
    }

    /// Removes `t` and each topic above it that holds nothing but its summary.
    pub fn prune(&self, t: &Topic) -> Result<()> {
        let mut t = t.clone();
        while !t.is_root() {
            let dir = self.path(&t.rel());
            let rest = fs::read_dir(&dir)
                .at(&dir)?
                .filter_map(Result::ok)
                .any(|e| e.file_name() != SUMMARY);
            if rest {
                break;
            }
            fs::remove_dir_all(&dir).at(&dir)?;
            t = t.parent().expect("not the root");
        }
        Ok(())
    }
}

/// Every file under `dir` but `.git`, by its path relative to `dir`.
fn files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut todo = vec![(String::new(), dir.to_path_buf())];
    while let Some((rel, d)) = todo.pop() {
        for e in fs::read_dir(&d).at(&d)? {
            let e = e.at(&d)?;
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            if rel.is_empty() && name == GIT {
                continue;
            }
            let r = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let kind = e.file_type().at(&e.path())?;
            if kind.is_dir() {
                todo.push((r, e.path()));
            } else if kind.is_file() {
                out.push((r, e.path()));
            }
        }
    }
    Ok(out)
}
