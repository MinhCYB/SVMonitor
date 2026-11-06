//! Append-only JSONL history of events and samples with size-based rotation.
use crate::model::{Event, Metrics};
use crate::util::ensure_private_dir;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub struct History {
    events: PathBuf,
    samples: PathBuf,
    keep_samples: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct SampleRec {
    ts: i64,
    server: String,
    cpu: Option<f32>,
    mem: f32,
    swap: Option<f32>,
    disk: Option<f32>,
    load1: f32,
    up: u64,
}

impl History {
    pub fn new(events: PathBuf, samples: PathBuf, keep_samples: bool) -> History {
        if let Some(d) = events.parent() {
            let _ = ensure_private_dir(d);
        }
        History {
            events,
            samples,
            keep_samples,
        }
    }

    pub fn record_event(&self, e: &Event) {
        if let Ok(line) = serde_json::to_string(e) {
            append(&self.events, &line);
        }
    }

    pub fn record_sample(&self, server: &str, m: &Metrics) {
        if !self.keep_samples {
            return;
        }
        let rec = SampleRec {
            ts: m.ts,
            server: server.into(),
            cpu: m.cpu_pct,
            mem: m.mem_pct(),
            swap: m.swap_pct(),
            disk: m.worst_disk().map(|d| d.pct()),
            load1: m.load[0],
            up: m.uptime_secs,
        };
        if let Ok(line) = serde_json::to_string(&rec) {
            append(&self.samples, &line);
        }
    }
}

fn append(path: &Path, line: &str) {
    rotate_if_needed(path);
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn rotate_if_needed(path: &Path) {
    if fs::metadata(path).map(|m| m.len()).unwrap_or(0) >= MAX_FILE_BYTES {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = fs::rename(path, PathBuf::from(old));
    }
}

/// Read the last `limit` events (optionally one server), oldest first. Bad lines are skipped.
pub fn read_events(path: &Path, server: Option<&str>, limit: usize) -> Vec<Event> {
    let mut all = Vec::new();
    let mut old = path.as_os_str().to_owned();
    old.push(".1");
    for p in [PathBuf::from(old), path.to_path_buf()] {
        if let Ok(f) = File::open(&p) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                if let Ok(e) = serde_json::from_str::<Event>(&line)
                    && server.is_none_or(|s| e.server == s)
                {
                    all.push(e);
                }
            }
        }
    }
    let n = all.len();
    if n > limit {
        all.drain(..n - limit);
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EventKind;

    fn ev(i: i64, s: &str) -> Event {
        Event {
            ts: i,
            server: s.into(),
            kind: EventKind::Down,
            message: format!("m{i}"),
            key: "down".into(),
            notify: true,
        }
    }

    #[test]
    fn append_read_filter_limit() {
        let d = tempfile::tempdir().unwrap();
        let h = History::new(d.path().join("e.jsonl"), d.path().join("s.jsonl"), true);
        for i in 0..10 {
            h.record_event(&ev(i, if i % 2 == 0 { "a" } else { "b" }));
        }
        assert_eq!(read_events(&d.path().join("e.jsonl"), None, 100).len(), 10);
        let a = read_events(&d.path().join("e.jsonl"), Some("a"), 100);
        assert_eq!(a.len(), 5);
        let last = read_events(&d.path().join("e.jsonl"), None, 3);
        assert_eq!(last.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![7, 8, 9]);
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("e.jsonl");
        let h = History::new(p.clone(), d.path().join("s"), false);
        h.record_event(&ev(1, "a"));
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"{broken\n")
            .unwrap();
        h.record_event(&ev(2, "a"));
        assert_eq!(read_events(&p, None, 10).len(), 2);
    }

    #[test]
    fn rotates_by_size_and_keeps_reading_old() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("e.jsonl");
        let h = History::new(p.clone(), d.path().join("s"), false);
        h.record_event(&ev(1, "a"));
        let f = OpenOptions::new().append(true).open(&p).unwrap();
        f.set_len(MAX_FILE_BYTES).unwrap();
        drop(f);
        h.record_event(&ev(2, "a"));
        assert!(d.path().join("e.jsonl.1").exists());
        let evs = read_events(&p, None, 10);
        assert_eq!(evs.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn missing_file_is_empty() {
        assert!(read_events(Path::new("/nonexistent/e"), None, 5).is_empty());
    }
}
