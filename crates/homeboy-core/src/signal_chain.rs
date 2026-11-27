//! Chained POSIX signal handlers.
//!
//! Several homeboy-core subsystems want to observe the same signal: the
//! daemon's graceful-stop SIGTERM handler and the local-exec process-group
//! cleanup handler both watch SIGTERM. Installing either with `signal(2)`
//! silently replaced the other, so a `daemon serve` that spawned one local
//! child stopped honouring SIGTERM (#18468). [`ChainedSignal`] installs a
//! handler with `sigaction(2)`, remembers the disposition it displaced, and
//! lets the new handler forward to it, so installation order does not
//! matter.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// `SA_SIGINFO`-style handler signature used by every chained handler.
pub(crate) type SigInfoHandler =
    extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void);

/// One chained installation: a single (signal, handler) pair and the
/// disposition it displaced.
pub(crate) struct ChainedSignal {
    installed: AtomicBool,
    previous: AtomicUsize,
    previous_siginfo: AtomicBool,
}

impl ChainedSignal {
    pub(crate) const fn new() -> Self {
        Self {
            installed: AtomicBool::new(false),
            previous: AtomicUsize::new(libc::SIG_DFL),
            previous_siginfo: AtomicBool::new(false),
        }
    }

    /// Install `handler` for `signal`, saving the current disposition so
    /// [`ChainedSignal::forward`] can invoke it. Installation happens at most
    /// once per `ChainedSignal`: a repeat call is a no-op because the handler
    /// is already in the chain, and re-installing it on top of a later
    /// handler would make the chain loop back on itself.
    pub(crate) fn install(
        &self,
        signal: libc::c_int,
        handler: SigInfoHandler,
    ) -> std::io::Result<()> {
        if self.installed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let result = self.install_once(signal, handler);
        if result.is_err() {
            self.installed.store(false, Ordering::SeqCst);
        }
        result
    }

    fn install_once(&self, signal: libc::c_int, handler: SigInfoHandler) -> std::io::Result<()> {
        let handler_addr = handler as usize;
        // SAFETY: plain sigaction(2) calls on zero-initialised structs; the
        // installed handler is async-signal-safe (atomics plus forwarding).
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut old) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if old.sa_sigaction == handler_addr {
                return Ok(());
            }
            // Publish the previous disposition before our handler can run.
            self.previous_siginfo
                .store(old.sa_flags & libc::SA_SIGINFO != 0, Ordering::SeqCst);
            self.previous.store(old.sa_sigaction, Ordering::SeqCst);

            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler_addr;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Invoke the displaced handler, if it was a real handler function.
    /// `SIG_DFL` and `SIG_IGN` are not forwarded, preserving the behaviour of
    /// a lone handler. Async-signal-safe.
    pub(crate) fn forward(
        &self,
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        let previous = self.previous.load(Ordering::SeqCst);
        if previous == libc::SIG_DFL || previous == libc::SIG_IGN || previous == libc::SIG_ERR {
            return;
        }
        // SAFETY: `previous` was read from sigaction(2) as an installed
        // handler, and SA_SIGINFO selects which signature it was installed with.
        unsafe {
            if self.previous_siginfo.load(Ordering::SeqCst) {
                let handler: SigInfoHandler = std::mem::transmute(previous);
                handler(signal, info, context);
            } else {
                let handler: extern "C" fn(libc::c_int) = std::mem::transmute(previous);
                handler(signal);
            }
        }
    }
}
