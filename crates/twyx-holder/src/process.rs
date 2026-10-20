//! The pane's foreground process, by the name agent kinds match.

/// The arguments a process runs with, as it shows them now.
#[cfg(target_os = "linux")]
fn argv(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// The arguments a process runs with, as it shows them now. `ps` joins them
/// with spaces, so a path with a space splits; the name is still its last
/// part's.
#[cfg(not(target_os = "linux"))]
fn argv(pid: u32) -> Option<Vec<String>> {
    let output = std::process::Command::new("ps")
        .args(["-ww", "-o", "args=", "-p", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
    )
}

/// The name of process `pid`, or `None` when it is gone.
pub fn name(pid: u32) -> Option<String> {
    let argv = argv(pid)?;
    if argv.is_empty() {
        return None;
    }
    Some(twyx_core::agent::process_name(&argv))
}
