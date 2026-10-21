use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{file, jsonl, memory::Who};

/// The file summary store format this ai reads and writes.
pub const FORMAT: u32 = 1;
const SUMMARIES: &str = "summaries.jsonl";

/// The summary of the node whose hash is `key`: a file's content, or a directory's children.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub key: String,
    pub ts: Timestamp,
    pub who: Who,
    pub text: String,
}

/// One project's file and directory summaries, by hash: `<dir>/summaries.jsonl`.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The store in `dir`; a store of another format is refused.
    pub fn open(dir: &Path) -> Result<Store> {
        file::check_format(dir, FORMAT, "file summary")?;
        Ok(Store { dir: dir.into() })
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every summary, by key; a key written twice keeps its first.
    pub fn load(&self) -> Result<HashMap<String, Summary>> {
        let p = self.dir.join(SUMMARIES);
        let rows: Vec<Summary> = jsonl::decode_all(&jsonl::read(&p)?)
            .with_context(|| format!("The file summaries are corrupt: {}", file::pretty(&p)))?;
        let mut all = HashMap::with_capacity(rows.len());
        for s in rows {
            all.entry(s.key.clone()).or_insert(s);
        }
        Ok(all)
    }

    /// Writes the summary of `key` unless it is stored; returns the one stored.
    pub fn put(&self, key: &str, who: &Who, text: &str) -> Result<Summary> {
        file::ensure_format(&self.dir, FORMAT)?;
        let _w = file::lock(&self.dir.join("summaries.lock"))?;
        if let Some(s) = self.load()?.remove(key) {
            return Ok(s);
        }
        let s = Summary {
            key: key.into(),
            ts: Timestamp::now(),
            who: who.clone(),
            text: text.into(),
        };
        jsonl::append(&self.dir.join(SUMMARIES), std::slice::from_ref(&s))?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn who(model: &str) -> Who {
        Who {
            agent: "claude".into(),
            model: model.into(),
            session: "s-1".into(),
        }
    }

    #[test]
    fn a_summary_is_found_by_its_key_and_kept_by_the_first_write() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("p");
        let s = Store::open(&dir).unwrap();
        assert!(s.load().unwrap().is_empty());
        assert!(!dir.exists());
        let a = s.put("k1", &who("sonnet"), "first").unwrap();
        let again = s.put("k1", &who("opus"), "second").unwrap();
        assert_eq!(again, a);
        s.put("k2", &who("sonnet"), "other\nlines").unwrap();
        let all = Store::open(&dir).unwrap().load().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["k1"], a);
        assert_eq!(all["k2"].text, "other\nlines");
        assert_eq!(fs::read_to_string(dir.join("format")).unwrap(), "1\n");
        let rows = fs::read_to_string(dir.join(SUMMARIES)).unwrap();
        assert_eq!(rows.lines().count(), 2);
        assert!(rows.starts_with(r#"{"key":"k1","ts":"#));
        assert!(rows.contains(r#""who":{"agent":"claude","model":"sonnet","session":"s-1"}"#));
    }

    #[test]
    fn a_store_of_another_format_is_refused() {
        let tmp = TempDir::new().unwrap();
        for (v, want) in [
            (
                "2\n",
                "This file summary store is format 2; this ai reads up to 1. Upgrade ai.",
            ),
            (
                "0\n",
                "This file summary store is format 0, which this ai does not read.",
            ),
            ("x\n", "The file summary store's format file is corrupt."),
        ] {
            fs::write(tmp.path().join("format"), v).unwrap();
            let e = Store::open(tmp.path()).map(|_| ()).unwrap_err();
            assert_eq!(e.to_string(), want);
        }
    }

    #[test]
    fn a_corrupt_row_is_an_error_naming_the_file_and_line() {
        let tmp = TempDir::new().unwrap();
        let s = Store::open(tmp.path()).unwrap();
        s.put("k1", &who("sonnet"), "fine").unwrap();
        let p = tmp.path().join(SUMMARIES);
        let mut b = fs::read(&p).unwrap();
        b.extend_from_slice(b"{\"key\":\"k2\"}\n");
        fs::write(&p, b).unwrap();
        let e = format!("{:#}", s.load().unwrap_err());
        let why = "line 2 is corrupt: missing field `ts` at line 1 column 12";
        assert_eq!(
            e,
            format!(
                "The file summaries are corrupt: {}: {why}",
                file::pretty(&p)
            )
        );
    }
}
