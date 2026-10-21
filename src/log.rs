//! `events.csv`: `cycle,event,detail`, one line per event. A run of
//! identical events on consecutive cycles (fixed TMR left with one good and
//! one stuck replica detects a disagreement on every cycle) is written as
//! its first line plus one closing line with the count, so a long run stays
//! small. The checkpoint records the log's length and the open run, and a
//! resume cuts the file back to that length, so no event is in it twice.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// An open run of identical events: what, detail, first cycle, last cycle, count.
pub type Streak = (String, String, u64, u64, u64);

pub struct EventLog {
    file: BufWriter<File>,
    streak: Option<Streak>,
}

impl EventLog {
    /// Open the log. `resume` is the length and open run the checkpoint
    /// recorded; without it the file starts fresh with its header.
    pub fn open(path: &Path, resume: Option<(u64, Option<Streak>)>) -> io::Result<EventLog> {
        let fresh = resume.is_none();
        let mut f = OpenOptions::new().create(true).write(true).truncate(fresh).open(path)?;
        let streak = match resume {
            Some((len, streak)) => {
                f.set_len(len)?;
                f.seek(SeekFrom::End(0))?;
                streak
            }
            None => None,
        };
        let mut log = EventLog { file: BufWriter::new(f), streak };
        if fresh {
            writeln!(log.file, "cycle,event,detail")?;
        }
        Ok(log)
    }

    pub fn event(&mut self, cycle: u64, what: &str, detail: &str) {
        if let Some((w, d, first, last, n)) = self.streak.as_mut() {
            if w == what && d == detail {
                *last = cycle;
                *n += 1;
                return;
            }
            if *n > 1 {
                let _ = writeln!(self.file, "{last},{w},{d} repeated {n}x cycles {first}-{last}");
            }
        }
        let _ = writeln!(self.file, "{cycle},{what},{detail}");
        self.streak = Some((what.to_string(), detail.to_string(), cycle, cycle, 1));
    }

    /// Close an open run with its count, so a finished log is complete.
    pub fn close_streak(&mut self) {
        if let Some((w, d, first, last, n)) = self.streak.take() {
            if n > 1 {
                let _ = writeln!(self.file, "{last},{w},{d} repeated {n}x cycles {first}-{last}");
            }
        }
    }

    /// Hand buffered lines to the kernel (no sync): what the file shows
    /// between checkpoints.
    pub fn flush(&mut self) {
        let _ = self.file.flush();
    }

    /// Flush and sync; returns the file's length for the checkpoint.
    pub fn sync(&mut self) -> io::Result<u64> {
        self.file.flush()?;
        self.file.get_ref().sync_data()?;
        Ok(self.file.get_ref().metadata()?.len())
    }

    pub fn streak(&self) -> Option<Streak> {
        self.streak.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("field-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn runs_collapse_like_the_flight_demo() {
        let p = tmp("collapse");
        let mut l = EventLog::open(&p, None).unwrap();
        l.event(50, "outvoted", "2");
        l.event(100, "outvoted", "1");
        l.event(101, "outvoted", "1");
        l.event(102, "outvoted", "1");
        l.event(102, "retired", "1 strikes");
        l.event(103, "detected", "compare");
        l.close_streak();
        l.sync().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            text,
            "cycle,event,detail\n50,outvoted,2\n100,outvoted,1\n102,outvoted,1 repeated 3x cycles 100-102\n102,retired,1 strikes\n103,detected,compare\n"
        );
    }

    #[test]
    fn a_resume_cuts_the_log_and_continues_the_run() {
        let p = tmp("resume");
        let mut l = EventLog::open(&p, None).unwrap();
        l.event(1, "detected", "compare");
        l.event(2, "detected", "compare");
        let len = l.sync().unwrap();
        let streak = l.streak();
        l.event(3, "detected", "compare");
        l.event(4, "lost", "0");
        l.sync().unwrap();
        drop(l);
        let mut l = EventLog::open(&p, Some((len, streak))).unwrap();
        l.event(2, "resumed", "1");
        l.sync().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            text,
            "cycle,event,detail\n1,detected,compare\n2,detected,compare repeated 2x cycles 1-2\n2,resumed,1\n"
        );
    }
}
