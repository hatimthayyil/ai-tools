use std::{
    collections::BTreeSet,
    env,
    fs::{self, DirBuilder, File},
    io::{self, Write},
    ops::Range,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use tempfile::NamedTempFile;

use crate::{file::AtPath, memory::broken_pipe};

/// The largest part of a paged output, in bytes: under Claude Code's 30,000-character cap on
/// a command's output.
pub const PART: u64 = 20_000;

/// How long a saved view lasts after it was last read.
pub const LIFE: Duration = Duration::from_secs(10 * 60);

/// The command that prints the next part.
pub const PAGE: &str = "ai page";

/// `ai page`: a part of a paged output, as the part before it names it.
#[derive(Args, Debug)]
pub struct Cli {
    #[arg(help = "a view and its part, as printed: 17:2")]
    page: String,
}

impl Cli {
    /// Prints the part from the views in the cache folder to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        match show(&cache()?, &self.page, &mut io::stdout().lock()) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r.map(|()| ExitCode::SUCCESS),
        }
    }
}

/// Where paged outputs are saved: `$XDG_CACHE_HOME/ai/views`, else `~/.cache/ai/views`.
pub fn cache() -> Result<PathBuf> {
    let base = match env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(d) if d.is_absolute() => d,
        _ => env::home_dir()
            .context("No home directory to keep paged output in.")?
            .join(".cache"),
    };
    Ok(base.join("ai").join("views"))
}

/// `text` in parts of at most `max` bytes, each cut after its last line end when that keeps
/// at least half of it, else at a character boundary.
pub fn pages(text: &str, max: u64) -> Vec<&str> {
    let max = max.max(1) as usize;
    let (mut out, mut rest) = (Vec::new(), text);
    while rest.len() > max {
        let cut = rest.floor_char_boundary(max);
        let at = match rest[..cut].rfind('\n') {
            Some(k) if k + 1 >= cut / 2 => k + 1,
            _ if cut == 0 => rest.ceil_char_boundary(1),
            _ => cut,
        };
        out.push(&rest[..at]);
        rest = &rest[at..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest);
    }
    out
}

/// Prints `text`: whole when `whole` or when it fits one part, else in [`PART`]-byte parts,
/// each headed by `command`: the first here, all of them saved in `views` for [`PAGE`].
pub fn print(
    out: &mut dyn Write,
    views: &Path,
    command: &str,
    text: &str,
    whole: bool,
) -> Result<()> {
    let parts = match whole {
        true => vec![text],
        false => pages(text, PART),
    };
    let n = parts.len();
    if n == 1 {
        writeln!(out, "{}", text.strip_suffix('\n').unwrap_or(text))?;
        return Ok(());
    }
    let parts = parts
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let body = p.strip_suffix('\n').unwrap_or(p);
            format!("{}\n{body}\n", header(command, k + 1, n, ""))
        })
        .collect();
    save_first(out, views, parts)
}

/// The parts of a search's matches, given each match's size, oldest first: part 1 the newest
/// that fit in [`PART`] bytes (at least one), each later part the newest of those left.
pub fn newest_first(sizes: &[u64]) -> Vec<Range<usize>> {
    let (mut parts, mut end) = (Vec::new(), sizes.len());
    while end > 0 {
        let (mut start, mut size) = (end - 1, sizes[end - 1]);
        while start > 0 && size + sizes[start - 1] <= PART {
            start -= 1;
            size += sizes[start];
        }
        parts.push(start..end);
        end = start;
    }
    parts
}

/// Prints a search's matches, given each kept match's size, oldest first, of `hits`, and
/// `render` giving the lines of those in a range: all of them then how many, when `whole` or
/// when they fit one part; else in parts newest first ([`newest_first`]), each headed by
/// `command` and how many it holds: the first here, all of them saved in `views`.
pub fn search(
    out: &mut dyn Write,
    views: &Path,
    command: &str,
    sizes: &[u64],
    hits: u64,
    whole: bool,
    render: impl Fn(Range<usize>) -> String,
) -> Result<()> {
    let kept = sizes.len() as u64;
    let parts = match whole {
        true => Vec::new(),
        false => newest_first(sizes),
    };
    if parts.len() < 2 {
        let all = match kept < hits {
            true => format!("Newest {kept} of {hits} matches."),
            false if hits == 1 => "1 match.".into(),
            false => format!("{hits} matches."),
        };
        writeln!(out, "{}{all}", render(0..sizes.len()))?;
        return Ok(());
    }
    let of = match kept < hits {
        true => format!("the newest {kept} of {hits} matches"),
        false => format!("{hits} matches"),
    };
    let n = parts.len();
    let parts = parts
        .into_iter()
        .enumerate()
        .map(|(k, r)| {
            let detail = format!(", newest first: {} of {of}", r.len());
            format!("{}\n{}", header(command, k + 1, n, &detail), render(r))
        })
        .collect();
    save_first(out, views, parts)
}

/// The line that heads part `k` of `n` of `command`'s output, with `detail` on it.
fn header(command: &str, k: usize, n: usize, detail: &str) -> String {
    format!("{command}, part {k} of {n}{detail}.")
}

/// Saves `parts` as a new view in `views` and prints the first.
fn save_first(out: &mut dyn Write, views: &Path, parts: Vec<String>) -> Result<()> {
    let v = save(views, &parts)?;
    put(out, &parts, v, 1)
}

/// Part `k` of view `v`'s `parts`, then, while parts remain, the command for the next.
fn put(out: &mut dyn Write, parts: &[String], v: u64, k: usize) -> Result<()> {
    out.write_all(parts[k - 1].as_bytes())?;
    if k < parts.len() {
        writeln!(out, "Next: {PAGE} {v}:{}", k + 1)?;
    }
    Ok(())
}

/// Whether a view last read at `m`'s time has expired.
fn expired(m: &fs::Metadata) -> bool {
    m.modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > LIFE)
}

/// Saves `parts` in `views`, private to the user, as the view with the lowest free number,
/// after deleting the expired ones; its number.
pub fn save(views: &Path, parts: &[String]) -> Result<u64> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(views)
        .at(views)?;
    let mut taken = BTreeSet::new();
    for e in fs::read_dir(views).at(views)? {
        let e = e.at(views)?;
        let Ok(m) = e.metadata() else {
            continue;
        };
        if expired(&m) {
            match fs::remove_file(e.path()) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => {
                    return Err(err).at(&e.path());
                }
                _ => continue,
            }
        }
        let name = e.file_name();
        if let Some(v) = name.to_str().and_then(|n| n.strip_suffix(".json")) {
            taken.extend(v.parse::<u64>());
        }
    }
    let mut file = NamedTempFile::new_in(views).at(views)?;
    serde_json::to_writer(&mut file, parts)?;
    file.flush().at(file.path())?;
    let mut v = 1;
    loop {
        while taken.contains(&v) {
            v += 1;
        }
        let p = views.join(format!("{v}.json"));
        match file.persist_noclobber(&p) {
            Ok(_) => return Ok(v),
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {
                taken.insert(v);
                file = e.file;
            }
            Err(e) => return Err(e.error).at(&p),
        }
    }
}

/// Prints the part `page` names, `<view>:<part>`, of a view saved in `views`, which then lasts
/// another [`LIFE`].
pub fn show(views: &Path, page: &str, out: &mut dyn Write) -> Result<()> {
    let bad = || anyhow!("'{page}' is not a page. Copy it as printed, like 17:2.");
    let (v, k) = page.split_once(':').ok_or_else(bad)?;
    let (v, k): (u64, usize) = (v.parse().map_err(|_| bad())?, k.parse().map_err(|_| bad())?);
    let gone = || anyhow!("View {v} is gone: run the command again.");
    let p = views.join(format!("{v}.json"));
    let f = match File::options().read(true).write(true).open(&p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(gone()),
        f => f.at(&p)?,
    };
    if expired(&f.metadata().at(&p)?) {
        return Err(gone());
    }
    let parts: Vec<String> = serde_json::from_reader(io::BufReader::new(&f))
        .with_context(|| format!("View {v} is unreadable: run the command again."))?;
    f.set_modified(SystemTime::now()).at(&p)?;
    let n = parts.len();
    if k == 0 || k > n {
        bail!("View {v} has no part {k}: it has {n}.");
    }
    put(out, &parts, v, k)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    fn printed(f: impl FnOnce(&mut Vec<u8>) -> Result<()>) -> String {
        let mut out = Vec::new();
        f(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn page(views: &Path, page: &str) -> Result<String> {
        let mut out = Vec::new();
        show(views, page, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    /// Makes view `v` in `views` last read `ago`.
    fn read_ago(views: &Path, v: u64, ago: Duration) {
        File::options()
            .write(true)
            .open(views.join(format!("{v}.json")))
            .unwrap()
            .set_modified(SystemTime::now() - ago)
            .unwrap();
    }

    #[test]
    fn searches_page_the_newest_first() {
        assert_eq!(newest_first(&[]), Vec::<Range<usize>>::new());
        assert_eq!(newest_first(&[5, 15_000, 4_000, 3_000]), [2..4, 0..2]);
        assert_eq!(newest_first(&[30_000, 1]), [1..2, 0..1]);
        let tmp = TempDir::new().unwrap();
        let views = tmp.path();
        let render = |r: Range<usize>| r.map(|k| format!("m{k}\n")).collect::<String>();
        let search = |sizes: &[u64], hits, whole| {
            printed(|o| super::search(o, views, "ai s", sizes, hits, whole, render))
        };
        assert_eq!(search(&[1, 2], 2, false), "m0\nm1\n2 matches.\n");
        assert_eq!(search(&[1], 1, false), "m0\n1 match.\n");
        assert_eq!(search(&[1, 2], 9, true), "m0\nm1\nNewest 2 of 9 matches.\n");
        let sizes = [15_000, 15_000, 15_000];
        assert_eq!(
            search(&sizes, 3, true),
            "m0\nm1\nm2\n3 matches.\n",
            "whole saves nothing"
        );
        assert_eq!(fs::read_dir(views).unwrap().count(), 0);
        assert_eq!(
            search(&sizes, 5, false),
            "ai s, part 1 of 3, newest first: 1 of the newest 3 of 5 matches.\nm2\nNext: ai page 1:2\n"
        );
        assert_eq!(
            page(views, "1:3").unwrap(),
            "ai s, part 3 of 3, newest first: 1 of the newest 3 of 5 matches.\nm0\n"
        );
    }

    #[test]
    fn pages_cut_at_line_ends_and_characters() {
        assert_eq!(pages("abc", 10), ["abc"]);
        assert_eq!(pages("ab\ncdef\ngh", 8), ["ab\ncdef\n", "gh"]);
        assert_eq!(pages("a\nbcdefgh", 6), ["a\nbcde", "fgh"]);
        assert_eq!(pages("ééé", 3), ["é", "é", "é"]);
        assert_eq!(pages("ééé", 1), ["é", "é", "é"]);
    }

    #[test]
    fn parts_are_saved_and_read_by_their_page() {
        let tmp = TempDir::new().unwrap();
        let views = &tmp.path().join("views");
        let text = format!("{}\n{}\n", "a".repeat(15_000), "b".repeat(15_000));
        let print = |whole| printed(|o| super::print(o, views, "ai x", &text, whole));
        assert_eq!(print(true), text);
        assert!(!views.exists(), "whole saves nothing");
        let first = print(false);
        assert!(first.starts_with("ai x, part 1 of 2.\naaa"), "{first}");
        assert!(first.ends_with("a\nNext: ai page 1:2\n"), "{first}");
        let second = page(views, "1:2").unwrap();
        assert_eq!(
            second,
            format!("ai x, part 2 of 2.\n{}\n", "b".repeat(15_000))
        );
        assert_eq!(
            page(views, "1:1").unwrap(),
            first,
            "a part reads again as printed"
        );
        assert_eq!(
            fs::metadata(views).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(
            print(false).ends_with("Next: ai page 2:2\n"),
            "a new number"
        );
        let short = |whole| printed(|o| super::print(o, views, "ai x", "x\n", whole));
        assert_eq!((short(true), short(false)), ("x\n".into(), "x\n".into()));
        for (p, e) in [
            ("1:3", "View 1 has no part 3: it has 2."),
            ("1:0", "View 1 has no part 0: it has 2."),
            ("9:1", "View 9 is gone: run the command again."),
            ("1", "'1' is not a page. Copy it as printed, like 17:2."),
            ("x:1", "'x:1' is not a page. Copy it as printed, like 17:2."),
        ] {
            assert_eq!(page(views, p).unwrap_err().to_string(), e);
        }
        let none = tmp.path().join("none");
        assert_eq!(
            page(&none, "1:2").unwrap_err().to_string(),
            "View 1 is gone: run the command again."
        );
    }

    #[test]
    fn views_expire_unread_and_free_their_numbers() {
        let tmp = TempDir::new().unwrap();
        let views = tmp.path();
        let parts = |s: &str| vec![format!("{s} 1\n"), format!("{s} 2\n")];
        assert_eq!(save(views, &parts("a")).unwrap(), 1);
        assert_eq!(save(views, &parts("b")).unwrap(), 2);
        assert_eq!(save(views, &parts("c")).unwrap(), 3);
        read_ago(views, 1, LIFE + Duration::from_secs(1));
        assert_eq!(
            page(views, "1:2").unwrap_err().to_string(),
            "View 1 is gone: run the command again.",
            "expired before it is deleted"
        );
        read_ago(views, 2, LIFE - Duration::from_secs(5));
        assert_eq!(page(views, "2:1").unwrap(), "b 1\nNext: ai page 2:2\n");
        read_ago(views, 3, LIFE + Duration::from_secs(1));
        assert_eq!(
            save(views, &parts("d")).unwrap(),
            1,
            "the lowest free number"
        );
        assert_eq!(page(views, "1:1").unwrap(), "d 1\nNext: ai page 1:2\n");
        assert_eq!(page(views, "2:2").unwrap(), "b 2\n", "reading kept it");
        assert_eq!(save(views, &parts("e")).unwrap(), 3);
        assert_eq!(save(views, &parts("f")).unwrap(), 4);
        let mut names: Vec<_> = fs::read_dir(views)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["1.json", "2.json", "3.json", "4.json"]);
    }

    #[test]
    fn sessions_saving_at_once_get_numbers_of_their_own() {
        let tmp = TempDir::new().unwrap();
        let views = tmp.path();
        let mut got: Vec<u64> = std::thread::scope(|s| {
            let saves: Vec<_> = (0..8)
                .map(|k| s.spawn(move || save(views, &[format!("{k}\n"), "x\n".into()]).unwrap()))
                .collect();
            saves.into_iter().map(|h| h.join().unwrap()).collect()
        });
        got.sort();
        assert_eq!(got, (1..=8).collect::<Vec<_>>());
    }
}
