//! `summary.txt`: what the run has done, from its counters. Written every
//! N cycles, at a halt and at a clean stop. Nothing in it is estimated.

use crate::checkpoint::Checkpoint;
use crate::config::{Adapter, Config};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Memory and CPU of this process and its children, from getrusage.
pub struct Footprint {
    pub supervisor_kib: i64,
    pub replica_kib: i64,
    pub cpu_s: f64,
    pub wall_s: f64,
}

/// Max RSS in KiB and CPU seconds for `who` (RUSAGE_SELF or RUSAGE_CHILDREN).
pub fn usage_of(who: libc::c_int) -> (i64, f64) {
    // SAFETY: getrusage fills a struct this frame owns.
    let u = unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(who, &mut u);
        u
    };
    let kib = if cfg!(target_os = "macos") { u.ru_maxrss / 1024 } else { u.ru_maxrss };
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    (kib as i64, secs(u.ru_utime) + secs(u.ru_stime))
}

pub struct Report<'a> {
    pub cfg: &'a Config,
    pub cp: &'a Checkpoint,
    pub mode: &'a str,
    pub alive: usize,
    /// `running` while the run goes on, else why it stopped.
    pub stopped: &'a str,
    pub halted_at: Option<u64>,
    pub footprint: Footprint,
    pub now_unix: u64,
    pub heartbeat: Option<(u64, u64)>,
    pub faults_configured: bool,
    pub oracle: bool,
}

pub fn text(r: &Report) -> String {
    let c = r.cfg;
    let cp = r.cp;
    let none = || "none".to_string();
    let mut lines = vec![
        format!(
            "field={} quorum={} policy={} task={} adapter={} input={} output={}",
            crate::VERSION,
            crate::QUORUM_VERSION,
            c.policy.name(),
            c.task.display(),
            match c.adapter {
                Adapter::None => "none",
                Adapter::Cli => "cli",
            },
            c.input,
            c.output
        ),
        format!(
            "cycles={} delivered={} detected={} wrong={} alive={} mode={} last_delivered={} kat_tests={}",
            cp.cycle,
            cp.delivered,
            cp.detected,
            if r.oracle { cp.wrong.to_string() } else { "-".to_string() },
            r.alive,
            r.mode,
            cp.last_delivered,
            cp.kat_tests
        ),
        format!(
            "stopped={} at_cycle={} halted_at={} started={} written={}",
            r.stopped,
            cp.cycle,
            r.halted_at.map_or(never(), |t| t.to_string()),
            utc(cp.started_unix),
            utc(r.now_unix)
        ),
        format!(
            "checkpoint: every={} resumes={} last={} halted={}",
            c.checkpoint_every,
            cp.resumes,
            cp.cycle,
            if cp.halted { "yes" } else { "no" }
        ),
        format!(
            "footprint: supervisor_max_rss={}KiB replica_max_rss={}KiB cpu={:.3}s wall={:.1}s",
            r.footprint.supervisor_kib, r.footprint.replica_kib, r.footprint.cpu_s, r.footprint.wall_s
        ),
        format!(
            "limits: memory={}MB/process cpu_share={} max_run={} reply_timeout={}ms period={}ms",
            c.memory_mb,
            if c.cpu_percent > 0.0 { format!("{}%", c.cpu_percent) } else { none() },
            if c.max_seconds > 0.0 { format!("{}s", c.max_seconds) } else { none() },
            c.reply_timeout_ms,
            c.period_ms
        ),
    ];
    if let Some((sent, failed)) = r.heartbeat {
        lines.push(format!("heartbeat: sent={sent} failed={failed}"));
    }
    if r.faults_configured {
        let caught = cp.caught.len() as u64;
        let (mean, max) = if caught > 0 {
            let lat: Vec<u64> = cp.caught.iter().map(|f| f.caught - f.at).collect();
            (format!("{:.1}", lat.iter().sum::<u64>() as f64 / caught as f64), lat.iter().max().unwrap().to_string())
        } else {
            (none(), none())
        };
        lines.push(format!(
            "faults: injected={} caught={caught} latency_mean={mean} latency_max={max} (cycles)",
            cp.faults_injected
        ));
        for f in &cp.caught {
            lines.push(format!(
                "fault: {}:{}@{} caught={} latency={} how={}",
                f.kind.name(),
                f.replica,
                f.at,
                f.caught,
                f.caught - f.at,
                f.how
            ));
        }
    }
    lines.join("\n") + "\n"
}

fn never() -> String {
    "never".to_string()
}

/// Write the summary atomically next to the log.
pub fn write(dir: &Path, text: &str) -> io::Result<()> {
    let path = dir.join("summary.txt");
    let tmp = dir.join("summary.tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &path)
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (civil-from-days, Howard Hinnant).
pub fn utc(unix: u64) -> String {
    let days = (unix / 86400) as i64;
    let rem = unix % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951782400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1761040000), "2025-10-21T09:46:40Z");
        assert_eq!(utc(4102444799), "2099-12-31T23:59:59Z");
    }
}
