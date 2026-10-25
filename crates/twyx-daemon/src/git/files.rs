//! A worktree's files and the lines that hold a query, under Invariant 10.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use twyx_protocol::client::Hit;

use super::{background, command_line, run};

/// The tracked files that are still there and the untracked files that are
/// not ignored, sorted, at most `limit`; whether there were more.
pub(crate) fn list(worktree: &Path, limit: usize) -> Result<(Vec<String>, bool)> {
    let out = run(background(
        worktree,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
        ],
    ))?;
    let mut paths: Vec<String> = out
        .split('\0')
        .filter(|path| !path.is_empty() && worktree.join(path).is_file())
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    let truncated = paths.len() > limit;
    paths.truncate(limit);
    Ok((paths, truncated))
}

/// The lines of the tracked and untracked, not ignored, text files that
/// hold `query` as it is written, ignoring case when it has no capital;
/// at most `limit`, and whether there were more.
pub(crate) fn grep(
    worktree: &Path,
    query: &str,
    limit: usize,
    line_chars: usize,
) -> Result<(Vec<Hit>, bool)> {
    let mut args = vec![
        "grep",
        "-z",
        "-n",
        "-I",
        "--no-color",
        "--untracked",
        "--fixed-strings",
    ];
    if !query.chars().any(char::is_uppercase) {
        args.push("-i");
    }
    args.extend(["-e", query]);
    let mut command = background(worktree, &args);
    let line = command_line(&command);
    let mut child = command
        .spawn()
        .with_context(|| format!("run {line}; is Git installed?"))?;
    let stdout = child.stdout.take().context("git grep has no output")?;
    let mut hits = Vec::new();
    let mut truncated = false;
    for record in BufReader::new(stdout).split(b'\n') {
        let record = record.with_context(|| format!("read {line}"))?;
        // `path NUL number NUL text`
        let mut parts = record.splitn(3, |byte| *byte == 0);
        let (Some(path), Some(number), Some(text)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if hits.len() == limit {
            truncated = true;
            break;
        }
        let Ok(number) = String::from_utf8_lossy(number).parse::<u32>() else {
            continue;
        };
        hits.push(Hit {
            path: String::from_utf8_lossy(path).into_owned(),
            line: number,
            text: String::from_utf8_lossy(text)
                .chars()
                .take(line_chars)
                .collect(),
        });
    }
    if truncated {
        let _ = child.kill();
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let status = child.wait().with_context(|| format!("wait for {line}"))?;
    // 1 is "nothing found".
    if !truncated && !status.success() && status.code() != Some(1) {
        return Err(super::GitError {
            command: line,
            stderr,
        }
        .into());
    }
    Ok((hits, truncated))
}
