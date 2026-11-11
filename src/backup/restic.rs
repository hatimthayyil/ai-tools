//! restic, run as a program: every byte a backup keeps is in a repository restic wrote, read and
//! checks.
//!
//! A run never forgets or prunes, so a repository only grows. The repository and password of
//! each command come from its arguments alone: the variables restic would read them from are
//! taken out of its environment, so a shell's own restic setup never reaches `ai`'s.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
    thread,
};

use anyhow::{Context, Result, anyhow};
use jiff::Timestamp;
use serde::{
    Deserialize, Deserializer,
    de::{SeqAccess, Visitor},
};

/// The program, found on `PATH`.
const PROGRAM: &str = "restic";
/// What restic exits with when a backup made its snapshot without some files it could not read.
const UNREAD: i32 = 3;
/// What restic exits with when there is no repository at the address.
const NO_REPOSITORY: i32 = 10;
/// The starts of the variables restic reads a repository, a password or a key from.
const DROPPED: [&str; 4] = [
    "RESTIC_REPOSITORY",
    "RESTIC_PASSWORD",
    "RESTIC_FROM_",
    "RESTIC_KEY_HINT",
];

/// A repository's password.
#[derive(Clone, PartialEq, Eq)]
pub enum Password {
    /// A file that holds it.
    File(PathBuf),
    /// The password itself, as its command printed it. restic gets it in a variable of its own
    /// environment, never as an argument, which any program on the machine can read.
    Given(String),
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Password::File(p) => write!(f, "File({p:?})"),
            Password::Given(_) => f.write_str("Given(***)"),
        }
    }
}

/// A repository, and what restic needs to open it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    /// The repository as restic names it: a folder's path, or a backend's address.
    pub address: OsString,
    pub password: Password,
    /// Variables its backend reads, such as a store's access keys.
    pub env: Vec<(String, String)>,
}

impl Repo {
    /// The repository in the folder `dir`.
    pub fn local(dir: &Path, password: Password) -> Repo {
        Repo {
            address: dir.into(),
            password,
            env: Vec::new(),
        }
    }
}

/// What a backup did, or with `dry`, would do.
#[derive(Debug, PartialEq, Eq)]
pub struct Backup {
    /// The files new or changed since the snapshot before.
    pub changed: u64,
    /// The bytes it added to the repository.
    pub added: u64,
    /// The snapshot it made; `None` for a dry run.
    pub snapshot: Option<String>,
    /// The files it could not read, each with why; the snapshot holds the rest.
    pub unread: Vec<String>,
}

/// A line restic prints with `--json`.
#[derive(Deserialize)]
#[serde(tag = "message_type", rename_all = "snake_case")]
enum Message {
    Summary {
        files_new: u64,
        files_changed: u64,
        data_added: u64,
        snapshot_id: Option<String>,
    },
    Error {
        error: Reason,
        #[serde(default)]
        item: String,
    },
    ExitError {
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Reason {
    message: String,
}

/// Whether restic would read a repository, a password or a key from the variable `name`.
fn dropped(name: &str) -> bool {
    DROPPED.iter().any(|d| name.starts_with(d))
}

/// `restic` on `repo`, its environment cleared of another repository's settings.
fn command(repo: &Repo) -> Command {
    let mut c = Command::new(PROGRAM);
    for (k, _) in std::env::vars_os() {
        if k.to_str().is_some_and(dropped) {
            c.env_remove(k);
        }
    }
    c.envs(repo.env.iter().map(|(k, v)| (k, v)));
    c.arg("--repo").arg(&repo.address);
    match &repo.password {
        Password::File(p) => c.arg("--password-file").arg(p),
        Password::Given(s) => c.env("RESTIC_PASSWORD", s),
    };
    c.stdin(Stdio::null());
    c
}

/// Names `from` as the repository `c` copies from.
fn from(c: &mut Command, from: &Repo) {
    c.arg("--from-repo").arg(&from.address);
    match &from.password {
        Password::File(p) => c.arg("--from-password-file").arg(p),
        Password::Given(s) => c.env("RESTIC_FROM_PASSWORD", s),
    };
}

/// An error that restic could not start; says so plainly when there is no restic.
fn unstarted(e: io::Error) -> anyhow::Error {
    match e.kind() {
        io::ErrorKind::NotFound => anyhow!(
            "Cannot back up: no restic program on PATH. Install restic: \
             https://restic.readthedocs.io/en/stable/020_installation.html"
        ),
        _ => anyhow::Error::new(e).context("Cannot start restic."),
    }
}

fn output(mut c: Command) -> Result<Output> {
    c.output().map_err(unstarted)
}

/// What restic said on its error output: each JSON message's text, any other line as it is.
fn said(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match serde_json::from_str::<Message>(l) {
            Ok(Message::Error { error, item }) if item.is_empty() => error.message,
            Ok(Message::Error { error, item }) => format!("{item}: {}", error.message),
            Ok(Message::ExitError { message }) => message,
            _ => l.to_owned(),
        })
        .collect()
}

/// The error of `restic <what>` that ended with `out`.
fn failed(what: &str, repo: &Repo, out: &Output) -> anyhow::Error {
    anyhow!(
        "restic {what} failed on {} ({}): {}",
        shown(&repo.address),
        out.status,
        said(&out.stderr).join("; ")
    )
}

/// `address` as it may be printed: a URL's user and password left out.
pub fn shown(address: &OsStr) -> String {
    let a = address.to_string_lossy();
    let Some(scheme) = a.find("://").map(|i| i + 3) else {
        return a.into_owned();
    };
    let host = a[scheme..].find('/').map_or(a.len(), |i| scheme + i);
    match a[scheme..host].rfind('@') {
        Some(at) => format!("{}***{}", &a[..scheme], &a[scheme + at..]),
        None => a.into_owned(),
    }
}

/// Whether there is a repository at `repo`'s address.
pub fn exists(repo: &Repo) -> Result<bool> {
    let mut c = command(repo);
    c.args(["cat", "config"]);
    let out = output(c)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(NO_REPOSITORY) => Ok(false),
        _ => Err(failed("cat config", repo, &out)),
    }
}

/// Makes the repository `repo`; with `like`, one that cuts files into chunks as `like` does, so
/// snapshots copied from it store each chunk once.
pub fn init(repo: &Repo, like: Option<&Repo>) -> Result<()> {
    let mut c = command(repo);
    c.arg("init");
    if let Some(like) = like {
        from(&mut c, like);
        c.arg("--copy-chunker-params");
    }
    let out = output(c)?;
    match out.status.success() {
        true => Ok(()),
        false => Err(failed("init", repo, &out)),
    }
}

/// The arguments of a backup of `paths`, as the machine `host`'s.
fn backup_command(repo: &Repo, host: &str, paths: &[PathBuf], dry: bool) -> Command {
    let mut c = command(repo);
    c.args(["backup", "--json", "--quiet", "--host", host]);
    if dry {
        c.arg("--dry-run");
    }
    c.arg("--").args(paths);
    c
}

/// Backs up `paths`, each a real path that exists, into `repo` as one snapshot of the machine
/// `host`; with `dry`, only finds what a backup would do.
pub fn backup(repo: &Repo, host: &str, paths: &[PathBuf], dry: bool) -> Result<Backup> {
    let out = output(backup_command(repo, host, paths, dry))?;
    let unread = match out.status.code() {
        Some(0) => Vec::new(),
        Some(UNREAD) => said(&out.stderr),
        _ => return Err(failed("backup", repo, &out)),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| match serde_json::from_str::<Message>(l) {
            Ok(Message::Summary {
                files_new,
                files_changed,
                data_added,
                snapshot_id,
            }) => Some(Backup {
                changed: files_new + files_changed,
                added: data_added,
                snapshot: snapshot_id.filter(|_| !dry),
                unread: unread.clone(),
            }),
            _ => None,
        })
        .with_context(|| {
            format!(
                "restic backup on {} ended without its summary.",
                shown(&repo.address)
            )
        })
}

/// Copies every snapshot of `source` that `to` lacks into `to`.
pub fn copy(to: &Repo, source: &Repo) -> Result<()> {
    let mut c = command(to);
    c.arg("copy");
    from(&mut c, source);
    let out = output(c)?;
    match out.status.success() {
        true => Ok(()),
        false => Err(failed("copy", to, &out)),
    }
}

/// A file as a snapshot holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub snapshot: String,
    pub path: PathBuf,
    pub size: u64,
    /// When the file was last written. Of two states of a file, the later is the newer.
    pub written: Timestamp,
}

/// One snapshot's part of what `restic find --json` prints.
#[derive(Deserialize)]
struct Hit {
    matches: Vec<Match>,
    snapshot: String,
}

#[derive(Deserialize)]
struct Match {
    path: PathBuf,
    #[serde(rename = "type")]
    kind: String,
    /// restic leaves out the size of an empty file.
    #[serde(default)]
    size: u64,
    mtime: Timestamp,
}

/// A snapshot as `restic snapshots --json` prints it.
#[derive(Deserialize)]
struct Listed {
    time: Timestamp,
    id: String,
}

/// Hands each element of a JSON list to `each`, one in memory at a time: a repository of many
/// thousands of snapshots prints lists of as many elements.
struct Each<'a, T>(&'a mut dyn FnMut(T));

impl<'de, T: Deserialize<'de>> Visitor<'de> for Each<'_, T> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(element) = seq.next_element::<T>()? {
            (self.0)(element);
        }
        Ok(())
    }
}

/// Runs `c`, the `restic <what>` on `repo` that prints a JSON list, handing each element to
/// `each` as it is printed.
fn listed<T: for<'de> Deserialize<'de>>(
    mut c: Command,
    what: &str,
    repo: &Repo,
    each: &mut dyn FnMut(T),
) -> Result<()> {
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(unstarted)?;
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let errors = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let parsed = serde_json::Deserializer::from_reader(stdout).deserialize_seq(Each(each));
    let status = child.wait().context("Cannot wait for restic.")?;
    let out = Output {
        status,
        stdout: Vec::new(),
        stderr: errors.join().unwrap_or_default(),
    };
    if !status.success() {
        return Err(failed(what, repo, &out));
    }
    parsed.with_context(|| {
        format!(
            "restic {what} on {} printed something that is no list.",
            shown(&repo.address)
        )
    })
}

/// The snapshots of the machine `host` in `repo`, each with when it was made, oldest first.
pub fn snapshots(repo: &Repo, host: &str) -> Result<Vec<(Timestamp, String)>> {
    let mut c = command(repo);
    c.args(["snapshots", "--json", "--host", host]);
    let mut all = Vec::new();
    listed(c, "snapshots", repo, &mut |s: Listed| {
        all.push((s.time, s.id))
    })?;
    all.sort();
    Ok(all)
}

/// The snapshots a search looks in.
#[derive(Clone, Copy, Debug)]
pub enum In<'a> {
    /// The machine's newest.
    Newest,
    /// These, by id.
    These(&'a [&'a str]),
}

/// `path` as a pattern of `restic find` that matches it alone.
fn pattern(path: &Path) -> OsString {
    let mut out = OsString::new();
    let mut buf = [0u8; 4];
    for c in path.to_string_lossy().chars() {
        if matches!(c, '\\' | '*' | '?' | '[') {
            out.push("\\");
        }
        out.push(c.encode_utf8(&mut buf));
    }
    out
}

/// The arguments of a search for `paths` in snapshots of the machine `host`.
fn find_command(repo: &Repo, host: &str, paths: &[PathBuf], within: In) -> Command {
    let mut c = command(repo);
    c.args(["find", "--json", "--host", host]);
    match within {
        In::Newest => {
            c.args(["--snapshot", "latest"]);
        }
        In::These(ids) => {
            for id in ids {
                c.args(["--snapshot", id]);
            }
        }
    }
    c.arg("--").args(paths.iter().map(|p| pattern(p)));
    c
}

/// Hands `each` every file at one of `paths`, absolute paths, as each snapshot of `within`, of
/// the machine `host` in `repo`, holds it. A search walks every snapshot it looks in, so it
/// never looks in them all: a repository that never forgets holds many thousands.
pub fn find(
    repo: &Repo,
    host: &str,
    paths: &[PathBuf],
    within: In,
    each: &mut dyn FnMut(Found),
) -> Result<()> {
    if matches!(within, In::These([])) {
        return Ok(());
    }
    let c = find_command(repo, host, paths, within);
    listed(c, "find", repo, &mut |hit: Hit| {
        for m in hit.matches.into_iter().filter(|m| m.kind == "file") {
            each(Found {
                snapshot: hit.snapshot.clone(),
                path: m.path,
                size: m.size,
                written: m.mtime,
            });
        }
    })
}

/// The file at `path` as `snapshot` of `repo` holds it, whole.
pub fn dump(repo: &Repo, snapshot: &str, path: &Path) -> Result<Vec<u8>> {
    let mut c = command(repo);
    c.args(["dump", "--quiet", "--", snapshot]).arg(path);
    let out = output(c)?;
    match out.status.success() {
        true => Ok(out.stdout),
        false => Err(failed("dump", repo, &out)),
    }
}

/// Runs `restic check` on `repo`, printing as restic does; with `read_data`, every byte it holds
/// is read and verified. Waits for another program's lock on the repository.
pub fn check(repo: &Repo, read_data: bool) -> Result<ExitStatus> {
    let mut c = command(repo);
    c.args(["check", "--retry-lock", "5m"]);
    if read_data {
        c.arg("--read-data");
    }
    c.status().map_err(unstarted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn local() -> Repo {
        Repo::local(
            Path::new("/h/.ai/backup"),
            Password::File("/h/.ai/k".into()),
        )
    }

    fn remote() -> Repo {
        Repo {
            address: "s3:https://h/b".into(),
            password: Password::Given("secret".into()),
            env: vec![("AWS_ACCESS_KEY_ID".into(), "id".into())],
        }
    }

    #[test]
    fn a_backup_names_its_repository_password_and_paths() {
        let paths = [PathBuf::from("/h/.claude/projects"), "/h/-odd".into()];
        assert_eq!(
            args(&backup_command(&local(), "eagle", &paths, false)),
            [
                "--repo",
                "/h/.ai/backup",
                "--password-file",
                "/h/.ai/k",
                "backup",
                "--json",
                "--quiet",
                "--host",
                "eagle",
                "--",
                "/h/.claude/projects",
                "/h/-odd"
            ]
        );
        let dry = args(&backup_command(&local(), "eagle", &paths, true));
        assert!(dry.contains(&"--dry-run".to_owned()));
    }

    #[test]
    fn a_command_carries_its_repositorys_variables_and_drops_another_repositorys() {
        let c = command(&remote());
        assert_eq!(args(&c), ["--repo", "s3:https://h/b"]);
        let set: Vec<_> = c
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?, v?.to_str()?)))
            .collect();
        assert_eq!(
            set,
            [("AWS_ACCESS_KEY_ID", "id"), ("RESTIC_PASSWORD", "secret")]
        );
        assert_eq!(format!("{:?}", remote().password), "Given(***)");
        for name in [
            "RESTIC_REPOSITORY",
            "RESTIC_REPOSITORY_FILE",
            "RESTIC_PASSWORD",
            "RESTIC_PASSWORD_FILE",
            "RESTIC_PASSWORD_COMMAND",
            "RESTIC_FROM_REPOSITORY",
            "RESTIC_FROM_PASSWORD_COMMAND",
            "RESTIC_KEY_HINT",
        ] {
            assert!(dropped(name), "{name}");
        }
        for name in [
            "RESTIC_CACHE_DIR",
            "RESTIC_HOST",
            "AWS_ACCESS_KEY_ID",
            "PATH",
        ] {
            assert!(!dropped(name), "{name}");
        }
    }

    #[test]
    fn a_copy_names_the_repository_it_copies_from() {
        let mut c = command(&remote());
        from(&mut c, &local());
        assert_eq!(
            args(&c)[2..],
            [
                "--from-repo",
                "/h/.ai/backup",
                "--from-password-file",
                "/h/.ai/k"
            ]
        );
        let mut c = command(&local());
        from(&mut c, &remote());
        assert_eq!(args(&c)[4..], ["--from-repo", "s3:https://h/b"]);
        assert!(
            c.get_envs()
                .any(|(k, v)| k == "RESTIC_FROM_PASSWORD" && v == Some(OsStr::new("secret")))
        );
    }

    #[test]
    fn a_search_names_each_path_as_a_pattern_of_it_alone() {
        let paths = [PathBuf::from("/h/a/s-1.jsonl"), r"/h/b[1]/*?\x".into()];
        assert_eq!(
            args(&find_command(&local(), "eagle", &paths, In::Newest))[4..],
            [
                "find",
                "--json",
                "--host",
                "eagle",
                "--snapshot",
                "latest",
                "--",
                "/h/a/s-1.jsonl",
                r"/h/b\[1]/\*\?\\x"
            ]
        );
        assert_eq!(
            args(&find_command(
                &local(),
                "eagle",
                &paths[..1],
                In::These(&["aa", "bb"])
            ))[8..],
            [
                "--snapshot",
                "aa",
                "--snapshot",
                "bb",
                "--",
                "/h/a/s-1.jsonl"
            ]
        );
    }

    #[test]
    fn the_hits_of_a_search_are_read_one_at_a_time() {
        let json = br#"[{"matches":[{"path":"/h/a","type":"file","mtime":"2026-11-11T01:10:53.959520665Z","size":7},{"path":"/h","type":"dir","mtime":"2026-11-11T01:10:53Z"}],"hits":2,"snapshot":"aa"},{"matches":[{"path":"/h/e","type":"file","mtime":"2026-11-11T02:00:00+01:00"}],"hits":1,"snapshot":"bb"}]"#;
        let mut found = Vec::new();
        serde_json::Deserializer::from_slice(json)
            .deserialize_seq(Each(&mut |h: Hit| found.push(h)))
            .unwrap();
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        let seen: Vec<_> = found
            .iter()
            .flat_map(|h| h.matches.iter().map(move |m| (h.snapshot.as_str(), m)))
            .map(|(s, m)| {
                (
                    s,
                    m.path.to_str().unwrap(),
                    m.kind.as_str(),
                    m.size,
                    m.mtime,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "aa",
                    "/h/a",
                    "file",
                    7,
                    at("2026-11-11T01:10:53.959520665Z")
                ),
                ("aa", "/h", "dir", 0, at("2026-11-11T01:10:53Z")),
                ("bb", "/h/e", "file", 0, at("2026-11-11T01:00:00Z")),
            ]
        );

        let json = br#"[{"time":"2026-11-11T02:00:00+01:00","tree":"t","paths":["/h"],"hostname":"eagle","id":"aa","short_id":"a"}]"#;
        let mut listed = Vec::new();
        serde_json::Deserializer::from_slice(json)
            .deserialize_seq(Each(&mut |s: Listed| listed.push((s.time, s.id))))
            .unwrap();
        assert_eq!(listed, [(at("2026-11-11T01:00:00Z"), "aa".to_owned())]);
    }

    #[test]
    fn restics_messages_read_as_text() {
        let stderr = br#"{"message_type":"error","error":{"message":"open /a: permission denied"},"during":"archival","item":"/a"}
{"message_type":"exit_error","code":3,"message":"Warning: at least one source file could not be read"}
Fatal: plain words
"#;
        assert_eq!(
            said(stderr),
            [
                "/a: open /a: permission denied",
                "Warning: at least one source file could not be read",
                "Fatal: plain words"
            ]
        );
    }

    #[test]
    fn an_address_is_shown_without_its_password() {
        let shown = |a: &str| shown(OsStr::new(a));
        assert_eq!(shown("/h/.ai/backup"), "/h/.ai/backup");
        assert_eq!(shown("s3:https://h/b"), "s3:https://h/b");
        assert_eq!(
            shown("rest:https://u:p@h:8000/r@x"),
            "rest:https://***@h:8000/r@x"
        );
    }
}
