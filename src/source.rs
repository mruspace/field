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
            Box::new(Lines { fd: OwnedFd::from(f), pending: Vec::new() })
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
}

/// Newline-terminated lines from a serial port. The record is the bytes
/// before the newline, without a trailing carriage return.
struct Lines {
    fd: OwnedFd,
    pending: Vec<u8>,
}

impl Source for Lines {
    fn next(&mut self, _: u64, record_max: u32, _: Duration) -> Result<Acquired, String> {
        let max = record_max as usize;
        loop {
            if let Some(i) = self.pending.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.len() > max {
                    let full = line.len();
                    line.truncate(max);
                    return Ok(Acquired::Overlong(line, full));
                }
                return Ok(Acquired::Record(line));
            }
            let mut byte = [0u8; 1];
            match read_exact_stoppable(self.fd.as_raw_fd(), &mut byte).map_err(|e| format!("serial: {e}"))? {
                Read::Full => self.pending.push(byte[0]),
                Read::End => return Ok(Acquired::End),
                Read::Stop => return Ok(Acquired::Stop),
            }
        }
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
