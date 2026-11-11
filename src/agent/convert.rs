//! Moving a project's agents and private settings from where an older ai kept them, `.ai/local`
//! in the project's home (agent store format 4), to the project's folder in `~/ai`. Each file kept
//! as a directory of segments becomes one file of the same bytes. A thread's copies of its
//! sessions' files become records of the harness's own files; a copy the harness no longer holds
//! whole is restored into the harness's files, where its records point. The old store is archived
//! whole in `~/.ai` and never enters `~/ai`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, FileType},
    io::{self, IsTerminal},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use dialoguer::Confirm;
use jiff::Timestamp;
use serde::Deserialize;

use crate::{
    agent::{
        record::{Event, Pointer, State, Track},
        source,
        store::{
            self, AGENTS, ATTACHMENTS, DIR, FORMAT, ITEMS, LINES, SESSIONS, SOURCES, THREADS,
            check_name,
        },
    },
    backup,
    config::{FILE, Homes, Settings},
    file::{self, AtPath, pretty, sha256},
    jsonl,
    project::{OLD_STORE, Project},
    repo,
};

/// The agent store format a move takes.
const FROM: u32 = 4;

/// Where this machine keeps the stores a move replaced, in `~/.ai`.
const ARCHIVE: &str = "archive";

/// The store an older ai kept for `project`, if it has one.
fn old(project: &Project) -> Option<PathBuf> {
    let p = project.home.join(OLD_STORE);
    p.exists().then_some(p)
}

/// Refuses the agents of `project` while an older ai's store of them awaits its move.
pub(super) fn refuse_pending(project: &Project, homes: &Homes) -> Result<()> {
    match old(project) {
        Some(p) => bail!(
            "This project's agents are still in {}, where an older ai kept them. Run an ai agent \
             command on a terminal to move them to {}.",
            pretty(&p),
            pretty(&homes.repo)
        ),
        None => Ok(()),
    }
}

/// On a terminal, offers to move the agents of the project at or above `cwd` from where an older
/// ai kept them; an error if declined. Off a terminal it does nothing, and commands that read the
/// agents refuse them until they move.
pub fn offer(cwd: &Path) -> Result<()> {
    let Some(project) = Project::find(cwd)? else {
        return Ok(());
    };
    if old(&project).is_none() || !asks() {
        return Ok(());
    }
    offer_with(&project, &Homes::user()?, |q| {
        Ok(Confirm::new().with_prompt(q).default(false).interact()?)
    })
}

/// Moves the agents of `project` into `homes` once `ask` agrees, telling what moved; an error if
/// `ask` declines.
pub fn offer_with(
    project: &Project,
    homes: &Homes,
    ask: impl FnOnce(&str) -> Result<bool>,
) -> Result<()> {
    let Some(from) = old(project) else {
        return Ok(());
    };
    let question = format!(
        "Move this project's agents and private settings from {} to {}? Each copy of a session's \
         file becomes a pointer to the harness's own file; the old store is archived in {}.",
        pretty(&from),
        pretty(&project.folder(homes)?),
        pretty(&homes.machine.join(ARCHIVE))
    );
    if !ask(&question)? {
        bail!(
            "Not moved: this project's agents stay in {}, and ai agent commands refuse them until \
             they move.",
            pretty(&from)
        );
    }
    let moved = convert(project, homes)?;
    eprintln!(
        "Moved {} to {}: {} {}, {} session {} pointed at in place.",
        pretty(&from),
        pretty(&project.folder(homes)?),
        moved.threads,
        if moved.threads == 1 {
            "thread"
        } else {
            "threads"
        },
        moved.pointed,
        if moved.pointed == 1 { "file" } else { "files" },
    );
    for (copy, to) in &moved.restored {
        eprintln!(
            "Restored {} from its copy: {copy}.",
            pretty(to),
            copy = copy.display()
        );
    }
    eprintln!("The old store is archived in {}.", pretty(&moved.archive));
    Ok(())
}

/// Whether a question can be put on the terminal: stdin and stderr are one, and reading it will
/// not stop this process as a background job of it.
fn asks() -> bool {
    let stdin = io::stdin();
    if !(stdin.is_terminal() && io::stderr().is_terminal()) {
        return false;
    }
    let group = unsafe { libc::tcgetpgrp(stdin.as_raw_fd()) };
    group < 0 || group == unsafe { libc::getpgrp() }
}

/// What a move did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Moved {
    pub threads: usize,
    /// The copies whose files the harness still holds, now pointed at.
    pub pointed: usize,
    /// Each copy the harness no longer held whole, by its path in the old store, and where it
    /// was restored.
    pub restored: Vec<(PathBuf, PathBuf)>,
    /// Where the old store is archived.
    pub archive: PathBuf,
}

/// Moves the agents, private settings and other files of `project`'s old store into its folder
/// of `homes.repo`, archives the old store in `homes.machine`, removes it, and commits the folder.
/// Nothing is removed before the archive is checked byte for byte. A move cut short after its
/// store is in place is finished by the next one.
pub fn convert(project: &Project, homes: &Homes) -> Result<Moved> {
    let from = old(project).with_context(|| {
        format!(
            "{} holds no store to move.",
            pretty(&project.home.join(OLD_STORE))
        )
    })?;
    let agents = from.join(DIR);
    if agents.exists() {
        match file::format(&agents, "agent")? {
            Some(FROM) => {}
            v => bail!(
                "Cannot move {}: its agents are format {}; this ai moves format {FROM}.",
                pretty(&from),
                v.map_or("unknown".into(), |v| v.to_string())
            ),
        }
    }
    let _locks = lock_old(&agents)?;
    let folder = project.claim(homes)?;
    let root = folder.join(DIR);
    let old_threads = names(&agents.join(THREADS))?;
    let archive = archive(&from, &homes.machine.join(ARCHIVE).join(&project.id))?;
    let mut moved = Moved {
        threads: old_threads.len(),
        archive: archive.clone(),
        ..Moved::default()
    };
    move_settings(&from, &folder)?;
    if root.exists() {
        let have = names(&root.join(THREADS))?;
        if !old_threads.is_subset(&have) {
            bail!(
                "Cannot move {}: {} already holds agents ({}). The old store is archived in {}.",
                pretty(&from),
                pretty(&root),
                have.into_iter().collect::<Vec<_>>().join(", "),
                pretty(&archive)
            );
        }
    } else if agents.exists() {
        let tmp = tempfile::Builder::new()
            .prefix(".agents-")
            .tempdir_in(&folder)
            .at(&folder)?;
        build(&agents, tmp.path(), &mut moved)?;
        fs::rename(tmp.path(), &root).at(&root)?;
        let _ = tmp.keep();
    }
    fs::remove_dir_all(&from).at(&from)?;
    let rel = folder
        .strip_prefix(&homes.repo)
        .expect("a project's folder is in the user's repository");
    repo::commit_paths(
        &homes.repo,
        &[rel],
        "ai",
        &format!("ai: move {}'s agents from {OLD_STORE}", project.name),
    )?;
    store::gc(homes, &Settings::find_in(homes, &project.home));
    Ok(moved)
}

/// The writers' and compactors' locks of every thread in the old store `agents`, so no older ai
/// writes while it moves; an error naming a thread one of them still holds. An older ai makes a
/// lock's file before taking it, so a lock without its file is held by none.
fn lock_old(agents: &Path) -> Result<Vec<file::Lock>> {
    let mut locks = Vec::new();
    if !agents.exists() {
        return Ok(locks);
    }
    let mut files = vec![agents.join("sessions.lock")];
    for n in names(&agents.join(THREADS))? {
        files.push(agents.join(format!("{n}.write.lock")));
        files.push(agents.join(format!("{n}.lock")));
    }
    for f in files.into_iter().filter(|f| f.is_file()) {
        match file::try_lock(&f)? {
            Some(l) => locks.push(l),
            None => bail!(
                "An older ai is writing this project's agents ({} is held). Let it finish, then \
                 try again.",
                pretty(&f)
            ),
        }
    }
    Ok(locks)
}

/// The thread names among the directories of `dir`; none if it does not exist.
fn names(dir: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    match fs::read_dir(dir) {
        Ok(entries) => {
            for e in entries {
                let e = e.at(dir)?;
                if e.file_type().at(dir)?.is_dir()
                    && let Some(n) = e.file_name().to_str().filter(|n| check_name(n).is_ok())
                {
                    out.insert(n.to_string());
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).at(dir),
    }
    Ok(out)
}

/// Copies the old store `from` whole into a new archive under `dir`, checks the copy byte for
/// byte, and returns it; an archive of the same store from a move cut short is reused.
fn archive(from: &Path, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir).at(dir)?;
    for (name, p, kind) in entries(dir)? {
        if kind.is_dir() && name.ends_with("--local") && same_tree(from, &p)? {
            return Ok(p);
        }
    }
    let stamp = Timestamp::now().strftime("%Y%m%dT%H%M%S").to_string();
    let to = dir.join(format!("{stamp}--local"));
    let partial = dir.join(format!("{stamp}--local.partial"));
    if partial.exists() {
        fs::remove_dir_all(&partial).at(&partial)?;
    }
    copy_tree(from, &partial)?;
    if !same_tree(from, &partial)? {
        bail!(
            "The archive {} differs from {}. Nothing was moved.",
            pretty(&partial),
            pretty(from)
        );
    }
    fs::rename(&partial, &to).at(&to)?;
    Ok(to)
}

/// Moves the old store's private settings and its other files into the project's `folder`: a
/// file already there must be empty or the same. Its agents, git repository and ignore file stay
/// for the archive.
fn move_settings(from: &Path, folder: &Path) -> Result<()> {
    for (name, p, kind) in entries(from)? {
        let to = folder.join(&name);
        match name.as_str() {
            DIR | ".git" | ".gitignore" => {}
            FILE => {
                let text = fs::read(&p).at(&p)?;
                match fs::read(&to) {
                    Ok(have) if have == text => {}
                    Ok(have) if have.is_empty() => fs::write(&to, &text).at(&to)?,
                    Ok(_) => bail!(
                        "Cannot move {}: {} already holds other settings.",
                        pretty(&p),
                        pretty(&to)
                    ),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        fs::write(&to, &text).at(&to)?
                    }
                    Err(e) => return Err(e).at(&to),
                }
            }
            _ if kind.is_symlink() => {}
            _ => {
                if to.exists() {
                    if same_tree(&p, &to)? {
                        continue;
                    }
                    bail!(
                        "Cannot move {}: {} already exists.",
                        pretty(&p),
                        pretty(&to)
                    );
                }
                copy_tree(&p, &to)?;
            }
        }
    }
    Ok(())
}

/// A change of a session's attachment as the old store kept it, for every thread in one file.
#[derive(Deserialize)]
struct OldEvent {
    harness: String,
    session: String,
    at: Timestamp,
    state: State,
    thread: Option<String>,
}

/// Builds at `to` the store of format [`FORMAT`] from the old store `agents`: each thread's own
/// files copied, its attachment changes taken from the store-wide file, its session tracks and
/// sources pointed at the harnesses' files.
fn build(agents: &Path, to: &Path, moved: &mut Moved) -> Result<()> {
    file::ensure_format(to, FORMAT)?;
    let mut events: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    let mut owner: BTreeMap<(String, String), String> = BTreeMap::new();
    let all = complete(joined(&agents.join(SESSIONS))?);
    for (i, l) in jsonl::lines(&all).enumerate() {
        let e: OldEvent = jsonl::decode(l).with_context(|| {
            format!(
                "{}: record {} is corrupt",
                pretty(&agents.join(SESSIONS)),
                i + 1
            )
        })?;
        let key = (e.harness.clone(), e.session.clone());
        let thread = match (e.state, e.thread) {
            (State::Attached, Some(t)) => {
                owner.insert(key, t.clone());
                t
            }
            (State::Attached, None) => bail!(
                "{}: record {} attaches a session to no thread.",
                pretty(&agents.join(SESSIONS)),
                i + 1
            ),
            (State::Detached, _) => match owner.remove(&key) {
                Some(t) => t,
                None => continue,
            },
        };
        events.entry(thread).or_default().push(Event {
            harness: e.harness,
            session: e.session,
            at: e.at,
            state: e.state,
        });
    }
    let threads = agents.join(THREADS);
    for name in names(&threads)? {
        let dst = to.join(THREADS).join(&name);
        thread(&threads.join(&name), &dst, moved)?;
        jsonl::append(
            &dst.join(ATTACHMENTS),
            events.get(&name).map_or(&[][..], Vec::as_slice),
        )?;
    }
    Ok(())
}

/// One thread of the old store at `src`, built at `dst`.
fn thread(src: &Path, dst: &Path, moved: &mut Moved) -> Result<()> {
    fs::create_dir_all(dst).at(dst)?;
    let mut tracks = Vec::new();
    for (name, from, _) in entries(src)? {
        match name.as_str() {
            ITEMS | LINES | AGENTS => join(&from, &dst.join(&name))?,
            "agents" => join_subagents(&from, &dst.join(&name))?,
            "calls" => copy_tree(&from, &dst.join(&name))?,
            SESSIONS => {
                let b = complete(joined(&from)?);
                for (i, l) in jsonl::lines(&b).enumerate() {
                    let t: Track = jsonl::decode(l).with_context(|| {
                        format!("{}: record {} is corrupt", pretty(&from), i + 1)
                    })?;
                    tracks.push(t);
                }
            }
            "sources" => {}
            _ => bail!(
                "Cannot move {}: it is not part of a format-{FROM} thread.",
                pretty(&from)
            ),
        }
    }
    let mut pointers = Vec::new();
    let host = backup::host()?;
    let read = Timestamp::now();
    let sources = src.join("sources");
    for (harness, by_harness, _) in entries_if(&sources)? {
        for (session, dir, _) in entries(&by_harness)? {
            let transcript = tracks
                .iter()
                .rfind(|t| t.harness == harness && t.session == session)
                .and_then(|t| t.transcript.clone())
                .with_context(|| {
                    format!(
                        "Cannot move {}: no session record names its transcript.",
                        pretty(&dir)
                    )
                })?;
            let base = transcript
                .parent()
                .context("A transcript needs a parent directory.")?;
            let companion = transcript.with_extension("");
            for (rel, copy) in copies(&dir)? {
                let bytes = joined(&copy)?;
                let raw = base.join(&rel);
                let at = match holds(&raw, &bytes)? {
                    true => {
                        moved.pointed += 1;
                        raw
                    }
                    false => {
                        let to = restore(&raw, &companion, &rel, &bytes)?;
                        moved.restored.push((copy.clone(), to.clone()));
                        to
                    }
                };
                pointers.push(Pointer {
                    harness: harness.clone(),
                    session: session.clone(),
                    host: host.clone(),
                    path: source::real(&at),
                    from: 0,
                    len: bytes.len() as u64,
                    sha256: sha256(&bytes),
                    at: read,
                });
            }
        }
    }
    for t in &mut tracks {
        t.transcript = t.transcript.take().map(|p| source::real(&p));
    }
    jsonl::append(&dst.join(SESSIONS), &tracks)?;
    jsonl::append(&dst.join(SOURCES), &pointers)?;
    Ok(())
}

/// Every copied file under a session's sources `dir`, by its path there: each a segmented file.
fn copies(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
        for (_, p, kind) in entries(dir)? {
            if is_segmented(&p) {
                let rel = p.strip_prefix(root).expect("under the walk's root");
                out.push((rel.to_path_buf(), p));
            } else if kind.is_dir() {
                walk(root, &p, out)?;
            } else {
                bail!(
                    "Cannot move {}: a format-{FROM} store keeps each copied file as a directory \
                     of segments.",
                    pretty(&p)
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

/// Whether `p` is a file as a format-4 store keeps one: a directory of segments, files
/// numbered from `000000`, each but the last ending a line.
fn is_segmented(p: &Path) -> bool {
    fs::read_dir(p).is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.path().is_file())))
}

/// Every byte of the segmented file `p`, its segments joined in order; empty if `p` does not
/// exist.
fn joined(p: &Path) -> Result<Vec<u8>> {
    if !p.exists() {
        return Ok(Vec::new());
    }
    if !p.is_dir() {
        bail!(
            "Cannot move {}: a format-{FROM} store keeps it as a directory of segments.",
            pretty(p)
        );
    }
    let segments = entries(p)?;
    let mut out = Vec::new();
    for (i, (name, seg, kind)) in segments.iter().enumerate() {
        let want = match p.extension() {
            Some(ext) => format!("{i:06}.{}", ext.to_string_lossy()),
            None => format!("{i:06}"),
        };
        if *name != want || !kind.is_file() {
            bail!(
                "Cannot move {}: its segments are not numbered from 0.",
                pretty(p)
            );
        }
        let b = fs::read(seg).at(seg)?;
        if i + 1 < segments.len() && b.last() != Some(&b'\n') {
            bail!("Cannot move {}: segment {i} ends inside a line.", pretty(p));
        }
        out.extend(b);
    }
    Ok(out)
}

/// `b`'s complete lines, as [`jsonl::read`] gives a file's.
fn complete(mut b: Vec<u8>) -> Vec<u8> {
    b.truncate(jsonl::complete(&b));
    b
}

/// Writes the segmented file `from` at `to` as one file of the same bytes.
fn join(from: &Path, to: &Path) -> Result<()> {
    fs::write(to, joined(from)?).at(to)
}

/// Writes the subagents' files under `from`, `<harness>/<agent>.jsonl` each segmented, under
/// `to` as one file each.
fn join_subagents(from: &Path, to: &Path) -> Result<()> {
    for (harness, dir, kind) in entries(from)? {
        if !kind.is_dir() {
            bail!(
                "Cannot move {}: it is not part of a format-{FROM} thread.",
                pretty(&dir)
            );
        }
        let out = to.join(&harness);
        fs::create_dir_all(&out).at(&out)?;
        for (name, file, _) in entries(&dir)? {
            join(&file, &out.join(name))?;
        }
    }
    Ok(())
}

/// Whether the file at `raw` begins with `bytes`.
fn holds(raw: &Path, bytes: &[u8]) -> Result<bool> {
    match fs::read(raw) {
        Ok(have) => Ok(have.starts_with(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).at(raw),
    }
}

/// Writes the copy `bytes` of the harness file `raw`, at `rel` in its transcript's directory,
/// back: at `raw` when it is gone, else as `restored/<rel>` in the session's `companion`
/// directory, which every sync reads, numbered `restored-2`, … when that path holds other
/// bytes. Returns where.
fn restore(raw: &Path, companion: &Path, rel: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let mut at = raw.to_path_buf();
    let mut n = 0;
    while at.exists() {
        if fs::read(&at).at(&at)? == bytes {
            return Ok(at);
        }
        n += 1;
        at = companion
            .join(match n {
                1 => "restored".to_string(),
                n => format!("restored-{n}"),
            })
            .join(rel);
    }
    if let Some(dir) = at.parent() {
        fs::create_dir_all(dir).at(dir)?;
    }
    fs::write(&at, bytes).at(&at)?;
    if fs::read(&at).at(&at)? != bytes {
        bail!("Restored {} reads back different bytes.", pretty(&at));
    }
    Ok(at)
}

/// The entries of `dir`: name, path, type, by name.
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf, FileType)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).at(dir)? {
        let e = e.at(dir)?;
        let name = e
            .file_name()
            .into_string()
            .map_err(|n| anyhow!("{}: a name is not UTF-8: {n:?}", pretty(dir)))?;
        out.push((name, e.path(), e.file_type().at(dir)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// [`entries`], none if `dir` does not exist.
fn entries_if(dir: &Path) -> Result<Vec<(String, PathBuf, FileType)>> {
    match dir.exists() {
        true => entries(dir),
        false => Ok(Vec::new()),
    }
}

/// Copies `from` to `to`: a directory with everything in it, links as links.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(from).at(from)?;
    if meta.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from).at(from)?, to).at(to)?;
    } else if meta.is_dir() {
        fs::create_dir_all(to).at(to)?;
        for (name, p, _) in entries(from)? {
            copy_tree(&p, &to.join(name))?;
        }
    } else {
        fs::copy(from, to).at(from)?;
    }
    Ok(())
}

/// Whether `a` and `b` hold the same: the same names, links to the same places, files of the same
/// bytes.
fn same_tree(a: &Path, b: &Path) -> Result<bool> {
    let (ma, mb) = (
        fs::symlink_metadata(a).at(a)?,
        match fs::symlink_metadata(b) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).at(b),
        },
    );
    if ma.is_symlink() || mb.is_symlink() {
        return Ok(ma.is_symlink()
            && mb.is_symlink()
            && fs::read_link(a).at(a)? == fs::read_link(b).at(b)?);
    }
    if ma.is_dir() != mb.is_dir() {
        return Ok(false);
    }
    if !ma.is_dir() {
        return Ok(ma.len() == mb.len() && fs::read(a).at(a)? == fs::read(b).at(b)?);
    }
    let (ea, eb) = (entries(a)?, entries(b)?);
    if ea.len() != eb.len() {
        return Ok(false);
    }
    for ((na, pa, _), (nb, pb, _)) in ea.iter().zip(&eb) {
        if na != nb || !same_tree(pa, pb)? {
            return Ok(false);
        }
    }
    Ok(true)
}
