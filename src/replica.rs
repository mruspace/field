//! One replica: the operator's task as a process the supervisor talks to
//! over its standard input and output (docs/spec.md, section 3), or, with
//! the `cli` adapter, a program run once per request.
//!
//! The supervisor sets the memory limit in the child before it executes the
//! task, so any executable gets it. Writes to the task are non-blocking with
//! a deadline: a task that stopped reading cannot hang the supervisor on a
//! full pipe. Reads have the same deadline, for the whole reply.

use crate::config::Adapter;
use crate::faults::Injected;
use crate::protocol::{self, Limits, ReadError, Response, FLAG_KAT, FLAG_SELF_CHECK};
use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct ProcessLimits {
    /// Address-space limit per process in MB; 0 is none. Enforced on Linux.
    pub memory_mb: u64,
}

impl ProcessLimits {
    /// Apply to the calling process (the supervisor on itself; the child
    /// before exec).
    pub fn apply(self) {
        #[cfg(target_os = "linux")]
        if self.memory_mb > 0 {
            let v = libc::rlim_t::try_from(self.memory_mb * 1024 * 1024).unwrap_or(libc::RLIM_INFINITY);
            let r = libc::rlimit { rlim_cur: v, rlim_max: v };
            // SAFETY: a valid rlimit for this process.
            unsafe { libc::setrlimit(libc::RLIMIT_AS, &r) };
        }
    }
}

/// Why a replica gave no answer.
#[derive(Debug)]
pub enum Lost {
    Timeout,
    Exited,
    Protocol(String),
}

impl fmt::Display for Lost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Lost::Timeout => write!(f, "timeout"),
            Lost::Exited => write!(f, "exited"),
            Lost::Protocol(s) => write!(f, "{s}"),
        }
    }
}

impl From<ReadError> for Lost {
    fn from(e: ReadError) -> Lost {
        match e {
            ReadError::Timeout => Lost::Timeout,
            ReadError::Closed => Lost::Exited,
            ReadError::Overlong { field, len } => Lost::Protocol(format!("{field} length {len} over the limit")),
            ReadError::Io(e) => Lost::Protocol(e.to_string()),
        }
    }
}

/// The known-answer test: one input record and its correct output.
#[derive(Clone, Debug)]
pub struct Kat {
    pub input: Vec<u8>,
    pub output: Vec<u8>,
}

enum Kind {
    Server { child: Child, to: OwnedFd, from: OwnedFd },
    Cli { task: PathBuf, limits: ProcessLimits, pending: Option<CliRun> },
}

struct CliRun {
    first: Child,
    second: Option<Child>,
    state: Vec<u8>,
}

pub struct Replica {
    kind: Kind,
    /// Faults the schedule put on this replica (tests and pilots only).
    pub injected: Injected,
    buf: Vec<u8>,
}

fn command(task: &Path, limits: ProcessLimits) -> Command {
    let mut cmd = Command::new(task);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            limits.apply();
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd
}

impl Replica {
    pub fn spawn(task: &Path, adapter: Adapter, limits: ProcessLimits) -> io::Result<Replica> {
        let kind = match adapter {
            Adapter::Cli => Kind::Cli { task: task.to_path_buf(), limits, pending: None },
            Adapter::None => {
                let mut child = command(task, limits).spawn()?;
                let to = OwnedFd::from(child.stdin.take().unwrap());
                let from = OwnedFd::from(child.stdout.take().unwrap());
                protocol::set_nonblocking(to.as_raw_fd())?;
                protocol::set_nonblocking(from.as_raw_fd())?;
                Kind::Server { child, to, from }
            }
        };
        Ok(Replica { kind, injected: Injected::default(), buf: Vec::new() })
    }

    /// Send one request. With the `cli` adapter this starts the program.
    pub fn send(&mut self, cycle: u64, flags: u8, input: &[u8], state: &[u8], deadline: Instant) -> Result<(), Lost> {
        match &mut self.kind {
            Kind::Server { to, .. } => {
                self.buf.clear();
                protocol::encode_request(cycle, flags, input, state, &mut self.buf);
                protocol::write_all_deadline(to.as_raw_fd(), &self.buf, deadline)?;
                Ok(())
            }
            Kind::Cli { task, limits, pending } => {
                let start = |task: &Path, limits: ProcessLimits| -> Result<Child, Lost> {
                    let mut child = command(task, limits).spawn().map_err(|e| Lost::Protocol(e.to_string()))?;
                    let to = OwnedFd::from(child.stdin.take().unwrap());
                    protocol::set_nonblocking(to.as_raw_fd()).map_err(|e| Lost::Protocol(e.to_string()))?;
                    protocol::write_all_deadline(to.as_raw_fd(), input, deadline)?;
                    drop(to); // end of input
                    Ok(child)
                };
                let first = start(task, *limits)?;
                let second = if flags & FLAG_SELF_CHECK != 0 { Some(start(task, *limits)?) } else { None };
                *pending = Some(CliRun { first, second, state: state.to_vec() });
                Ok(())
            }
        }
    }

    /// Read the reply to the last request.
    pub fn receive(&mut self, self_check: bool, limits: Limits, deadline: Instant) -> Result<Response, Lost> {
        match &mut self.kind {
            Kind::Server { from, .. } => Ok(protocol::read_response(from.as_raw_fd(), self_check, limits, deadline)?),
            Kind::Cli { pending, .. } => {
                let Some(run) = pending.take() else { return Err(Lost::Protocol("receive before send".into())) };
                let output = cli_output(run.first, limits, deadline)?;
                let second = match run.second {
                    Some(child) => Some(cli_output(child, limits, deadline)?),
                    None => None,
                };
                Ok(Response { output, state: run.state, second })
            }
        }
    }

    pub fn ask(
        &mut self,
        cycle: u64,
        flags: u8,
        input: &[u8],
        state: &[u8],
        limits: Limits,
        timeout: Duration,
    ) -> Result<Response, Lost> {
        let deadline = Instant::now() + timeout;
        self.send(cycle, flags, input, state, deadline)?;
        self.receive(flags & FLAG_SELF_CHECK != 0, limits, deadline)
    }

    /// Run the known-answer test: true only for the right answer in time.
    /// The schedule's faults apply, so a stuck replica fails its test.
    pub fn kat(&mut self, kat: &Kat, cycle: u64, limits: Limits, timeout: Duration) -> bool {
        match self.ask(cycle, FLAG_KAT, &kat.input, &[], limits, timeout) {
            Ok(mut resp) => {
                let mut inj = self.injected;
                crate::faults::inject_bytes(&mut inj, &mut resp);
                resp.output == kat.output
            }
            Err(_) => false,
        }
    }

    pub fn pid(&self) -> Option<libc::pid_t> {
        match &self.kind {
            Kind::Server { child, .. } => Some(child.id() as libc::pid_t),
            Kind::Cli { .. } => None,
        }
    }

    /// Send a signal to the task process (fault injection).
    pub fn signal(&self, sig: libc::c_int) {
        if let Some(pid) = self.pid() {
            // SAFETY: a pid this supervisor spawned and has not reaped.
            unsafe { libc::kill(pid, sig) };
        }
    }

    /// Kill the task process and reap it.
    pub fn retire(self) {
        if let Kind::Server { mut child, .. } = self.kind {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// End the task cleanly: close its input, wait up to `grace`, then kill.
    pub fn stop(self, grace: Duration) {
        if let Kind::Server { mut child, to, from } = self.kind {
            drop(to);
            drop(from);
            let until = Instant::now() + grace;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                    _ => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn cli_output(mut child: Child, limits: Limits, deadline: Instant) -> Result<Vec<u8>, Lost> {
    let from = OwnedFd::from(child.stdout.take().unwrap());
    protocol::set_nonblocking(from.as_raw_fd()).map_err(|e| Lost::Protocol(e.to_string()))?;
    let out = protocol::read_to_end_deadline(from.as_raw_fd(), limits.record_max as usize, deadline);
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
    };
    let status = child.wait().map_err(|e| Lost::Protocol(e.to_string()))?;
    if !status.success() {
        return Err(Lost::Protocol(format!("exit {status}")));
    }
    Ok(out)
}
