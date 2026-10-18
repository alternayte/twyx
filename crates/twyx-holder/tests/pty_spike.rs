//! Spike: the pane child runs in a new session, and the PTY it was given is
//! its controlling terminal.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Read;

use portable_pty::Child;
use twyx_holder::pty::{Master, Pty, Spawn};

const SCRIPT: &str = r#"printf 'tty:%s\n' "$(tty)"; \
if printf '' >/dev/tty 2>/dev/null; then printf 'ctty:yes\n'; else printf 'ctty:no\n'; fi; \
ps -o pid=,pgid=,tpgid= -p $$"#;

fn spawn_probe() -> (Master, Box<dyn Child + Send + Sync>, Box<dyn Read + Send>) {
    let pty = Pty::spawn(&Spawn {
        argv: vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()],
        cwd: std::env::temp_dir(),
        env: Vec::new(),
        cols: 80,
        rows: 24,
    })
    .expect("spawn the probe");
    let (master, child) = pty.split();
    let reader = master.reader().expect("clone the pty reader");
    (master, child, reader)
}

fn read_to_end(reader: &mut dyn Read) -> String {
    let mut out = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // EIO once every slave descriptor is closed, which is the
            // normal end of a pty stream on Linux.
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn process_ids(output: &str) -> (i64, i64, i64) {
    let line = output
        .lines()
        .find(|line| line.split_whitespace().count() == 3)
        .expect("a ps line with pid, pgid and tpgid");
    let mut fields = line.split_whitespace().map(|field| {
        field
            .parse::<i64>()
            .unwrap_or_else(|_| panic!("{field} is not a process id"))
    });
    (
        fields.next().expect("pid"),
        fields.next().expect("pgid"),
        fields.next().expect("tpgid"),
    )
}

#[test]
fn pane_child_runs_in_a_new_session_with_its_pty_as_controlling_terminal() {
    let (_master, mut child, mut reader) = spawn_probe();
    let output = read_to_end(reader.as_mut());
    child.wait().expect("the probe exits");

    assert!(
        output.contains("ctty:yes"),
        "the child has no controlling terminal: {output:?}"
    );

    let tty = output
        .lines()
        .find_map(|line| line.strip_prefix("tty:"))
        .expect("a tty line");
    assert!(
        tty.starts_with("/dev/") && tty != "/dev/tty",
        "the child is not attached to a pty: {tty:?}"
    );

    let (pid, pgid, tpgid) = process_ids(&output);
    assert_eq!(pid, pgid, "not the leader of its own process group");
    assert_eq!(pid, tpgid, "not the foreground group of its terminal");
}
