use serde_json::Value;

use crate::{
    chat::{
        id::Id,
        model::{Item, Kind, Marker},
    },
    memory::{flat, plural},
};

/// No cut.
pub const WHOLE: usize = usize::MAX;

/// How much of an item a line shows, in bytes: of its words, of a tool call's input, and of a
/// tool output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    pub words: usize,
    /// A cut input is on one line.
    pub input: usize,
    /// `None` shows only the output's size.
    pub output: Option<usize>,
}

impl Cut {
    pub const WHOLE: Cut = Cut {
        words: WHOLE,
        input: WHOLE,
        output: Some(WHOLE),
    };
}

/// Item `id` as `id|kind: text`, cut by `cut`; its text continues on lines indented by two
/// spaces. A tool call's input is `key=value` per field, strings verbatim.
pub fn item_line(id: Id, item: &Item, cut: Cut) -> String {
    format!("{id}|{}", item_text(item, cut))
}

/// An item as `kind: text`, cut by `cut`: [`item_line`] without its id.
pub fn item_text(item: &Item, cut: Cut) -> String {
    let text = match &item.kind {
        Kind::User(t) | Kind::Text(t) | Kind::Work { text: t, .. } => clip(t, cut.words),
        Kind::ToolCall { input, .. } => match cut.input {
            WHOLE => args(input),
            max => clip(&flat(&args(input)), max),
        },
        Kind::ToolResult { output, .. } => match cut.output {
            Some(max) => clip(output, max),
            None => size(output),
        },
        Kind::Marker(m) => {
            let what = match m {
                Marker::Compaction => "the harness compacted its context here",
                Marker::Summary => "the harness's summary of the context it compacted",
                Marker::Interrupt => "interrupted by the user",
            };
            return format!("[{what}]");
        }
    };
    let text: Vec<&str> = text.trim_end().lines().collect();
    format!("{}: {}", label(&item.kind), text.join("\n  "))
}

/// An item's kind and whole text, as grep searches it: a tool output only with `output`, a
/// marker never.
pub fn searched(item: &Item, output: bool) -> Option<(String, String)> {
    let text = match &item.kind {
        Kind::User(t) | Kind::Text(t) | Kind::Work { text: t, .. } => t.clone(),
        Kind::ToolCall { input, .. } => args(input),
        Kind::ToolResult { output: o, .. } if output => o.clone(),
        Kind::ToolResult { .. } | Kind::Marker(_) => return None,
    };
    Some((label(&item.kind), text))
}

/// An item's kind as printed before its text.
fn label(kind: &Kind) -> String {
    match kind {
        Kind::User(_) => "user".into(),
        Kind::Work { from, .. } => format!("agent {from}"),
        Kind::Text(_) => "ai".into(),
        Kind::ToolCall { name, .. } => format!("tool {name}"),
        Kind::ToolResult { is_error: true, .. } => "error".into(),
        Kind::ToolResult { .. } => "output".into(),
        Kind::Marker(_) => "marker".into(),
    }
}

/// A tool output's size, in place of its text.
fn size(output: &str) -> String {
    match output.trim_end() {
        "" => "(empty)".into(),
        o => format!(
            "({}, {} bytes)",
            plural(o.lines().count() as u64, "line"),
            output.len()
        ),
    }
}

/// A tool call's input: `key=value` per field, strings verbatim.
fn args(v: &Value) -> String {
    let field = |v: &Value| match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    match v {
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| format!("{k}={}", field(v)))
            .collect::<Vec<_>>()
            .join(" "),
        Value::Null => String::new(),
        v => field(v),
    }
}

/// `s` in about `max` bytes: its head and tail around a note on how much was cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.into();
    }
    let head = &s[..s.floor_char_boundary(max / 2)];
    let tail = &s[s.ceil_char_boundary(s.len() - max / 2)..];
    let cut = s.len() - head.len() - tail.len();
    format!("{head} [{cut} bytes cut] {tail}")
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use serde_json::json;

    use super::*;
    use crate::chat::model::SourceId;

    #[test]
    fn clips_keep_head_and_tail() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("short", WHOLE), "short");
        let s = format!("{}{}", "a".repeat(10), "b".repeat(10));
        assert_eq!(clip(&s, 8), "aaaa [12 bytes cut] bbbb");
        let c = clip(&"é".repeat(10), 7);
        assert!(c.starts_with("é [") && c.ends_with("] é"), "{c}");
    }

    fn item(kind: Kind) -> Item {
        Item {
            id: SourceId {
                harness: "test".into(),
                session: "s".into(),
                entry: "e".into(),
            },
            parent: None,
            ts: Timestamp::UNIX_EPOCH,
            kind,
            ends_turn: false,
        }
    }

    const ID: Id = Id::Items {
        turn: 3,
        first: 7,
        n: 1,
    };

    #[test]
    fn items_print_with_their_kind() {
        let cut = Cut {
            words: 100,
            input: 40,
            output: Some(40),
        };
        let r = |k| item_line(ID, &item(k), cut);
        assert_eq!(r(Kind::User("fix it".into())), "3.7|user: fix it");
        assert_eq!(
            r(Kind::Work {
                from: "reviewer".into(),
                text: "done".into()
            }),
            "3.7|agent reviewer: done"
        );
        assert_eq!(r(Kind::Text("ok".into())), "3.7|ai: ok");
        let call = |input| Kind::ToolCall {
            id: "t1".into(),
            name: "Bash".into(),
            input,
        };
        assert_eq!(
            r(call(json!({"command": "cargo test", "timeout": 60}))),
            "3.7|tool Bash: command=cargo test timeout=60"
        );
        assert_eq!(r(call(json!("ls"))), "3.7|tool Bash: ls");
        let edit = r(call(
            json!({"old_string": "x".repeat(50), "new_string": "y"}),
        ));
        assert_eq!(
            edit,
            format!(
                "3.7|tool Bash: new_string=y old_str [34 bytes cut] {}",
                "x".repeat(20)
            )
        );
        let out = format!("{}{}", "h".repeat(30), "t".repeat(30));
        let result = |is_error| {
            r(Kind::ToolResult {
                id: "t1".into(),
                output: out.clone(),
                is_error,
            })
        };
        assert_eq!(
            result(true),
            format!(
                "3.7|error: {} [20 bytes cut] {}",
                "h".repeat(20),
                "t".repeat(20)
            )
        );
        assert!(result(false).starts_with("3.7|output: hhh"));
        assert_eq!(
            item_line(ID, &item(Kind::Text(out.clone())), Cut::WHOLE),
            format!("3.7|ai: {out}")
        );
        assert_eq!(
            r(Kind::Marker(Marker::Interrupt)),
            "3.7|[interrupted by the user]"
        );
    }

    #[test]
    fn text_continues_indented_and_a_cut_input_is_one_line() {
        let r = |k, cut| item_line(ID, &item(k), cut);
        assert_eq!(
            r(Kind::Text("one\n## two\r\n3|three\n".into()), Cut::WHOLE),
            "3.7|ai: one\n  ## two\n  3|three"
        );
        let call = Kind::ToolCall {
            id: "t1".into(),
            name: "Bash".into(),
            input: json!({"command": "cat <<EOF\nhi\nEOF"}),
        };
        assert_eq!(
            r(call.clone(), Cut::WHOLE),
            "3.7|tool Bash: command=cat <<EOF\n  hi\n  EOF"
        );
        let one = Cut {
            input: 400,
            ..Cut::WHOLE
        };
        assert_eq!(r(call, one), "3.7|tool Bash: command=cat <<EOF hi EOF");
    }

    #[test]
    fn a_hidden_output_shows_its_size() {
        let hidden = Cut {
            output: None,
            ..Cut::WHOLE
        };
        let r = |output: &str, is_error| {
            item_line(
                ID,
                &item(Kind::ToolResult {
                    id: "t1".into(),
                    output: output.into(),
                    is_error,
                }),
                hidden,
            )
        };
        assert_eq!(r("a\nbb\n", false), "3.7|output: (2 lines, 5 bytes)");
        assert_eq!(r("oops", true), "3.7|error: (1 line, 4 bytes)");
        assert_eq!(r("", false), "3.7|output: (empty)");
    }
}
