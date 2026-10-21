use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, bail};
use gix::{
    ObjectId,
    objs::{
        WriteTo,
        tree::{Entry, EntryKind},
    },
};
use ignore::{WalkBuilder, gitignore::GitignoreBuilder};

use crate::{
    file::{AtPath, pretty},
    git::hash,
};

/// A text file: its path from the root, its size in bytes and its blob id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub size: u64,
    pub hash: String,
}

/// A directory holding at least one text file: its path from the root (`""` for the root), its
/// children by name, and the tree id over their names and ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dir {
    pub path: String,
    pub hash: String,
    pub kids: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    File(File),
    Dir(Dir),
}

impl Node {
    pub fn path(&self) -> &str {
        match self {
            Node::File(f) => &f.path,
            Node::Dir(d) => &d.path,
        }
    }

    pub fn name(&self) -> &str {
        let p = self.path();
        p.rsplit_once('/').map_or(p, |(_, n)| n)
    }

    pub fn hash(&self) -> &str {
        match self {
            Node::File(f) => &f.hash,
            Node::Dir(d) => &d.hash,
        }
    }
}

/// The text files under `root` and the directories holding them, leaving out what git ignores,
/// what `ignores` (gitignore lines, relative to `root`) ignores, and `.git`.
pub fn walk(root: &Path, ignores: &[String]) -> Result<Dir> {
    if !root.is_dir() {
        bail!("{}: not a directory.", pretty(root));
    }
    let mut extra = GitignoreBuilder::new(root);
    for line in ignores {
        extra
            .add_line(None, line)
            .with_context(|| format!("Bad ignore pattern {line:?}."))?;
    }
    let extra = extra.build()?;
    let walker = WalkBuilder::new(root)
        .standard_filters(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .parents(true)
        .filter_entry(move |e| {
            let dir = e.file_type().is_some_and(|t| t.is_dir());
            e.file_name() != ".git" && !extra.matched(e.path(), dir).is_ignore()
        })
        .build();
    let mut top = BTreeMap::new();
    for e in walker {
        let e = e?;
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = e.path().strip_prefix(root)?;
        let Some(names) = rel.iter().map(|n| n.to_str()).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let bytes = std::fs::read(e.path()).at(e.path())?;
        if memchr::memchr(0, &bytes).is_some() {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let file = File {
            path: names.join("/"),
            size: text.len() as u64,
            hash: hash(&text)?,
        };
        let (name, dirs) = names.split_last().expect("a walked file is below the root");
        let mut at = &mut top;
        for d in dirs {
            let Draft::Dir(kids) = at
                .entry(d.to_string())
                .or_insert_with(|| Draft::Dir(BTreeMap::new()))
            else {
                unreachable!("a file and a directory share a path");
            };
            at = kids;
        }
        at.insert(name.to_string(), Draft::File(file));
    }
    dir(String::new(), top)
}

enum Draft {
    File(File),
    Dir(BTreeMap<String, Draft>),
}

fn dir(path: String, drafts: BTreeMap<String, Draft>) -> Result<Dir> {
    let mut kids = Vec::with_capacity(drafts.len());
    for (name, d) in drafts {
        kids.push(match d {
            Draft::File(f) => Node::File(f),
            Draft::Dir(sub) => {
                let p = if path.is_empty() {
                    name
                } else {
                    format!("{path}/{name}")
                };
                Node::Dir(dir(p, sub)?)
            }
        });
    }
    Ok(Dir {
        hash: tree_hash(&kids)?,
        path,
        kids,
    })
}

/// The sha1 tree id git would give a tree holding `kids`.
fn tree_hash(kids: &[Node]) -> Result<String> {
    let mut tree = gix::objs::Tree::empty();
    for k in kids {
        let kind = match k {
            Node::File(_) => EntryKind::Blob,
            Node::Dir(_) => EntryKind::Tree,
        };
        tree.entries.push(Entry {
            mode: kind.into(),
            filename: k.name().into(),
            oid: ObjectId::from_hex(k.hash().as_bytes())?,
        });
    }
    tree.entries.sort();
    let mut bytes = Vec::new();
    tree.write_to(&mut bytes)?;
    let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Tree, &bytes)?;
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    fn put(root: &Path, path: &str, bytes: &[u8]) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Every path in the tree, depth first.
    fn paths(d: &Dir) -> Vec<String> {
        let mut out = Vec::new();
        for k in &d.kids {
            out.push(k.path().to_string());
            if let Node::Dir(sub) = k {
                out.extend(paths(sub));
            }
        }
        out
    }

    #[test]
    fn hashes_are_the_ids_git_gives() {
        let t = tempfile::tempdir().unwrap();
        put(t.path(), "b.md", b"bee\n");
        put(t.path(), "a/x.rs", b"fn x() {}\n");
        put(t.path(), "a-b.txt", b"dash\n");
        put(t.path(), "a/deep/y.txt", b"why\n");
        let d = walk(t.path(), &[]).unwrap();
        git(t.path(), &["init", "-q"]);
        git(t.path(), &["add", "."]);
        assert_eq!(d.hash, git(t.path(), &["write-tree"]));
        let Node::File(f) = &d.kids[2] else {
            panic!("{d:?}")
        };
        assert_eq!(
            (f.path.as_str(), f.size),
            ("b.md", 4),
            "children are in name order"
        );
        assert_eq!(f.hash, git(t.path(), &["hash-object", "b.md"]));
        assert_eq!(
            paths(&d),
            ["a", "a/deep", "a/deep/y.txt", "a/x.rs", "a-b.txt", "b.md"]
        );
    }

    #[test]
    fn ignored_and_binary_files_are_left_out_with_the_directories_they_empty() {
        let t = tempfile::tempdir().unwrap();
        put(t.path(), ".gitignore", b"target/\n*.log\n");
        put(t.path(), "src/main.rs", b"fn main() {}\n");
        put(t.path(), "src/run.log", b"noise\n");
        put(t.path(), "target/debug/out.rs", b"built\n");
        put(t.path(), "img/logo.png", b"\x89PNG\r\n\x1a\n\0\0");
        put(t.path(), "img/bad.txt", b"\xff\xfe");
        put(t.path(), "docs/draft.md", b"draft\n");
        put(t.path(), "docs/keep.md", b"keep\n");
        put(t.path(), ".git/HEAD", b"ref: refs/heads/main\n");
        let d = walk(t.path(), &["draft.md".into(), "/notes/".into()]).unwrap();
        assert_eq!(
            paths(&d),
            [".gitignore", "docs", "docs/keep.md", "src", "src/main.rs"],
            "no repository needed"
        );
    }

    #[test]
    fn a_change_below_changes_every_hash_above_it_and_no_other() {
        let t = tempfile::tempdir().unwrap();
        put(t.path(), "a/b/c.txt", b"one\n");
        put(t.path(), "z/d.txt", b"two\n");
        let before = walk(t.path(), &[]).unwrap();
        put(t.path(), "a/b/c.txt", b"three\n");
        let after = walk(t.path(), &[]).unwrap();
        let hashes = |d: &Dir| {
            let mut h = vec![d.hash.clone()];
            fn go(d: &Dir, h: &mut Vec<String>) {
                for k in &d.kids {
                    h.push(k.hash().to_string());
                    if let Node::Dir(sub) = k {
                        go(sub, h);
                    }
                }
            }
            go(d, &mut h);
            h
        };
        let changed: Vec<bool> = hashes(&before)
            .iter()
            .zip(hashes(&after))
            .map(|(a, b)| *a != b)
            .collect();
        assert_eq!(paths(&after), ["a", "a/b", "a/b/c.txt", "z", "z/d.txt"]);
        assert_eq!(changed, [true, true, true, true, false, false]);
    }

    #[test]
    fn a_subdirectory_of_a_repository_keeps_the_repository_s_ignores() {
        let t = tempfile::tempdir().unwrap();
        git(t.path(), &["init", "-q"]);
        put(t.path(), ".gitignore", b"*.tmp\n");
        put(t.path(), ".git/info/exclude", b"secret.md\n");
        put(t.path(), "sub/a.md", b"a\n");
        put(t.path(), "sub/b.tmp", b"b\n");
        put(t.path(), "sub/secret.md", b"s\n");
        let d = walk(&t.path().join("sub"), &[]).unwrap();
        assert_eq!(paths(&d), ["a.md"]);
    }

    #[test]
    fn a_bad_ignore_pattern_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let e = walk(t.path(), &["[z-a]".into()]).unwrap_err();
        assert!(e.to_string().starts_with("Bad ignore pattern"), "{e}");
    }
}
