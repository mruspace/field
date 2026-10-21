//! Delivery: checked outputs go to the sink the operator configures; the
//! heartbeat, one line per period, goes to its own target. The serial port
//! setup lives here too (raw mode, 8N1, no flow control).

use crate::config::Endpoint;
use crate::protocol;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub trait Sink {
    fn deliver(&mut self, cycle: u64, bytes: &[u8]) -> Result<(), String>;
    /// Flush and sync. Returns the length of a file sink (0 for the others),
    /// for the checkpoint.
    fn sync(&mut self) -> io::Result<u64>;
}

/// Open the output sink. On a resume, a file sink is cut back to the
/// length the checkpoint recorded.
pub fn open(ep: &Endpoint, resume_len: Option<u64>, cmd_timeout: Duration) -> Result<Box<dyn Sink>, String> {
    Ok(match ep {
        Endpoint::None => Box::new(Discard),
        Endpoint::File(p) => {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(resume_len.is_none())
                .open(p)
                .map_err(|e| format!("{}: {e}", p.display()))?;
            if let Some(len) = resume_len {
                f.set_len(len).map_err(|e| format!("{}: {e}", p.display()))?;
                f.seek(SeekFrom::End(0)).map_err(|e| format!("{}: {e}", p.display()))?;
            }
            Box::new(FramedFile { file: f })
        }
        Endpoint::Tcp { host, port } => {
            let s = TcpStream::connect((host.as_str(), *port)).map_err(|e| format!("tcp:{host}:{port}: {e}"))?;
            Box::new(FramedStream { stream: s })
        }
        Endpoint::Serial { dev, baud } => {
            let f = open_serial(dev, *baud).map_err(|e| format!("{}: {e}", dev.display()))?;
            Box::new(SerialLines { file: f })
        }
        Endpoint::Cmd(argv) => Box::new(CmdSink { argv: argv.clone(), timeout: cmd_timeout }),
        Endpoint::Tick => return Err("output = tick".into()),
    })
}

struct Discard;

impl Sink for Discard {
    fn deliver(&mut self, _: u64, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn sync(&mut self) -> io::Result<u64> {
        Ok(0)
    }
}

/// `cycle u64 LE, len u32 LE, bytes`, appended.
pub fn frame(cycle: u64, bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + bytes.len());
    v.extend_from_slice(&cycle.to_le_bytes());
    v.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    v.extend_from_slice(bytes);
    v
}

struct FramedFile {
    file: File,
}

impl Sink for FramedFile {
    fn deliver(&mut self, cycle: u64, bytes: &[u8]) -> Result<(), String> {
        self.file.write_all(&frame(cycle, bytes)).map_err(|e| format!("file: {e}"))
    }
    fn sync(&mut self) -> io::Result<u64> {
        self.file.sync_data()?;
        Ok(self.file.metadata()?.len())
    }
}

struct FramedStream {
    stream: TcpStream,
}

impl Sink for FramedStream {
    fn deliver(&mut self, cycle: u64, bytes: &[u8]) -> Result<(), String> {
        self.stream.write_all(&frame(cycle, bytes)).map_err(|e| format!("tcp: {e}"))
    }
    fn sync(&mut self) -> io::Result<u64> {
        self.stream.flush()?;
        Ok(0)
    }
}

/// `<cycle>,<bytes>\n` on a serial port. The task keeps the output ASCII.
struct SerialLines {
    file: File,
}

impl Sink for SerialLines {
    fn deliver(&mut self, cycle: u64, bytes: &[u8]) -> Result<(), String> {
        let mut line = cycle.to_string().into_bytes();
        line.push(b',');
        line.extend_from_slice(bytes);
        line.push(b'\n');
        write_deadline(&self.file, &line, Duration::from_secs(5)).map_err(|e| format!("serial: {e}"))
    }
    fn sync(&mut self) -> io::Result<u64> {
        Ok(0)
    }
}

fn write_deadline(f: &File, bytes: &[u8], timeout: Duration) -> Result<(), protocol::ReadError> {
    protocol::write_all_deadline(f.as_raw_fd(), bytes, Instant::now() + timeout)
}

/// A program run once per delivered output, with the bytes on its standard
/// input and `FIELD_CYCLE` in its environment.
struct CmdSink {
    argv: Vec<String>,
    timeout: Duration,
}

impl Sink for CmdSink {
    fn deliver(&mut self, cycle: u64, bytes: &[u8]) -> Result<(), String> {
        let mut child = Command::new(&self.argv[0])
            .args(&self.argv[1..])
            .env("FIELD_CYCLE", cycle.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| format!("cmd:{}: {e}", self.argv[0]))?;
        let to = std::os::fd::OwnedFd::from(child.stdin.take().unwrap());
        protocol::set_nonblocking(to.as_raw_fd()).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + self.timeout;
        let written = protocol::write_all_deadline(to.as_raw_fd(), bytes, deadline);
        drop(to);
        if let Err(e) = written {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("cmd:{}: {e}", self.argv[0]));
        }
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(format!("cmd:{}: exit {status}", self.argv[0])),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("cmd:{}: timeout", self.argv[0]));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    fn sync(&mut self) -> io::Result<u64> {
        Ok(0)
    }
}

/// One line at a time: the heartbeat.
pub trait Line {
    fn line(&mut self, text: &str) -> Result<(), String>;
}

pub fn open_line(ep: &Endpoint) -> Result<Option<Box<dyn Line>>, String> {
    Ok(match ep {
        Endpoint::None => None,
        Endpoint::File(p) => Some(Box::new(LineFile { path: p.clone() })),
        Endpoint::Tcp { host, port } => {
            let s = TcpStream::connect((host.as_str(), *port)).map_err(|e| format!("tcp:{host}:{port}: {e}"))?;
            Some(Box::new(LineStream { stream: s }))
        }
        Endpoint::Serial { dev, baud } => {
            let f = open_serial(dev, *baud).map_err(|e| format!("{}: {e}", dev.display()))?;
            Some(Box::new(LineSerial { file: f }))
        }
        Endpoint::Tick | Endpoint::Cmd(_) => return Err(format!("heartbeat = {ep}")),
    })
}

/// The file holds the latest line only (written to a temporary, renamed).
struct LineFile {
    path: PathBuf,
}

impl Line for LineFile {
    fn line(&mut self, text: &str) -> Result<(), String> {
        let tmp = self.path.with_extension("tmp");
        let w = || -> io::Result<()> {
            let mut f = File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        w().map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

struct LineStream {
    stream: TcpStream,
}

impl Line for LineStream {
    fn line(&mut self, text: &str) -> Result<(), String> {
        self.stream.write_all(format!("{text}\n").as_bytes()).map_err(|e| format!("tcp: {e}"))
    }
}

struct LineSerial {
    file: File,
}

impl Line for LineSerial {
    fn line(&mut self, text: &str) -> Result<(), String> {
        write_deadline(&self.file, format!("{text}\n").as_bytes(), Duration::from_secs(5))
            .map_err(|e| format!("serial: {e}"))
    }
}

/// Open a serial device in raw mode: 8 data bits, no parity, one stop bit,
/// no flow control, the given speed.
pub fn open_serial(dev: &Path, baud: u32) -> io::Result<File> {
    let speed = match baud {
        300 => libc::B300,
        1200 => libc::B1200,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        230400 => libc::B230400,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unsupported baud rate {baud}"))),
    };
    let f = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(dev)?;
    let fd = f.as_raw_fd();
    // SAFETY: termios calls on an open descriptor, with a struct this frame owns.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cflag &= !(libc::CRTSCTS);
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
        if libc::cfsetispeed(&mut t, speed) != 0 || libc::cfsetospeed(&mut t, speed) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(f)
}

use std::os::unix::fs::OpenOptionsExt;
