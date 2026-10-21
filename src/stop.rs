//! The clean stop: SIGTERM or SIGINT sets a flag; every wait in the
//! supervisor looks at it, so the run ends within a fraction of a second
//! with its cycle finished, its checkpoint written and its summary complete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);

/// The longest any wait goes without looking at the flag.
pub const SLICE: Duration = Duration::from_millis(200);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn install_handlers() {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    let handler = handler as *const () as libc::sighandler_t;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
    }
}

pub fn requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Sleep until `until`, or until a stop is requested. Returns true if the
/// sleep ran to its end.
pub fn sleep_until(until: Instant) -> bool {
    loop {
        if requested() {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(SLICE));
    }
}
