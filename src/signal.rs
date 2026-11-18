//! What a signal that ends `ai` leaves behind.

use std::{io, mem::MaybeUninit, ptr};

/// The signals git removes its lock files on before it dies of them.
const TERMINATING: [libc::c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

/// Has each terminating signal remove the lock and temporary files gix holds (an object or a ref
/// being written) before the signal ends `ai` as it would have, as git's own handlers do: an
/// interrupted commit then leaves no ref lock to refuse every later one. A signal ignored when
/// `ai` starts stays ignored, so `nohup` and a shell's background jobs keep their meaning.
///
/// SIGKILL cannot be caught, and a file gix is writing to at that instant is out of its reach, so
/// a lock can still be left behind; git's error then names it.
pub fn clean_up_on_termination() -> io::Result<()> {
    // With no file to remove yet, this only makes gix's registry, so the handler never does.
    gix::tempfile::registry::cleanup_tempfiles_signal_safe();
    for sig in TERMINATING {
        let mut current = MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: a null new action only reads the current one into `current`.
        if unsafe { libc::sigaction(sig, ptr::null(), current.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: sigaction succeeded, so it filled `current`.
        if unsafe { current.assume_init() }.sa_sigaction == libc::SIG_IGN {
            continue;
        }
        // SAFETY: an all-zero sigaction is a valid one: no flags and an empty mask.
        let mut action: libc::sigaction = unsafe { MaybeUninit::zeroed().assume_init() };
        action.sa_sigaction = clean_up as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: `action` is initialised and `clean_up` does only async-signal-safe work.
        if unsafe { libc::sigaction(sig, &action, ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Removes gix's files, then dies of `sig`: the signal stays blocked while this runs, so the one
/// raised here arrives as it returns, with the default action back in place.
extern "C" fn clean_up(sig: libc::c_int) {
    gix::tempfile::registry::cleanup_tempfiles_signal_safe();
    // SAFETY: signal and raise are async-signal-safe.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}
