use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};

use crate::file::AtPath;

/// The complete lines of `p`, each ending in a newline: an incomplete last line is left out.
/// Empty if `p` does not exist.
pub fn read(p: &Path) -> Result<Vec<u8>> {
    let mut b = match fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).at(p),
    };
    b.truncate(complete(&b));
    Ok(b)
}

/// The length of the complete lines at the start of `b`.
pub fn complete(b: &[u8]) -> usize {
    memchr::memrchr(b'\n', b).map_or(0, |i| i + 1)
}

/// The lines of `b`, complete lines as [`read`] returns them, without their newlines.
pub fn lines(b: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = b;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (line, next) = match memchr::memchr(b'\n', rest) {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        rest = next;
        Some(line)
    })
}

/// The number of lines in `b`.
pub fn count(b: &[u8]) -> u64 {
    memchr::memchr_iter(b'\n', b).count() as u64
}

/// One line, decoded.
pub fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(line)?)
}

/// Every line of `b`, decoded; an error names the line (from 1).
pub fn decode_all<T: DeserializeOwned>(b: &[u8]) -> Result<Vec<T>> {
    lines(b)
        .enumerate()
        .map(|(i, l)| decode(l).with_context(|| format!("line {} is corrupt", i + 1)))
        .collect()
}

/// `rows`, one line each.
pub fn encode<T: Serialize>(rows: &[T]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut buf, r)?;
        buf.push(b'\n');
    }
    Ok(buf)
}

/// Appends `rows` to `p`, one line each, first dropping an incomplete last line a crash left.
/// The caller holds the file's writer lock.
pub fn append<T: Serialize>(p: &Path, rows: &[T]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let buf = encode(rows)?;
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(p)
        .at(p)?;
    let len = f.metadata().at(p)?.len();
    if len > 0 {
        let mut last = [0];
        f.seek(SeekFrom::Start(len - 1)).at(p)?;
        f.read_exact(&mut last).at(p)?;
        if last[0] != b'\n' {
            let mut all = Vec::new();
            f.seek(SeekFrom::Start(0)).at(p)?;
            f.read_to_end(&mut all).at(p)?;
            f.set_len(complete(&all) as u64).at(p)?;
        }
    }
    f.write_all(&buf).at(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_incomplete_last_line_is_unread_and_dropped_before_appending() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("x.jsonl");
        assert!(read(&p).unwrap().is_empty());
        append(&p, &[1, 2]).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"{\"torn")
            .unwrap();
        assert_eq!(read(&p).unwrap(), b"1\n2\n");
        append(&p, &[3]).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"1\n2\n3\n");
        let b = read(&p).unwrap();
        assert_eq!(count(&b), 3);
        assert_eq!(decode_all::<u32>(&b).unwrap(), [1, 2, 3]);
        assert_eq!(lines(b"").count(), 0);
        let e = decode_all::<u32>(b"1\nx\n").unwrap_err();
        assert_eq!(e.to_string(), "line 2 is corrupt");
    }
}
