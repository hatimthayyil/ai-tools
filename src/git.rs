use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    rc::Rc,
};

use anyhow::{Context, Result, bail};
use gix::{
    ObjectId,
    objs::tree::EntryKind,
    refs::{
        Target,
        transaction::{PreviousValue, RefEdit},
    },
};

/// The blob at a store's tree root naming its format.
pub const FORMAT: &str = "format";

/// The sha1 blob id of `input`: the id git gives a file holding it.
pub fn hash(input: &str) -> Result<String> {
    let id = gix::objs::compute_hash(
        gix::hash::Kind::Sha1,
        gix::objs::Kind::Blob,
        input.as_bytes(),
    )?;
    Ok(id.to_string())
}

/// Entry `n` under `dir`, fanned out 256 per tree.
pub fn fan_path(dir: &str, n: u64) -> String {
    format!("{dir}/{:04x}/{:02x}", n >> 8, n & 255)
}

/// New blobs by path, and paths removed with all under them; removals apply first.
#[derive(Debug, Default)]
pub struct Changes {
    put: BTreeMap<String, Vec<u8>>,
    gone: BTreeSet<String>,
}

impl Changes {
    pub fn put(&mut self, path: String, bytes: Vec<u8>) {
        self.put.insert(path, bytes);
    }

    pub fn remove(&mut self, path: String) {
        self.gone.insert(path);
    }

    pub fn is_empty(&self) -> bool {
        self.put.is_empty() && self.gone.is_empty()
    }
}

/// A store under one git ref: a tree of blobs, one commit per mutation, compare-and-swap on
/// the ref. `what` names the store in messages; `format` is the format this ai writes.
pub struct Ref {
    repo: gix::Repository,
    name: String,
    what: &'static str,
    format: u32,
}

impl Ref {
    pub fn new(repo: gix::Repository, name: String, what: &'static str, format: u32) -> Ref {
        Ref {
            repo,
            name,
            what,
            format,
        }
    }

    pub fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn head(&self) -> Result<Option<ObjectId>> {
        Ok(self
            .repo
            .try_find_reference(self.name.as_str())?
            .and_then(|r| r.try_id().map(|id| id.detach())))
    }

    /// The tree of `commit`, of any format; `None` is the empty store.
    pub fn raw(&self, commit: Option<ObjectId>) -> Result<Snap<'_>> {
        let tree = match commit {
            Some(c) => self.repo.find_commit(c)?.tree_id()?.detach(),
            None => ObjectId::empty_tree(self.repo.object_hash()),
        };
        Ok(self.tree(commit, tree))
    }

    /// The tree `tree`, read as if `commit` held it.
    pub fn tree(&self, commit: Option<ObjectId>, tree: ObjectId) -> Snap<'_> {
        Snap {
            git: self,
            commit,
            tree,
            cache: RefCell::default(),
        }
    }

    /// The format a `format` blob names; none is format 1.
    pub fn parse_format(&self, b: &[u8]) -> Result<u32> {
        if b.is_empty() {
            return Ok(1);
        }
        match std::str::from_utf8(b)
            .ok()
            .and_then(|s| s.trim_end().parse().ok())
        {
            Some(v) if v > 0 => Ok(v),
            _ => bail!("This {}'s format blob is corrupt.", self.what),
        }
    }

    /// Refuses a format newer than this ai's.
    pub fn readable(&self, v: u32) -> Result<()> {
        if v > self.format {
            bail!(
                "This {} is format {v}; this ai reads up to {}. Upgrade ai.",
                self.what,
                self.format
            );
        }
        Ok(())
    }

    /// Applies `change` to the store `at` gives for the head as one commit, redoing it until
    /// the ref update wins; returns its result and the store after it.
    pub fn mutate<'r, S: AsRef<Snap<'r>>, R>(
        &'r self,
        msg: &str,
        at: impl Fn(Option<ObjectId>) -> Result<S>,
        mut change: impl FnMut(&S) -> Result<(R, Changes)>,
    ) -> Result<(R, S)> {
        loop {
            let base = at(self.head()?)?;
            let (r, ch) = change(&base)?;
            if ch.is_empty() {
                return Ok((r, base));
            }
            let b = base.as_ref();
            let commit = self.commit(b, ch, msg)?;
            if self.cas(b.commit, commit, msg)? {
                return Ok((r, at(Some(commit))?));
            }
        }
    }

    /// `ch` over `base`, the first commit naming the format.
    fn commit(&self, base: &Snap, mut ch: Changes, msg: &str) -> Result<ObjectId> {
        if base.commit.is_none() {
            ch.put(FORMAT.into(), format_blob(self.format));
        }
        let tree = self.write_tree(base.tree, ch)?;
        self.new_commit(msg, tree, base.commit)
    }

    pub fn write_tree(&self, base: ObjectId, Changes { put, gone }: Changes) -> Result<ObjectId> {
        let mut ed = self.repo.edit_tree(base)?;
        for p in gone {
            ed.remove(&p)?;
        }
        for (p, bytes) in put {
            let id = self.repo.write_blob(bytes)?;
            ed.upsert(&p, EntryKind::Blob, id)?;
        }
        Ok(ed.write()?.detach())
    }

    pub fn new_commit(
        &self,
        msg: &str,
        tree: ObjectId,
        parent: Option<ObjectId>,
    ) -> Result<ObjectId> {
        let me = self.repo.committer().context("No committer.")??;
        Ok(self.repo.new_commit_as(me, me, msg, tree, parent)?.id)
    }

    /// Moves the ref from `from` to `to`; false if another writer moved it first.
    pub fn cas(&self, from: Option<ObjectId>, to: ObjectId, msg: &str) -> Result<bool> {
        let expected = match from {
            Some(c) => PreviousValue::MustExistAndMatch(Target::Object(c)),
            None => PreviousValue::MustNotExist,
        };
        let edit = RefEdit::update(self.name.as_str().try_into()?, to, expected, msg);
        match self.repo.edit_reference(edit) {
            Ok(_) => Ok(true),
            Err(_) if self.head()? != from => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// The `format` blob of format `v`.
pub fn format_blob(v: u32) -> Vec<u8> {
    format!("{v}\n").into_bytes()
}

/// A store as of one commit: every read through it sees the same state.
pub struct Snap<'r> {
    git: &'r Ref,
    commit: Option<ObjectId>,
    tree: ObjectId,
    cache: RefCell<HashMap<String, Rc<[u8]>>>,
}

impl<'r> AsRef<Snap<'r>> for Snap<'r> {
    fn as_ref(&self) -> &Snap<'r> {
        self
    }
}

impl Snap<'_> {
    pub fn tree_id(&self) -> ObjectId {
        self.tree
    }

    /// The format its `format` blob names; the empty store is of this ai's format.
    pub fn format(&self) -> Result<u32> {
        match self.commit {
            Some(_) => self.git.parse_format(&self.read(FORMAT)?),
            None => Ok(self.git.format),
        }
    }

    pub fn entry(&self, path: &str) -> Result<Option<ObjectId>> {
        let root = self.git.repo.find_tree(self.tree)?;
        Ok(root.lookup_entry_by_path(path)?.map(|e| e.object_id()))
    }

    pub fn fetch(&self, id: ObjectId) -> Result<Vec<u8>> {
        Ok(self.git.repo.find_blob(id)?.take_data())
    }

    /// The blob at `path`, empty if absent; cached.
    pub fn read(&self, path: &str) -> Result<Rc<[u8]>> {
        if let Some(b) = self.cache.borrow().get(path) {
            return Ok(b.clone());
        }
        let b: Rc<[u8]> = match self.entry(path)? {
            Some(id) => self.fetch(id)?.into(),
            None => Rc::from([]),
        };
        self.cache.borrow_mut().insert(path.into(), b.clone());
        Ok(b)
    }

    fn named(&self, tree: ObjectId) -> Result<Vec<(String, ObjectId)>> {
        let t = self.git.repo.find_tree(tree)?;
        Ok(t.decode()?
            .entries
            .iter()
            .map(|e| (e.filename.to_string(), e.oid.to_owned()))
            .collect())
    }

    /// A tree's entries by their hex names, in order.
    fn numbered(&self, tree: ObjectId) -> Result<Vec<(u64, ObjectId)>> {
        self.named(tree)?
            .into_iter()
            .map(|(name, id)| {
                let n = u64::from_str_radix(&name, 16)
                    .with_context(|| format!("An entry is misnamed: {name}."))?;
                Ok((n, id))
            })
            .collect()
    }

    /// Every blob under the fanned-out `dir`, with its number.
    pub fn walk(&self, dir: &str, mut each: impl FnMut(u64, ObjectId) -> Result<()>) -> Result<()> {
        let Some(top) = self.entry(dir)? else {
            return Ok(());
        };
        for (hi, sub) in self.numbered(top)? {
            for (lo, id) in self.numbered(sub)? {
                each(hi << 8 | lo, id)?;
            }
        }
        Ok(())
    }

    /// One past the highest number under the fanned-out `dir`; 0 if empty.
    pub fn count(&self, dir: &str) -> Result<u64> {
        let Some(top) = self.entry(dir)? else {
            return Ok(0);
        };
        let Some(&(hi, sub)) = self.numbered(top)?.last() else {
            return Ok(0);
        };
        let Some(&(lo, _)) = self.numbered(sub)?.last() else {
            return Ok(0);
        };
        Ok((hi << 8 | lo) + 1)
    }
}
