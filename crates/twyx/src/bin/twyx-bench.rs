//! Measures twyx, Zellij and Herdr from the outer terminal's side.
//!
//! The harness is the outer terminal: it writes keys into the client's PTY and
//! reads what the client draws. Nothing polls a screen capture, so each number
//! is the multiplexer's own cost plus the PTY round trip.
//!
//!     twyx-bench --samples 20 --twyx ./target/release/twyx zellij herdr
//!
//! For each client it reports keystroke-to-screen latency, the throughput of a
//! large `cat`, peak memory under a full-speed producer, and CPU and memory
//! while idle with ten panes. Memory and CPU count the multiplexer's own
//! processes (client, server, holders), never the shells in the panes.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const COLS: u16 = 120;
const ROWS: u16 = 40;
const PANES: usize = 10;
const CAT_BYTES: usize = 32 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(10);
const PRODUCER: Duration = Duration::from_secs(10);

// A cell-drawing client sends one run per changed cell, so the marker has to
// be one character that cannot already be on screen. Section sign is not in a
// path, a prompt or a program name.
const MARKER: &[u8] = "\u{a7}".as_bytes();
// The shell prints the marker; the typed command line holds only its escape.
const PRINT_MARKER: &str = "printf '\\302\\247\\n'";

#[derive(Debug, Clone)]
enum Client {
    Twyx(PathBuf),
    Zellij,
    Herdr,
}

struct Report {
    name: String,
    version: String,
    latency_p50: Duration,
    latency_p95: Duration,
    throughput_mib_s: f64,
    producer_peak_kib: u64,
    idle_cpu_percent: f64,
    idle_kib: u64,
}

fn main() {
    let mut samples = 20;
    let mut clients = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--samples" => {
                if let Some(value) = arguments.next().and_then(|value| value.parse().ok()) {
                    samples = value;
                }
            }
            "--twyx" => {
                // The PTY child starts in the home directory, so a relative
                // path would not resolve.
                if let Some(path) = arguments.next() {
                    let path = std::fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(path));
                    clients.push(Client::Twyx(path));
                }
            }
            "zellij" => clients.push(Client::Zellij),
            "herdr" => clients.push(Client::Herdr),
            other => {
                eprintln!("twyx-bench: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if clients.is_empty() {
        eprintln!("usage: twyx-bench [--samples N] [--twyx PATH] [zellij] [herdr]");
        std::process::exit(2);
    }

    println!("{}", machine());
    let mut failed = false;
    for client in clients {
        match measure(&client, samples) {
            Ok(report) => print(&report),
            Err(error) => {
                failed = true;
                eprintln!("twyx-bench: {}: {error:#}", client.name());
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

impl Client {
    fn name(&self) -> &'static str {
        match self {
            Client::Twyx(_) => "twyx",
            Client::Zellij => "zellij",
            Client::Herdr => "herdr",
        }
    }

    fn program(&self) -> String {
        match self {
            Client::Twyx(path) => path.display().to_string(),
            Client::Zellij => "zellij".to_owned(),
            Client::Herdr => "herdr".to_owned(),
        }
    }

    fn version(&self) -> String {
        Command::new(self.program())
            .arg("--version")
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default()
    }

    /// The process name its client, server and holders run under.
    fn process_name(&self) -> &'static str {
        self.name()
    }

    fn command(&self, run: &Run) -> Vec<String> {
        let mut argv = vec![self.program()];
        if let Client::Zellij = self {
            argv.extend(
                [
                    "--session",
                    &run.session,
                    "options",
                    "--show-release-notes",
                    "false",
                    "--show-startup-tips",
                    "false",
                    "--session-serialization",
                    "false",
                ]
                .map(str::to_owned),
            );
        }
        argv
    }

    /// Writes the config a first run needs to skip its welcome screens.
    fn prepare(&self, run: &Run) -> anyhow::Result<()> {
        if let Client::Herdr = self {
            let dir = run.root.join("config/herdr");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("config.toml"), "onboarding = false\n")?;
        }
        Ok(())
    }

    fn add_pane(&self, run: &Run, index: usize) -> anyhow::Result<()> {
        let mut command = run.command(&self.program());
        match self {
            Client::Twyx(_) => command.args(["pane", "new"]),
            Client::Zellij => command.args(["--session", &run.session, "action", "new-pane"]),
            Client::Herdr => command.args([
                "pane",
                "split",
                "w1:p1",
                "--direction",
                if index.is_multiple_of(2) {
                    "right"
                } else {
                    "down"
                },
            ]),
        };
        let output = command.output().context("start the pane command")?;
        if !output.status.success() {
            bail!(
                "adding a pane failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn stop(&self, run: &Run) {
        let mut command = run.command(&self.program());
        match self {
            Client::Twyx(_) => command.args(["daemon", "stop"]),
            Client::Zellij => command.args(["kill-session", &run.session]),
            Client::Herdr => command.args(["server", "stop"]),
        };
        let _ = command.output();
    }
}

/// One client's isolated directories.
struct Run {
    root: PathBuf,
    session: String,
}

impl Run {
    fn new(client: &Client) -> anyhow::Result<Self> {
        // Short and fixed: Herdr keeps its socket under XDG_CONFIG_HOME, and
        // macOS $TMPDIR paths overflow the 104-byte socket path limit.
        let root = PathBuf::from(format!(
            "/tmp/twyx-bench-{}-{}",
            std::process::id(),
            client.name()
        ));
        let runtime = root.join("run");
        std::fs::create_dir_all(&runtime)?;
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
        std::fs::create_dir_all(root.join("state"))?;
        std::fs::create_dir_all(root.join("config"))?;
        Ok(Self {
            session: format!("twyx-bench-{}", std::process::id()),
            root,
        })
    }

    fn env(&self) -> [(&'static str, PathBuf); 3] {
        [
            ("XDG_RUNTIME_DIR", self.root.join("run")),
            ("XDG_STATE_HOME", self.root.join("state")),
            ("XDG_CONFIG_HOME", self.root.join("config")),
        ]
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.envs(self.env());
        command
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The client's PTY, seen from the outer terminal.
struct Terminal {
    writer: Box<dyn Write + Send>,
    received: Receiver<Vec<u8>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Terminal {
    fn start(argv: &[String], run: &Run) -> anyhow::Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut builder = CommandBuilder::new(&argv[0]);
        builder.args(&argv[1..]);
        for (key, value) in run.env() {
            builder.env(key, value);
        }
        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;
        let (output, received) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            // Keeps the master open for as long as the reader runs.
            let _master = pair.master;
            let mut buffer = [0_u8; 65536];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if output.send(buffer[..count].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            writer,
            received,
            child,
        })
    }

    fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        self.writer.write_all(text.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    fn drain(&self) {
        while self.received.try_recv().is_ok() {}
    }

    /// Reads until `needle` appears, keeping only a tail of what it saw.
    fn wait_for(&self, needle: &[u8], timeout: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut seen: Vec<u8> = Vec::new();
        while Instant::now() < deadline {
            match self.received.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    seen.extend_from_slice(&chunk);
                    if contains(&seen, needle) {
                        return Ok(());
                    }
                    if seen.len() > 8192 {
                        seen.drain(..seen.len() - 4096);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("the client exited"),
            }
        }
        bail!(
            "timed out waiting for {:?}; last output {:?}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&seen[seen.len().saturating_sub(160)..])
        )
    }

    fn wait_for_any(&self, timeout: Duration) -> anyhow::Result<()> {
        match self.received.recv_timeout(timeout) {
            Ok(_) => Ok(()),
            Err(RecvTimeoutError::Timeout) => bail!("the client never drew"),
            Err(RecvTimeoutError::Disconnected) => bail!("the client exited"),
        }
    }
}

fn measure(client: &Client, samples: usize) -> anyhow::Result<Report> {
    let run = Run::new(client)?;
    client.prepare(&run)?;
    let big = run.root.join("big.txt");
    write_big_file(&big)?;

    let before = processes(client.process_name())?
        .into_iter()
        .map(|process| process.pid)
        .collect::<HashSet<_>>();
    let mut terminal = Terminal::start(&client.command(&run), &run)?;
    let result = measure_in(client, &run, &mut terminal, samples, &big, &before);

    let _ = terminal.child.kill();
    client.stop(&run);
    // Whatever the run started and the stop left behind.
    if let Ok(left) = processes(client.process_name()) {
        for process in left.iter().filter(|process| !before.contains(&process.pid)) {
            let _ = Command::new("kill")
                .arg("-9")
                .arg(process.pid.to_string())
                .output();
        }
    }
    result
}

fn measure_in(
    client: &Client,
    run: &Run,
    terminal: &mut Terminal,
    samples: usize,
    big: &Path,
    before: &HashSet<u32>,
) -> anyhow::Result<Report> {
    terminal.wait_for_any(Duration::from_secs(20))?;
    // The first frame means the client is up; the shell prompt follows.
    std::thread::sleep(Duration::from_millis(2000));

    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        terminal.drain();
        let started = Instant::now();
        terminal.writer.write_all(MARKER)?;
        terminal.writer.flush()?;
        terminal.wait_for(MARKER, Duration::from_secs(10))?;
        latencies.push(started.elapsed());
        terminal.type_text("\x7f")?;
        std::thread::sleep(Duration::from_millis(40));
    }
    latencies.sort();

    terminal.drain();
    terminal.type_text(&format!("cat {}; {PRINT_MARKER}\r", big.display()))?;
    let started = Instant::now();
    terminal
        .wait_for(MARKER, Duration::from_secs(300))
        .context("the large cat never finished")?;
    let throughput_mib_s = CAT_BYTES as f64 / (1024.0 * 1024.0) / started.elapsed().as_secs_f64();
    terminal.type_text("clear\r")?;
    std::thread::sleep(Duration::from_millis(500));

    terminal.drain();
    terminal.type_text("yes twyx-bench-producer\r")?;
    let mut producer_peak_kib = 0;
    let deadline = Instant::now() + PRODUCER;
    while Instant::now() < deadline {
        // Keep the client's output flowing; a full PTY would stall the producer.
        terminal.drain();
        producer_peak_kib = producer_peak_kib.max(total(client, before)?.rss_kib);
        std::thread::sleep(Duration::from_millis(100));
    }
    terminal.type_text("\x03")?;
    std::thread::sleep(Duration::from_millis(500));
    terminal.drain();
    terminal.type_text(&format!("clear; {PRINT_MARKER}\r"))?;
    terminal.wait_for(MARKER, Duration::from_secs(30))?;

    for index in 1..PANES {
        client.add_pane(run, index)?;
    }
    std::thread::sleep(Duration::from_secs(3));
    terminal.drain();
    let start = total(client, before)?;
    let started = Instant::now();
    while started.elapsed() < IDLE {
        terminal.drain();
        std::thread::sleep(Duration::from_millis(200));
    }
    let end = total(client, before)?;
    let idle_cpu_percent =
        100.0 * end.cpu.saturating_sub(start.cpu).as_secs_f64() / started.elapsed().as_secs_f64();

    Ok(Report {
        name: client.name().to_owned(),
        version: client.version(),
        latency_p50: percentile(&latencies, 50),
        latency_p95: percentile(&latencies, 95),
        throughput_mib_s,
        producer_peak_kib,
        idle_cpu_percent,
        idle_kib: end.rss_kib,
    })
}

fn write_big_file(path: &Path) -> anyhow::Result<()> {
    let line = format!("{}\n", "twyx-bench ".repeat(7).trim_end());
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut written = 0;
    while written < CAT_BYTES {
        file.write_all(line.as_bytes())?;
        written += line.len();
    }
    file.flush()?;
    Ok(())
}

struct Process {
    pid: u32,
    rss_kib: u64,
    cpu: Duration,
}

struct Total {
    rss_kib: u64,
    cpu: Duration,
}

/// The multiplexer processes this run started.
fn total(client: &Client, before: &HashSet<u32>) -> anyhow::Result<Total> {
    let mut total = Total {
        rss_kib: 0,
        cpu: Duration::ZERO,
    };
    for process in processes(client.process_name())? {
        if before.contains(&process.pid) {
            continue;
        }
        total.rss_kib += process.rss_kib;
        total.cpu += process.cpu;
    }
    Ok(total)
}

fn processes(name: &str) -> anyhow::Result<Vec<Process>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,rss=,time=,args="])
        .output()
        .context("run ps")?;
    let mut found = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(rss), Some(time), Some(program)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(name)
        {
            continue;
        }
        let (Ok(pid), Ok(rss_kib), Some(cpu)) = (pid.parse(), rss.parse(), cpu_time(time)) else {
            continue;
        };
        found.push(Process { pid, rss_kib, cpu });
    }
    Ok(found)
}

/// Parses `ps` cumulative CPU time: `[[dd-]hh:]mm:ss[.cc]`.
fn cpu_time(text: &str) -> Option<Duration> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<f64>().ok()?, clock),
        None => (0.0, text),
    };
    let mut seconds = 0.0;
    for part in clock.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(Duration::from_secs_f64(days * 86_400.0 + seconds))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[(sorted.len() - 1) * percent / 100]
}

fn machine() -> String {
    let uname = Command::new("uname")
        .arg("-srm")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    format!("machine: {uname}, {} cpus", num_cpus())
}

fn num_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

fn print(report: &Report) {
    println!(
        "{} ({}): latency p50 {:.1} ms, p95 {:.1} ms; cat {:.1} MiB/s; \
         producer peak {:.1} MiB; idle with {PANES} panes {:.2}% cpu, {:.1} MiB",
        report.name,
        report.version,
        report.latency_p50.as_secs_f64() * 1000.0,
        report.latency_p95.as_secs_f64() * 1000.0,
        report.throughput_mib_s,
        report.producer_peak_kib as f64 / 1024.0,
        report.idle_cpu_percent,
        report.idle_kib as f64 / 1024.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_reads_every_ps_format() {
        assert_eq!(cpu_time("0:00.03"), Some(Duration::from_millis(30)));
        assert_eq!(cpu_time("01:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(
            cpu_time("2-00:00:01"),
            Some(Duration::from_secs(2 * 86_400 + 1))
        );
        assert_eq!(cpu_time("n/a"), None);
    }
}
