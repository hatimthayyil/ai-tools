//! Errors as `ai` reports them: one message that names every cause.

/// `e` followed by every cause under it, outermost first. A cause comes after a space when the
/// message before it ends a sentence, else after `: `; one whose text is already in the message
/// adds nothing and is left out. Each part is written in its alternate form, which for gix's
/// errors leaves out the source location every frame carries. A gix error holding a tree of
/// causes, whose alternate form draws that tree over many lines, gives each error of the tree as
/// a part of its own, in the order gix visits them.
pub fn line(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        match cause.downcast_ref::<gix::Error>() {
            Some(tree) => {
                for part in tree.iter_errors().filter(|e| !e.is::<gix::Error>()) {
                    push(&mut out, &format!("{part:#}"));
                }
            }
            None => push(&mut out, &format!("{cause:#}")),
        }
    }
    out
}

/// Adds `part` to the message `out` as [`line`] does.
fn push(out: &mut String, part: &str) {
    let part = part.trim();
    if part.is_empty() || out.contains(part) {
        return;
    }
    if !out.is_empty() {
        out.push_str(match out.ends_with(['.', '!', '?', ':']) {
            true => " ",
            false => ": ",
        });
    }
    out.push_str(part);
}

#[cfg(test)]
mod tests {
    use std::{fs, io};

    use anyhow::{Context, anyhow};

    use super::*;

    #[test]
    fn every_cause_follows_the_message_above_it() {
        let e = Err::<(), _>(io::Error::other("disk full"))
            .context("Cannot write the index")
            .context("Cannot commit ~/ai")
            .unwrap_err();
        assert_eq!(
            line(&e),
            "Cannot commit ~/ai: Cannot write the index: disk full"
        );
    }

    #[test]
    fn a_cause_after_a_sentence_follows_a_space() {
        let e = Err::<(), _>(anyhow!("No such file."))
            .context("Not a hook input with a prompt.")
            .unwrap_err();
        assert_eq!(line(&e), "Not a hook input with a prompt. No such file.");
    }

    #[test]
    fn a_cause_the_message_already_holds_is_left_out() {
        let e = Err::<(), _>(io::Error::other("File exists"))
            .context("File exists")
            .context("The lock is held: File exists")
            .unwrap_err();
        assert_eq!(line(&e), "The lock is held: File exists");
    }

    #[test]
    fn a_stale_ref_lock_is_told_on_one_line_naming_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut repo = gix::init(tmp.path()).unwrap();
        repo.committer_or_set_fallback("ai", "ai@localhost")
            .unwrap();
        let lock = repo.git_dir().join("refs/heads/x.lock");
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, "").unwrap();
        let tree = gix::ObjectId::empty_tree(repo.object_hash());
        let me = repo.committer().unwrap().unwrap();
        let e = repo
            .commit_as(me, me, "refs/heads/x", "one", tree, None::<gix::ObjectId>)
            .context("Cannot commit")
            .unwrap_err();
        let l = line(&e);
        assert_eq!(l.lines().count(), 1, "{l}");
        assert!(l.starts_with("Cannot commit: "), "{l}");
        assert!(
            l.contains(&format!("The lockfile at '{}'", lock.display())),
            "{l}"
        );
        assert!(!l.contains("Message {"), "no debug forms: {l}");
        assert!(!l.contains(".rs:"), "no source locations: {l}");
    }

    #[test]
    fn an_error_without_causes_is_its_message() {
        assert_eq!(
            line(&anyhow!("Agent dev already exists.")),
            "Agent dev already exists."
        );
    }
}
