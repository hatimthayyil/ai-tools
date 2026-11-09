//! The commands a setting names to print a secret, a password or an API key: each run by
//! `sh -c` with no input, and ended if it takes too long, so a command that waits for someone to
//! answer it can't hold up everything after it.

use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

/// How long a command may take: enough for a key store to answer, or for a touch of a hardware
/// key, and short enough that a run with no one there fails soon.
pub const LIMIT: Duration = Duration::from_secs(60);

/// What the command of the setting `key` printed, as text, if it ended well within `limit`.
/// Its errors name the setting and the command, never what it printed: that is a secret.
pub fn printed(key: &str, command: &str, limit: Duration) -> Result<String> {
    let says = || format!("`{key} = {command:?}`");
    let mut child = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .with_context(|| format!("Cannot run {}.", says()))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(read);
    });
    let deadline = Instant::now() + limit;
    let slow = || {
        anyhow!(
            "{} did not end within {} seconds, so it was stopped. It must print without \
             waiting for anyone to answer it.",
            says(),
            limit.as_secs()
        )
    };
    // The output ends once the command and everything it started have let go of it.
    let Ok(read) = rx.recv_timeout(limit) else {
        end(&mut child);
        return Err(slow());
    };
    let status = loop {
        if let Some(s) = child.try_wait().context("Cannot wait for a command.")? {
            break s;
        }
        if Instant::now() >= deadline {
            end(&mut child);
            return Err(slow());
        }
        thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        bail!("{} failed: {status}.", says());
    }
    let bytes = read.with_context(|| format!("Cannot read what {} printed.", says()))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("{} printed something that is not text.", says()))
}

/// Kills `child` and everything it started in its process group, and reaps it.
fn end(child: &mut Child) {
    // SAFETY: killpg takes a process group id and a signal; the group is the child's own, made
    // at its spawn, and the child is not reaped yet, so the id still names it.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_prints_its_secret() {
        let out = printed("[backup] password-command", "printf 'a b\\n'", LIMIT).unwrap();
        assert_eq!(out, "a b\n");
    }

    #[test]
    fn a_failing_command_is_named_without_what_it_printed() {
        let e = printed("[backup] password-command", "echo hunter2; exit 7", LIMIT)
            .unwrap_err()
            .to_string();
        assert!(e.contains("`[backup] password-command = \"echo hunter2; exit 7\"` failed"));
        assert!(e.contains("exit status: 7"), "{e}");
    }

    #[test]
    fn a_command_that_hangs_is_stopped_with_what_it_started() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mark = tmp.path().join("alive");
        let command = format!("(sleep 2; touch '{}') & sleep 30", mark.display());
        let began = Instant::now();
        let e = printed(
            "[backup.remote] env-command",
            &command,
            Duration::from_millis(200),
        )
        .unwrap_err()
        .to_string();
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
        assert!(e.contains("did not end within 0 seconds"), "{e}");
        thread::sleep(Duration::from_millis(2300));
        assert!(!mark.exists(), "what the command started outlived it");
    }

    #[test]
    fn a_command_that_ends_but_leaves_its_output_open_is_stopped() {
        let began = Instant::now();
        let e = printed("k", "sleep 30 & echo x", Duration::from_millis(200)).unwrap_err();
        assert!(began.elapsed() < Duration::from_secs(2));
        assert!(e.to_string().contains("did not end within"), "{e}");
    }
}
