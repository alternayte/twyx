//! Measures keystroke-to-screen latency through a client's own PTY.
//!
//! The harness is the outer terminal: it writes a key into the client's PTY and
//! times how long the echo takes to come back. Nothing polls a screen capture,
//! so the number is the client's own latency plus the PTY round trip.
//!
//!     twyx-bench --samples 20 -- twyx
//!     twyx-bench --samples 20 -- zellij

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const COLS: u16 = 120;
const ROWS: u16 = 40;

#[derive(Debug)]
struct Sample {
    latency: Duration,
}

fn main() {
    let mut samples = 20;
    let mut command = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--samples" => {
                if let Some(value) = arguments.next().and_then(|value| value.parse().ok()) {
                    samples = value;
                }
            }
            "--" => {
                command.extend(arguments);
                break;
            }
            other => command.push(other.to_owned()),
        }
    }
    if command.is_empty() {
        eprintln!("usage: twyx-bench [--samples N] -- <client command>");
        std::process::exit(2);
    }

    match run(&command, samples) {
        Ok(result) => {
            println!(
                "{}: p50 {:.1} ms, p95 {:.1} ms over {} samples",
                command.join(" "),
                millis(percentile(&result, 50)),
                millis(percentile(&result, 95)),
                result.len()
            );
        }
        Err(error) => {
            eprintln!("twyx-bench: {error:#}");
            std::process::exit(1);
        }
    }
}

fn run(command: &[String], samples: usize) -> anyhow::Result<Vec<Sample>> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: ROWS,
        cols: COLS,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    // Every run gets its own daemon and panes, or a leftover pane from an
    // earlier run becomes the thing being measured.
    let root = std::env::temp_dir().join(format!("twyx-bench-{}", std::process::id()));
    let runtime = root.join("run/twyx");
    std::fs::create_dir_all(&runtime)?;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir_all(root.join("state"))?;
    std::fs::create_dir_all(root.join("config"))?;

    let mut builder = CommandBuilder::new(&command[0]);
    builder.args(&command[1..]);
    builder.env("XDG_RUNTIME_DIR", root.join("run"));
    builder.env("XDG_STATE_HOME", root.join("state"));
    builder.env("XDG_CONFIG_HOME", root.join("config"));
    let mut child = pair.slave.spawn_command(builder)?;
    drop(pair.slave);

    let mut writer = pair.master.take_writer()?;
    let mut reader = pair.master.try_clone_reader()?;
    let (output, received) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
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

    // A cell-drawing client sends one run per changed cell, so the marker has
    // to be one character that cannot already be on screen. Section sign is
    // not in a path, a prompt or a program name.
    const MARKER: &[u8] = "\u{a7}".as_bytes();

    // The first frame means the client is up; the shell prompt follows.
    wait_for_any(&received, Duration::from_secs(20))
        .map_err(|error| anyhow::anyhow!("the client never drew: {error}"))?;
    std::thread::sleep(Duration::from_millis(1500));

    let mut results = Vec::with_capacity(samples);
    for _ in 0..samples {
        // Anything still arriving belongs to the previous sample.
        while received.try_recv().is_ok() {}
        let started = Instant::now();
        writer.write_all(MARKER)?;
        writer.flush()?;
        wait_for(&received, MARKER, Duration::from_secs(10))?;
        results.push(Sample {
            latency: started.elapsed(),
        });
        writer.write_all(b"\x7f")?;
        writer.flush()?;
        std::thread::sleep(Duration::from_millis(40));
    }

    let _ = child.kill();
    if let Ok(pid) = std::fs::read_to_string(runtime.join("twyx.sock")) {
        let _ = pid;
    }
    let _ = std::process::Command::new(&command[0])
        .arg("daemon")
        .arg("stop")
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .output();
    let _ = std::fs::remove_dir_all(&root);
    Ok(results)
}

fn wait_for(
    received: &std::sync::mpsc::Receiver<Vec<u8>>,
    needle: &[u8],
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut seen: Vec<u8> = Vec::new();
    while Instant::now() < deadline {
        match received.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                seen.extend_from_slice(&chunk);
                if contains(&seen, needle) {
                    return Ok(());
                }
                // Only the tail matters for a marker.
                if seen.len() > 4096 {
                    let tail = seen.len() - 2048;
                    seen.drain(..tail);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the client exited")
            }
        }
    }
    anyhow::bail!(
        "timed out waiting for {:?}; last output {:?}",
        String::from_utf8_lossy(needle),
        String::from_utf8_lossy(&seen[seen.len().saturating_sub(160)..])
    )
}

fn wait_for_any(
    received: &std::sync::mpsc::Receiver<Vec<u8>>,
    timeout: Duration,
) -> anyhow::Result<()> {
    match received.recv_timeout(timeout) {
        Ok(_) => Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => anyhow::bail!("timed out"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("the client exited"),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn percentile(samples: &[Sample], percent: usize) -> Duration {
    let mut sorted: Vec<Duration> = samples.iter().map(|sample| sample.latency).collect();
    sorted.sort();
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = (sorted.len() - 1) * percent / 100;
    sorted[index]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Keeps the sender type in one place for the reader thread.
#[allow(dead_code)]
fn channel_type(_: Sender<Vec<u8>>) {}
