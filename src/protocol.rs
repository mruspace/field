//! The task interface: the bytes between the supervisor and a replica
//! (docs/spec.md, section 3), and reads and writes that give up at a deadline.
//!
//! Request, one per cycle: cycle (8 bytes), flags (1), input length (4) and
//! input, state length (4) and state. Response: output length (4) and output,
//! state length (4) and state, then a second output (length and bytes) only
//! when the self-check flag was set. All integers little-endian.

use std::io::{self, Read};
use std::os::fd::RawFd;
use std::time::Instant;

/// Compute twice and return both outputs.
pub const FLAG_SELF_CHECK: u8 = 1;
/// This request is a known-answer test.
pub const FLAG_KAT: u8 = 2;

/// Size limits a reader enforces on length fields, so a wrong length cannot
/// make it allocate without bound.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub record_max: u32,
    pub state_max: u32,
}

/// A decoded request, owned (used by a task written in Rust and by tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub cycle: u64,
    pub flags: u8,
    pub input: Vec<u8>,
    pub state: Vec<u8>,
}

/// A replica's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub output: Vec<u8>,
    pub state: Vec<u8>,
    /// The second computation, present only after a self-check request.
    pub second: Option<Vec<u8>>,
}

/// Append the request bytes to `buf`.
pub fn encode_request(cycle: u64, flags: u8, input: &[u8], state: &[u8], buf: &mut Vec<u8>) {
    buf.extend_from_slice(&cycle.to_le_bytes());
    buf.push(flags);
    push_field(buf, input);
    push_field(buf, state);
}

/// Append the response bytes to `buf`.
#[allow(dead_code)] // the task side of the protocol: used by the demo task and tests
pub fn encode_response(resp: &Response, buf: &mut Vec<u8>) {
    push_field(buf, &resp.output);
    push_field(buf, &resp.state);
    if let Some(second) = &resp.second {
        push_field(buf, second);
    }
}

fn push_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

#[allow(dead_code)] // the task side of the protocol
/// Read one request from a blocking reader (a task's standard input).
/// `Ok(None)` is a clean end of input before a request started.
pub fn read_request<R: Read>(r: &mut R, limits: Limits) -> io::Result<Option<Request>> {
    let mut head = [0u8; 9];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "end of input inside a request")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    let cycle = u64::from_le_bytes(head[..8].try_into().unwrap());
    let flags = head[8];
    let input = read_field(r, limits.record_max, "input")?;
    let state = read_field(r, limits.state_max, "state")?;
    Ok(Some(Request { cycle, flags, input, state }))
}

#[allow(dead_code)] // tests
/// Read one response from a blocking reader (tests and the check-task tool
/// use it on a pipe they know has data).
pub fn read_response_blocking<R: Read>(r: &mut R, self_check: bool, limits: Limits) -> io::Result<Response> {
    let output = read_field(r, limits.record_max, "output")?;
    let state = read_field(r, limits.state_max, "state")?;
    let second = if self_check { Some(read_field(r, limits.record_max, "second")?) } else { None };
    Ok(Response { output, state, second })
}

#[allow(dead_code)]
fn read_field<R: Read>(r: &mut R, max: u32, what: &'static str) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{what} length {len} is over the limit {max}")));
    }
    let mut bytes = vec![0u8; len as usize];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Why a read or write on a replica did not complete.
#[derive(Debug)]
pub enum ReadError {
    /// The deadline passed.
    Timeout,
    /// The other end closed (the replica exited).
    Closed,
    /// A length field over the limit: the reply is not a reply.
    Overlong {
        field: &'static str,
        len: u32,
    },
    Io(io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Timeout => write!(f, "timeout"),
            ReadError::Closed => write!(f, "closed"),
            ReadError::Overlong { field, len } => write!(f, "{field} length {len} over the limit"),
            ReadError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Wait until `fd` is ready for `events` (POLLIN or POLLOUT) or the deadline
/// passes. A signal that interrupts the wait is not an error: the wait goes
/// on with the time left.
pub fn poll_fd(fd: RawFd, events: libc::c_short, deadline: Instant) -> Result<(), ReadError> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ReadError::Timeout);
        }
        let ms = libc::c_int::try_from(left.as_millis().saturating_add(1)).unwrap_or(libc::c_int::MAX);
        let mut p = libc::pollfd { fd, events, revents: 0 };
        // SAFETY: one valid pollfd, owned by this frame.
        let ready = unsafe { libc::poll(&mut p, 1, ms) };
        if ready > 0 {
            return Ok(());
        }
        if ready == 0 {
            continue; // poll's own timeout; the loop checks the deadline
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(ReadError::Io(e));
        }
    }
}

/// Fill `buf` from `fd`, giving up at the deadline.
pub fn read_exact_deadline(fd: RawFd, buf: &mut [u8], deadline: Instant) -> Result<(), ReadError> {
    let mut filled = 0;
    while filled < buf.len() {
        poll_fd(fd, libc::POLLIN, deadline)?;
        let rest = &mut buf[filled..];
        // SAFETY: rest is a valid writable buffer of rest.len() bytes.
        let n = unsafe { libc::read(fd, rest.as_mut_ptr().cast(), rest.len()) };
        if n > 0 {
            filled += n as usize;
            continue;
        }
        if n == 0 {
            return Err(ReadError::Closed);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
            _ => return Err(ReadError::Io(e)),
        }
    }
    Ok(())
}

/// Read from `fd` until it closes, giving up at the deadline or when more
/// than `max` bytes arrive.
pub fn read_to_end_deadline(fd: RawFd, max: usize, deadline: Instant) -> Result<Vec<u8>, ReadError> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        poll_fd(fd, libc::POLLIN, deadline)?;
        // SAFETY: chunk is a valid writable buffer.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n > 0 {
            out.extend_from_slice(&chunk[..n as usize]);
            if out.len() > max {
                return Err(ReadError::Overlong { field: "output", len: out.len().min(u32::MAX as usize) as u32 });
            }
            continue;
        }
        if n == 0 {
            return Ok(out);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
            _ => return Err(ReadError::Io(e)),
        }
    }
}

/// Write all of `buf` to a non-blocking `fd`, giving up at the deadline. A
/// replica that stopped reading leaves the pipe full; this is what keeps the
/// supervisor from hanging on it.
pub fn write_all_deadline(fd: RawFd, buf: &[u8], deadline: Instant) -> Result<(), ReadError> {
    let mut written = 0;
    while written < buf.len() {
        poll_fd(fd, libc::POLLOUT, deadline)?;
        let rest = &buf[written..];
        // SAFETY: rest is a valid readable buffer of rest.len() bytes.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n >= 0 {
            written += n as usize;
            continue;
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
            Some(libc::EPIPE) => return Err(ReadError::Closed),
            _ => return Err(ReadError::Io(e)),
        }
    }
    Ok(())
}

/// Read one response from `fd` with a deadline for the whole reply.
pub fn read_response(fd: RawFd, self_check: bool, limits: Limits, deadline: Instant) -> Result<Response, ReadError> {
    let output = read_field_deadline(fd, limits.record_max, "output", deadline)?;
    let state = read_field_deadline(fd, limits.state_max, "state", deadline)?;
    let second = if self_check { Some(read_field_deadline(fd, limits.record_max, "second", deadline)?) } else { None };
    Ok(Response { output, state, second })
}

fn read_field_deadline(fd: RawFd, max: u32, field: &'static str, deadline: Instant) -> Result<Vec<u8>, ReadError> {
    let mut len = [0u8; 4];
    read_exact_deadline(fd, &mut len, deadline)?;
    let len = u32::from_le_bytes(len);
    if len > max {
        return Err(ReadError::Overlong { field, len });
    }
    let mut bytes = vec![0u8; len as usize];
    read_exact_deadline(fd, &mut bytes, deadline)?;
    Ok(bytes)
}

/// Put `fd` in non-blocking mode.
pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a file descriptor this process owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::Duration;

    const LIMITS: Limits = Limits { record_max: 65536, state_max: 65536 };

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: a valid array of two ints.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: fds are fresh and owned here.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    #[test]
    fn request_round_trip() {
        for (input, state) in [(vec![], vec![]), (vec![7], vec![]), (vec![1; 65536], vec![2; 65536])] {
            let mut buf = Vec::new();
            encode_request(42, FLAG_SELF_CHECK, &input, &state, &mut buf);
            assert_eq!(buf.len(), 9 + 8 + input.len() + state.len());
            let got = read_request(&mut Cursor::new(&buf), LIMITS).unwrap().unwrap();
            assert_eq!(got, Request { cycle: 42, flags: FLAG_SELF_CHECK, input, state });
        }
        assert!(read_request(&mut Cursor::new(b""), LIMITS).unwrap().is_none());
        assert!(read_request(&mut Cursor::new(b"abc"), LIMITS).is_err());
    }

    #[test]
    fn response_round_trip() {
        let resp = Response { output: b"out".to_vec(), state: vec![], second: Some(b"out".to_vec()) };
        let mut buf = Vec::new();
        encode_response(&resp, &mut buf);
        assert_eq!(read_response_blocking(&mut Cursor::new(&buf), true, LIMITS).unwrap(), resp);
        let plain = Response { second: None, ..resp.clone() };
        let mut buf = Vec::new();
        encode_response(&plain, &mut buf);
        assert_eq!(read_response_blocking(&mut Cursor::new(&buf), false, LIMITS).unwrap(), plain);
    }

    #[test]
    fn overlong_length_is_refused() {
        let buf = [0xff, 0xff, 0xff, 0x7f, 0, 0, 0, 0];
        assert!(read_response_blocking(&mut Cursor::new(&buf), false, LIMITS).is_err());
        let (r, w) = pipe();
        let mut buf = buf.to_vec();
        buf.resize(1024, 0);
        write_all_deadline(w.as_raw_fd(), &buf, Instant::now() + Duration::from_secs(1)).unwrap();
        match read_response(r.as_raw_fd(), false, LIMITS, Instant::now() + Duration::from_secs(1)) {
            Err(ReadError::Overlong { field: "output", .. }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn read_times_out_without_data_and_sees_a_close() {
        let (r, w) = pipe();
        let mut buf = [0u8; 4];
        let t = Instant::now();
        match read_exact_deadline(r.as_raw_fd(), &mut buf, t + Duration::from_millis(50)) {
            Err(ReadError::Timeout) => {}
            other => panic!("{other:?}"),
        }
        assert!(t.elapsed() >= Duration::from_millis(50));
        drop(w);
        match read_exact_deadline(r.as_raw_fd(), &mut buf, Instant::now() + Duration::from_millis(50)) {
            Err(ReadError::Closed) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn write_times_out_on_a_full_pipe() {
        let (_r, w) = pipe();
        set_nonblocking(w.as_raw_fd()).unwrap();
        let big = vec![0u8; 1 << 20];
        match write_all_deadline(w.as_raw_fd(), &big, Instant::now() + Duration::from_millis(50)) {
            Err(ReadError::Timeout) => {}
            other => panic!("{other:?}"),
        }
    }

    extern "C" fn nothing(_: libc::c_int) {}

    #[test]
    fn a_signal_does_not_end_the_wait() {
        // SAFETY: a handler that does nothing is async-signal-safe.
        unsafe { libc::signal(libc::SIGUSR1, nothing as libc::sighandler_t) };
        let (r, w) = pipe();
        let reader = std::thread::current().id();
        let _ = reader;
        // SAFETY: pthread_self is always valid.
        let me = unsafe { libc::pthread_self() };
        let interrupter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            // SAFETY: me is a live thread; SIGUSR1 has a handler.
            unsafe { libc::pthread_kill(me, libc::SIGUSR1) };
            std::thread::sleep(Duration::from_millis(30));
            write_all_deadline(w.as_raw_fd(), b"abcd", Instant::now() + Duration::from_secs(1)).unwrap();
        });
        let mut buf = [0u8; 4];
        read_exact_deadline(r.as_raw_fd(), &mut buf, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(&buf, b"abcd");
        interrupter.join().unwrap();
    }
}
