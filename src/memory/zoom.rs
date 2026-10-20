use anyhow::Result;

use crate::memory::{
    meta::{Meta, dated},
    store::Snapshot,
    tree::Coord,
    view::HIDDEN,
};

/// Line `id+n` opened two levels down: the four lines of `n/4` under it, its messages' lines
/// for `n` of 2 or 4, or for `n = 1` the message whole; with `meta`, each line gets its head
/// and the lines their date headers. `None` when `id+n` is not a built line.
pub fn zoom(s: &Snapshot, id: u64, n: u64, meta: Option<&Meta>) -> Result<Option<String>> {
    let Some(c) = Coord::at(id, n, s.log_len()?) else {
        return Ok(None);
    };
    if c.l == 0 {
        let m = s.message(id)?;
        let label = m.label();
        return Ok(Some(match meta {
            Some(meta) => dated(&[meta.line(c, std::slice::from_ref(&m), &label)]).join("\n"),
            None => format!("{c}|{label}"),
        }));
    }
    if s.node(c)?.is_none() {
        return Ok(None);
    }
    let l = c.l.saturating_sub(2);
    let mut lines = Vec::new();
    for k in (c.id() >> l..c.end() >> l).map(|i| Coord::new(l, i)) {
        let Some(node) = s.node(k)? else {
            return Ok(None);
        };
        let text = if node.text.is_empty() {
            HIDDEN
        } else {
            &node.text
        };
        lines.push((k, text.to_string()));
    }
    let lines = match meta {
        Some(meta) => {
            let ls = lines
                .iter()
                .map(|(k, text)| meta.line_at(s, *k, text))
                .collect::<Result<Vec<_>>>()?;
            dated(&ls)
        }
        None => lines
            .iter()
            .map(|(k, text)| format!("{k}|{text}"))
            .collect(),
    };
    Ok(Some(lines.join("\n")))
}
