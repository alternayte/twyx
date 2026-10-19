//! Pane history: the pane's screen and scrollback on disk, so they outlive
//! a holder crash, a power loss and a host restart.
//!
//! A history directory holds generations. Generation `n` is a checkpoint
//! (`checkpoint-n`, the engine's own snapshot of screen and scrollback) and
//! a log (`log-n`) of the raw output since. A new checkpoint is written to a
//! temporary file and renamed, and only then are older generations removed,
//! so a crash at any point leaves one complete generation.
//!
//! A thread of its own does the writing. The holder hands it output without
//! waiting; when it falls behind, the output is dropped and the log records
//! a gap (Invariant 2).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use twyx_core::tuning;

const OUTPUT: u8 = 0;
const GAP: u8 = 1;

enum Record {
    Checkpoint(Vec<u8>),
    Output(Vec<u8>),
    Gap,
}

/// One entry of a log, as restore reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Output(Vec<u8>),
    /// Output was dropped here because the writer fell behind.
    Gap,
}

/// The holder's end of the history writer.
#[derive(Debug)]
pub struct History {
    records: SyncSender<Record>,
    /// Output was dropped since the last record that reached the writer.
    gap: bool,
    /// Output arrived since the last checkpoint.
    changed: bool,
    last_checkpoint: Instant,
    /// Bytes logged since the last checkpoint.
    logged: usize,
}

impl History {
    /// Starts the writer for `dir`. The first checkpoint must follow at once.
    pub fn start(dir: PathBuf) -> Result<Self> {
        create_dir(&dir)?;
        let generation = generations(&dir).last().copied().unwrap_or(0);
        let (records, inbound) = mpsc::sync_channel(tuning::HISTORY_QUEUE);
        thread::Builder::new()
            .name("twyx-history".to_owned())
            .spawn(move || write_records(&dir, generation, &inbound))
            .context("start the history writer")?;
        Ok(Self {
            records,
            gap: false,
            changed: true,
            last_checkpoint: Instant::now(),
            logged: 0,
        })
    }

    /// Hands output to the writer, or records a gap if it is behind.
    pub fn output(&mut self, bytes: &[u8]) {
        self.changed = true;
        if self.gap && !self.send(Record::Gap) {
            return;
        }
        self.gap = false;
        if self.send(Record::Output(bytes.to_vec())) {
            self.logged += bytes.len();
        } else {
            self.gap = true;
        }
    }

    /// Whether a checkpoint is due: output changed and the interval passed,
    /// or the log outgrew its share of the pane's disk cap.
    pub fn due(&self) -> bool {
        self.changed
            && (self.last_checkpoint.elapsed() >= tuning::HISTORY_CHECKPOINT_INTERVAL
                || self.logged >= tuning::HISTORY_LOG_BYTES)
    }

    /// Hands a snapshot to the writer. A writer that is behind gets it on a
    /// later try.
    pub fn checkpoint(&mut self, snapshot: Vec<u8>) {
        if snapshot.len() > tuning::HISTORY_DISK_BYTES {
            // The engine's scrollback budget keeps snapshots well below the
            // cap; one above it is not written, and the log goes on.
            return;
        }
        if self.send(Record::Checkpoint(snapshot)) {
            self.changed = false;
            self.gap = false;
            self.logged = 0;
            self.last_checkpoint = Instant::now();
        }
    }

    fn send(&self, record: Record) -> bool {
        match self.records.try_send(record) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}

fn create_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

fn private_file(path: &Path, append: bool) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .mode(0o600)
        .open(path)
}

/// The generations in a history directory, oldest first.
fn generations(dir: &Path) -> Vec<u64> {
    let mut found: Vec<u64> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix("checkpoint-")?
                .parse()
                .ok()
        })
        .collect();
    found.sort_unstable();
    found
}

fn write_records(dir: &Path, mut generation: u64, inbound: &Receiver<Record>) {
    let mut log: Option<File> = None;
    for record in inbound {
        match record {
            Record::Checkpoint(snapshot) => {
                generation += 1;
                log = write_checkpoint(dir, generation, &snapshot).ok();
            }
            Record::Output(bytes) => append(&mut log, OUTPUT, &bytes),
            Record::Gap => append(&mut log, GAP, &[]),
        }
    }
}

/// Writes generation `n` and removes the older ones. Returns its log.
fn write_checkpoint(dir: &Path, n: u64, snapshot: &[u8]) -> Result<File> {
    // A purge may have removed the directory under a running holder.
    create_dir(dir)?;
    let temporary = dir.join(format!("checkpoint-{n}.tmp"));
    let mut file = private_file(&temporary, false).context("create a checkpoint")?;
    file.write_all(snapshot).context("write a checkpoint")?;
    file.sync_data().context("sync a checkpoint")?;
    let log = private_file(&dir.join(format!("log-{n}")), false).context("create a log")?;
    fs::rename(&temporary, dir.join(format!("checkpoint-{n}"))).context("commit a checkpoint")?;
    for old in generations(dir).into_iter().filter(|old| *old < n) {
        let _ = fs::remove_file(dir.join(format!("checkpoint-{old}")));
        let _ = fs::remove_file(dir.join(format!("log-{old}")));
    }
    Ok(log)
}

fn append(log: &mut Option<File>, tag: u8, bytes: &[u8]) {
    let Some(file) = log else {
        return;
    };
    let mut record = Vec::with_capacity(bytes.len() + 5);
    record.push(tag);
    record.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    record.extend_from_slice(bytes);
    if file.write_all(&record).is_err() {
        // The next checkpoint starts a new log.
        *log = None;
    }
}

/// The newest complete generation of a history: its checkpoint and its log.
/// A log cut short by a crash ends at its last whole record.
pub fn read(dir: &Path) -> Result<Option<(Vec<u8>, Vec<Entry>)>> {
    let Some(n) = generations(dir).last().copied() else {
        return Ok(None);
    };
    let checkpoint = fs::read(dir.join(format!("checkpoint-{n}")))
        .with_context(|| format!("read the checkpoint in {}", dir.display()))?;
    let mut bytes = Vec::new();
    if let Ok(mut file) = File::open(dir.join(format!("log-{n}"))) {
        file.read_to_end(&mut bytes).context("read the log")?;
    }
    let mut entries = Vec::new();
    let mut at = 0;
    while at + 5 <= bytes.len() {
        let tag = bytes[at];
        let len = u32::from_le_bytes([bytes[at + 1], bytes[at + 2], bytes[at + 3], bytes[at + 4]])
            as usize;
        let Some(body) = bytes.get(at + 5..at + 5 + len) else {
            break;
        };
        entries.push(match tag {
            GAP => Entry::Gap,
            _ => Entry::Output(body.to_vec()),
        });
        at += 5 + len;
    }
    Ok(Some((checkpoint, entries)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn a_new_checkpoint_starts_a_new_log_and_removes_the_old_generation() {
        let dir = std::env::temp_dir().join(format!("twyx-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut history = History::start(dir.clone()).expect("start");
        history.checkpoint(b"first".to_vec());
        history.output(b"one");
        history.output(b"two");
        wait_for(|| {
            read(&dir)
                .ok()
                .flatten()
                .is_some_and(|(_, log)| log.len() == 2)
        });
        assert_eq!(
            read(&dir).expect("read"),
            Some((
                b"first".to_vec(),
                vec![
                    Entry::Output(b"one".to_vec()),
                    Entry::Output(b"two".to_vec())
                ]
            ))
        );

        history.changed = true;
        history.checkpoint(b"second".to_vec());
        history.output(b"three");
        wait_for(|| {
            read(&dir)
                .ok()
                .flatten()
                .is_some_and(|(c, l)| c == b"second" && l.len() == 1)
        });
        assert_eq!(generations(&dir), [2]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_log_cut_short_ends_at_its_last_whole_record() {
        let dir = std::env::temp_dir().join(format!("twyx-history-cut-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        create_dir(&dir).expect("create");
        fs::write(dir.join("checkpoint-3"), b"snap").expect("write");
        let mut log = vec![OUTPUT, 2, 0, 0, 0, b'h', b'i', GAP, 0, 0, 0, 0];
        log.extend_from_slice(&[OUTPUT, 9, 0, 0, 0, b'x']);
        fs::write(dir.join("log-3"), log).expect("write");
        assert_eq!(
            read(&dir).expect("read"),
            Some((
                b"snap".to_vec(),
                vec![Entry::Output(b"hi".to_vec()), Entry::Gap]
            ))
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
