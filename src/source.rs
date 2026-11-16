//! Acquisition: one input record per cycle from the source the operator
//! configures. The supervisor reads the record once and hands the same
//! bytes to every replica (docs/spec.md, section 2). Every wait looks at
//! the stop flag, so a slow source cannot delay a clean stop.

use crate::config::Endpoint;
use crate::protocol::{self, ReadError};
use crate::stop;
use std::fs::File;
use std::io;
use std::net::TcpStream;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub enum Acquired {
    Record(Vec<u8>),
    /// The source ended (a file read to its end).
    End,
    /// A stop was requested while waiting.
    Stop,
    /// A `cmd:` source failed this time; the cycle is not consumed.
    Failed(String),
    /// A serial line longer than `record_max`: the record is cut there; the
    /// full length is logged.
    Overlong(Vec<u8>, usize),
}

pub trait Source {
    /// The record for `cycle`.
    fn next(&mut self, cycle: u64, record_max: u32, cmd_timeout: Duration) -> Result<Acquired, String>;

    /// `input_backlog = newest` (docs/spec.md, section 11): replace `record`
    /// with the newest complete record already waiting, and return it with
    /// the count of records skipped. Reads only what is there, and stops at
    /// `until` even if a flood keeps it coming. Sources with no backlog
    /// (`tick`, `cmd:`) skip nothing.
    fn newest(&mut self, record: Vec<u8>, _record_max: u32, _until: Instant) -> Result<(Vec<u8>, u64), String> {
        Ok((record, 0))
    }
}

/// True if `fd` has something to read now (data, or the end).
fn readable_now(fd: RawFd) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd, owned by this frame; a zero timeout.
    let ready = unsafe { libc::poll(&mut p, 1, 0) };
    ready > 0 && p.revents & (libc::POLLIN | libc::POLLHUP) != 0
}

pub fn open(ep: &Endpoint) -> Result<Box<dyn Source>, String> {
    Ok(match ep {
        Endpoint::Tick => Box::new(Tick),
        Endpoint::File(p) => {
            let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
            Box::new(Framed { fd: OwnedFd::from(f), what: "file" })
        }
        Endpoint::Tcp { host, port } => {
            let s = TcpStream::connect((host.as_str(), *port)).map_err(|e| format!("tcp:{host}:{port}: {e}"))?;
            s.set_nonblocking(true).map_err(|e| e.to_string())?;
            Box::new(Framed { fd: OwnedFd::from(s), what: "tcp" })
        }
        Endpoint::Serial { dev, baud } => {
            let f = crate::sink::open_serial(dev, *baud).map_err(|e| format!("{}: {e}", dev.display()))?;
            Box::new(Lines { fd: OwnedFd::from(f), pending: Vec::new(), cut: None })
        }
        Endpoint::Cmd(argv) => Box::new(Cmd { argv: argv.clone() }),
        Endpoint::None => return Err("input = none".into()),
    })
}

/// The cycle number as the record: the supervisor's own clock, for a
/// self-contained task.
struct Tick;

impl Source for Tick {
    fn next(&mut self, cycle: u64, _: u32, _: Duration) -> Result<Acquired, String> {
        Ok(Acquired::Record(cycle.to_le_bytes().to_vec()))
    }
}

enum Read {
    Full,
    End,
    Stop,
}

/// Fill `buf` from `fd`, looking at the stop flag between waits.
fn read_exact_stoppable(fd: RawFd, buf: &mut [u8]) -> io::Result<Read> {
    let mut filled = 0;
    while filled < buf.len() {
        if stop::requested() {
            return Ok(Read::Stop);
        }
        match protocol::poll_fd(fd, libc::POLLIN, Instant::now() + stop::SLICE) {
            Ok(()) => {}
            Err(ReadError::Timeout) => continue,
            Err(ReadError::Io(e)) => return Err(e),
            Err(_) => unreachable!(),
        }
        let rest = &mut buf[filled..];
        // SAFETY: rest is a valid writable buffer of rest.len() bytes.
        let n = unsafe { libc::read(fd, rest.as_mut_ptr().cast(), rest.len()) };
        match n {
            1.. => filled += n as usize,
            0 => return Ok(Read::End),
            _ => {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EINTR) | Some(libc::EAGAIN) => continue,
                    _ => return Err(e),
                }
            }
        }
    }
    Ok(Read::Full)
}

/// Length-prefixed records (`len u32 LE, bytes`) from a file, a FIFO or a
/// TCP stream.
struct Framed {
    fd: OwnedFd,
    what: &'static str,
}

impl Source for Framed {
    fn next(&mut self, _: u64, record_max: u32, _: Duration) -> Result<Acquired, String> {
        let fd = self.fd.as_raw_fd();
        let mut len = [0u8; 4];
        match read_exact_stoppable(fd, &mut len).map_err(|e| format!("{}: {e}", self.what))? {
            Read::Full => {}
            Read::End => return Ok(Acquired::End),
            Read::Stop => return Ok(Acquired::Stop),
        }
        let n = u32::from_le_bytes(len);
        if n > record_max {
            return Err(format!("{}: record length {n} over record_max {record_max}", self.what));
        }
        let mut bytes = vec![0u8; n as usize];
        match read_exact_stoppable(fd, &mut bytes).map_err(|e| format!("{}: {e}", self.what))? {
            Read::Full => Ok(Acquired::Record(bytes)),
            Read::End => Err(format!("{}: ended inside a record", self.what)),
            Read::Stop => Ok(Acquired::Stop),
        }
    }

    fn newest(&mut self, mut record: Vec<u8>, record_max: u32, until: Instant) -> Result<(Vec<u8>, u64), String> {
        let mut skipped = 0;
        while Instant::now() < until && readable_now(self.fd.as_raw_fd()) {
            match self.next(0, record_max, Duration::ZERO)? {
                Acquired::Record(r) => {
                    record = r;
                    skipped += 1;
                }
                // the end or a stop: the next cycle's read meets it again
                _ => break,
            }
        }
        Ok((record, skipped))
    }
}

/// Newline-terminated lines from a serial port. The record is the bytes
/// before the newline, without a trailing carriage return. A line longer
/// than `record_max` is cut there as it arrives, and the rest of it is
/// counted and dropped, so a source that never sends a newline cannot grow
/// the supervisor's memory.
struct Lines {
    fd: OwnedFd,
    pending: Vec<u8>,
    /// The first `record_max` bytes of an overlong line, and its length so far.
    cut: Option<(Vec<u8>, usize)>,
}

impl Lines {
    /// The next complete line in `pending`, if there is one.
    fn take_line(&mut self, max: usize) -> Option<Acquired> {
        let Some(i) = self.pending.iter().position(|b| *b == b'\n') else {
            if self.pending.len() > max {
                let (_, seen) = self.cut.get_or_insert_with(|| (self.pending[..max].to_vec(), 0));
                *seen += self.pending.len();
                self.pending.clear();
            }
            return None;
        };
        let mut line: Vec<u8> = self.pending.drain(..=i).collect();
        line.pop();
        if let Some((head, seen)) = self.cut.take() {
            return Some(Acquired::Overlong(head, seen + line.len()));
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.len() > max {
            let full = line.len();
            line.truncate(max);
            return Some(Acquired::Overlong(line, full));
        }
        Some(Acquired::Record(line))
    }
}

impl Source for Lines {
    fn next(&mut self, _: u64, record_max: u32, _: Duration) -> Result<Acquired, String> {
        let max = record_max as usize;
        loop {
            if let Some(a) = self.take_line(max) {
                return Ok(a);
            }
            let mut byte = [0u8; 1];
            match read_exact_stoppable(self.fd.as_raw_fd(), &mut byte).map_err(|e| format!("serial: {e}"))? {
                Read::Full => self.pending.push(byte[0]),
                Read::End => return Ok(Acquired::End),
                Read::Stop => return Ok(Acquired::Stop),
            }
        }
    }

    fn newest(&mut self, mut record: Vec<u8>, record_max: u32, until: Instant) -> Result<(Vec<u8>, u64), String> {
        let max = record_max as usize;
        let fd = self.fd.as_raw_fd();
        let mut skipped = 0;
        loop {
            while let Some(a) = self.take_line(max) {
                if let Acquired::Record(r) | Acquired::Overlong(r, _) = a {
                    record = r;
                    skipped += 1;
                }
            }
            if Instant::now() >= until || !readable_now(fd) {
                break;
            }
            let mut buf = [0u8; 4096];
            // SAFETY: buf is a valid writable buffer of buf.len() bytes.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break; // the end, or nothing after all: the next read sees it
            }
            self.pending.extend_from_slice(&buf[..n as usize]);
        }
        Ok((record, skipped))
    }
}

/// A program run once per cycle; its standard output is the record.
struct Cmd {
    argv: Vec<String>,
}

impl Source for Cmd {
    fn next(&mut self, cycle: u64, record_max: u32, timeout: Duration) -> Result<Acquired, String> {
        let mut child = Command::new(&self.argv[0])
            .args(&self.argv[1..])
            .env("FIELD_CYCLE", cycle.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cmd:{}: {e}", self.argv[0]))?;
        let from = OwnedFd::from(child.stdout.take().unwrap());
        protocol::set_nonblocking(from.as_raw_fd()).map_err(|e| e.to_string())?;
        let out = protocol::read_to_end_deadline(from.as_raw_fd(), record_max as usize, Instant::now() + timeout);
        match out {
            Ok(out) => {
                let status = child.wait().map_err(|e| e.to_string())?;
                if status.success() {
                    Ok(Acquired::Record(out))
                } else {
                    Ok(Acquired::Failed(format!(
                        "cmd-failed {}",
                        status.code().map_or("signal".to_string(), |c| c.to_string())
                    )))
                }
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Ok(Acquired::Failed(format!("cmd-failed {e}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pipe() -> (OwnedFd, std::fs::File) {
        let mut fds = [0; 2];
        // SAFETY: a valid array of two ints.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        use std::os::fd::FromRawFd;
        // SAFETY: two fresh descriptors this test owns.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), std::fs::File::from_raw_fd(fds[1])) }
    }

    fn framed(n: u64) -> Vec<u8> {
        let mut v = 8u32.to_le_bytes().to_vec();
        v.extend_from_slice(&n.to_le_bytes());
        v
    }

    #[test]
    fn newest_skips_the_framed_records_that_queued_up() {
        let (r, mut w) = pipe();
        for n in 0..10 {
            w.write_all(&framed(n)).unwrap();
        }
        let mut src = Framed { fd: r, what: "file" };
        let Acquired::Record(first) = src.next(1, 64, Duration::ZERO).unwrap() else { panic!() };
        assert_eq!(first, 0u64.to_le_bytes());
        let far = Instant::now() + Duration::from_secs(5);
        let (rec, skipped) = src.newest(first, 64, far).unwrap();
        assert_eq!((rec.clone(), skipped), (9u64.to_le_bytes().to_vec(), 9));
        // nothing waiting: the record stands
        let (rec, skipped) = src.newest(rec, 64, far).unwrap();
        assert_eq!((rec, skipped), (9u64.to_le_bytes().to_vec(), 0));
        w.write_all(&framed(10)).unwrap();
        let Acquired::Record(next) = src.next(2, 64, Duration::ZERO).unwrap() else { panic!() };
        assert_eq!(next, 10u64.to_le_bytes());
    }

    #[test]
    fn newest_takes_the_last_complete_line() {
        let (r, mut w) = pipe();
        w.write_all(b"a\nb\nc\nd").unwrap();
        let mut src = Lines { fd: r, pending: Vec::new(), cut: None };
        let Acquired::Record(first) = src.next(1, 64, Duration::ZERO).unwrap() else { panic!() };
        assert_eq!(first, b"a");
        let (rec, skipped) = src.newest(first, 64, Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!((rec.as_slice(), skipped), (&b"c"[..], 2));
        // the partial line waits for its newline
        w.write_all(b"e\n").unwrap();
        let Acquired::Record(next) = src.next(2, 64, Duration::ZERO).unwrap() else { panic!() };
        assert_eq!(next, b"de");
    }

    #[test]
    fn a_line_with_no_end_does_not_grow_memory() {
        let (r, mut w) = pipe();
        let writer = std::thread::spawn(move || {
            w.write_all(&vec![b'x'; 1 << 20]).unwrap();
            w.write_all(b"\nok\n").unwrap();
        });
        let mut src = Lines { fd: r, pending: Vec::new(), cut: None };
        match src.next(1, 16, Duration::ZERO).unwrap() {
            Acquired::Overlong(head, full) => assert_eq!((head, full), (vec![b'x'; 16], 1 << 20)),
            _ => panic!("expected the cut line"),
        }
        assert!(src.pending.len() <= 17, "pending {}", src.pending.len());
        let Acquired::Record(next) = src.next(2, 16, Duration::ZERO).unwrap() else { panic!() };
        assert_eq!(next, b"ok");
        writer.join().unwrap();
    }
}
