use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::{
    file::{AtPath, pretty},
    jsonl,
};

/// The size past which a segment takes no new line.
pub const CAP: usize = 1 << 20;

/// The name of segment `i` of the segmented file `p`: six digits and `p`'s extension.
fn name(p: &Path, i: usize) -> OsString {
    match p.extension() {
        Some(ext) => format!("{i:06}.{}", ext.to_string_lossy()).into(),
        None => format!("{i:06}").into(),
    }
}

/// The segments of `p`, in order; none if `p` does not exist.
fn segments(p: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(p) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) if p.is_file() => bail!("{} is a whole file, not segments.", pretty(p)),
        Err(e) => return Err(e).at(p),
    };
    let mut found = Vec::new();
    for e in entries {
        let e = e.at(p)?;
        if !e.file_type().at(p)?.is_file() {
            bail!("{} is corrupt: it holds a directory.", pretty(p));
        }
        found.push(e.file_name());
    }
    found.sort();
    let mut want: Vec<OsString> = (0..found.len()).map(|i| name(p, i)).collect();
    want.sort();
    if found != want {
        bail!(
            "{} is corrupt: its segments are not numbered from 0.",
            pretty(p)
        );
    }
    Ok((0..found.len()).map(|i| p.join(name(p, i))).collect())
}

/// Whether `p` is a segmented file: a directory holding files.
pub fn is_segmented(p: &Path) -> bool {
    fs::read_dir(p).is_ok_and(|mut d| d.any(|e| e.is_ok_and(|e| e.path().is_file())))
}

/// Every byte of `p`, its segments joined in order; empty if `p` does not exist.
pub fn read_all(p: &Path) -> Result<Vec<u8>> {
    let segs = segments(p)?;
    let mut out = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        let b = fs::read(s).at(s)?;
        if i + 1 < segs.len() && b.last() != Some(&b'\n') {
            bail!("{} is corrupt: segment {i} ends inside a line.", pretty(p));
        }
        out.extend(b);
    }
    Ok(out)
}

/// The complete lines of `p`, as [`jsonl::read`] gives a whole file's.
pub fn read(p: &Path) -> Result<Vec<u8>> {
    let mut b = read_all(p)?;
    b.truncate(jsonl::complete(&b));
    Ok(b)
}

/// Appends `bytes` to `p`, made with an empty first segment if it does not exist.
pub fn extend(p: &Path, bytes: &[u8]) -> Result<()> {
    extend_to(p, bytes, CAP)
}

/// [`extend`] with segments of `cap` bytes: a line goes to the last segment, unless that one ends
/// a line and the line would take it past `cap`; then it starts the next. So a segment is cut only
/// at a line's end, and a line longer than `cap` gets a segment to itself.
fn extend_to(p: &Path, bytes: &[u8], cap: usize) -> Result<()> {
    let segs = segments(p)?;
    if segs.is_empty() {
        fs::create_dir_all(p).at(p)?;
    }
    let (mut len, mut ends) = match segs.last() {
        Some(s) => tail(s)?,
        None => (0, true),
    };
    let mut i = segs.len().saturating_sub(1);
    let mut buf = Vec::new();
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        if len > 0 && ends && len + line.len() > cap {
            put(p, i, &buf)?;
            buf.clear();
            (i, len) = (i + 1, 0);
        }
        buf.extend_from_slice(line);
        len += line.len();
        ends = line.ends_with(b"\n");
    }
    put(p, i, &buf)
}

/// Segment `s`'s length, and whether it ends a line (an empty one does).
fn tail(s: &Path) -> Result<(usize, bool)> {
    let mut f = File::open(s).at(s)?;
    let len = f.metadata().at(s)?.len();
    if len == 0 {
        return Ok((0, true));
    }
    let mut last = [0];
    f.seek(SeekFrom::Start(len - 1)).at(s)?;
    f.read_exact(&mut last).at(s)?;
    Ok((len as usize, last[0] == b'\n'))
}

/// Appends `bytes` to segment `i` of `p`, making it if absent.
fn put(p: &Path, i: usize, bytes: &[u8]) -> Result<()> {
    let s = p.join(name(p, i));
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&s)
        .and_then(|mut f| f.write_all(bytes))
        .at(&s)
}

/// Appends `rows` to `p`, one line each, first dropping an incomplete last line a crash left.
/// The caller holds the file's writer lock.
pub fn append<T: Serialize>(p: &Path, rows: &[T]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    if let Some(last) = segments(p)?.last()
        && !tail(last)?.1
    {
        let b = fs::read(last).at(last)?;
        OpenOptions::new()
            .write(true)
            .open(last)
            .and_then(|f| f.set_len(jsonl::complete(&b) as u64))
            .at(last)?;
    }
    extend(p, &jsonl::encode(rows)?)
}

/// Makes `p` hold `bytes` alone, in new segments.
pub fn write(p: &Path, bytes: &[u8]) -> Result<()> {
    match fs::remove_dir_all(p) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).at(p),
    }
    extend(p, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The segments of `p` as text, in order.
    fn parts(p: &Path) -> Vec<String> {
        segments(p)
            .unwrap()
            .iter()
            .map(|s| fs::read_to_string(s).unwrap())
            .collect()
    }

    #[test]
    fn a_segment_takes_lines_until_the_next_would_pass_the_cap() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.jsonl");
        extend_to(&p, b"aaa\nbbb\n", 10).unwrap();
        extend_to(&p, b"cc\nddd\n", 10).unwrap();
        assert_eq!(parts(&p), ["aaa\nbbb\n", "cc\nddd\n"]);
        let names: Vec<_> = segments(&p)
            .unwrap()
            .into_iter()
            .map(|s| s.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["000000.jsonl", "000001.jsonl"]);
        assert_eq!(read_all(&p).unwrap(), b"aaa\nbbb\ncc\nddd\n");
    }

    #[test]
    fn a_line_over_the_cap_gets_a_segment_to_itself() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x");
        extend_to(&p, b"a\nbbbbbbbbbbbbbbb\nc\n", 4).unwrap();
        assert_eq!(parts(&p), ["a\n", "bbbbbbbbbbbbbbb\n", "c\n"]);
        assert_eq!(segments(&p).unwrap()[0].file_name().unwrap(), "000000");
    }

    #[test]
    fn an_unfinished_line_keeps_its_segment_open_until_it_ends() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.jsonl");
        extend_to(&p, b"aaaa\nbb", 10).unwrap();
        extend_to(&p, b"bbbbbbbb\nc\n", 10).unwrap();
        assert_eq!(parts(&p), ["aaaa\nbbbbbbbbbb\n", "c\n"]);
    }

    #[test]
    fn joined_segments_give_back_every_byte() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.bin");
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 256) as u8).collect();
        let mut at = 0;
        for n in [1, 13, 700, 0, 2000, 2286] {
            extend_to(&p, &bytes[at..at + n], 64).unwrap();
            at += n;
        }
        assert_eq!(at, bytes.len());
        assert!(segments(&p).unwrap().len() > 1);
        assert_eq!(read_all(&p).unwrap(), bytes);
        write(&p, b"new\n").unwrap();
        assert_eq!(parts(&p), ["new\n"]);
    }

    #[test]
    fn an_empty_file_is_one_empty_segment() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x");
        assert!(read_all(&p).unwrap().is_empty());
        assert!(!is_segmented(&p));
        extend(&p, b"").unwrap();
        assert_eq!(parts(&p), [""]);
        assert!(is_segmented(&p));
    }

    #[test]
    fn appending_drops_an_incomplete_last_line_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.jsonl");
        append(&p, &[1, 2]).unwrap();
        extend(&p, b"{\"torn").unwrap();
        assert_eq!(read(&p).unwrap(), b"1\n2\n");
        append(&p, &[3]).unwrap();
        assert_eq!(read_all(&p).unwrap(), b"1\n2\n3\n");
    }

    #[test]
    fn anything_but_numbered_segments_is_corrupt() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.jsonl");
        extend_to(&p, b"a\nb\n", 2).unwrap();
        fs::remove_file(p.join("000000.jsonl")).unwrap();
        let e = read_all(&p).unwrap_err().to_string();
        assert!(
            e.ends_with("is corrupt: its segments are not numbered from 0."),
            "{e}"
        );

        let whole = dir.path().join("whole.jsonl");
        fs::write(&whole, "1\n").unwrap();
        let e = read_all(&whole).unwrap_err().to_string();
        assert!(
            e.ends_with("whole.jsonl is a whole file, not segments."),
            "{e}"
        );

        let q = dir.path().join("q");
        extend_to(&q, b"a\nb\n", 2).unwrap();
        fs::write(q.join("000000"), "a").unwrap();
        let e = read_all(&q).unwrap_err().to_string();
        assert!(
            e.ends_with("is corrupt: segment 0 ends inside a line."),
            "{e}"
        );
    }
}
