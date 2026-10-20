//! `git diff` output, parsed into files and hunks, and patches made again
//! from chosen hunks for `git apply`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`, about the line before it.
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@` line, with any section heading.
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<Line>,
}

impl Hunk {
    /// The hunk's text as `git diff` printed it.
    fn text(&self) -> String {
        let mut text = format!("{}\n", self.header);
        for line in &self.lines {
            let mark = match line.kind {
                LineKind::Context => ' ',
                LineKind::Added => '+',
                LineKind::Removed => '-',
                LineKind::NoNewline => '\\',
            };
            text.push(mark);
            text.push_str(&line.text);
            text.push('\n');
        }
        text
    }

    /// An id that names this hunk's content wherever it sits in the file:
    /// FNV-1a over its lines, not its line numbers.
    pub fn id(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for line in &self.lines {
            for byte in [line.kind as u8]
                .into_iter()
                .chain(line.text.bytes())
                .chain(*b"\n")
            {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
        }
        hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// The path before a rename.
    pub old_path: Option<String>,
    pub status: Status,
    /// A binary file: no hunks, only the whole file's patch.
    pub binary: bool,
    /// Every line before the first hunk: `diff --git`, modes, `index`,
    /// `---` and `+++`, or the binary patch.
    header: String,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// A patch with the hunks whose ids are in `ids`, or every hunk.
    pub fn patch(&self, ids: Option<&[u64]>) -> String {
        let mut patch = self.header.clone();
        for hunk in &self.hunks {
            if ids.is_none_or(|ids| ids.contains(&hunk.id())) {
                patch.push_str(&hunk.text());
            }
        }
        patch
    }
}

/// Parses `git diff` output made with `--no-color`, `--no-ext-diff` and
/// `--no-textconv`. Paths with spaces or quotes come from the `---` and
/// `+++` lines, which Git quotes.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            files.push(FileDiff {
                path: String::new(),
                old_path: None,
                status: Status::Modified,
                binary: false,
                header: String::new(),
                hunks: Vec::new(),
            });
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if let Some(hunk) = file.hunks.last_mut()
            && !line.starts_with("@@")
        {
            let (kind, rest) = match line.chars().next() {
                Some('+') => (LineKind::Added, &line[1..]),
                Some('-') => (LineKind::Removed, &line[1..]),
                Some('\\') => (LineKind::NoNewline, &line[1..]),
                Some(' ') => (LineKind::Context, &line[1..]),
                // An empty context line, from a diff tool that trims it.
                None => (LineKind::Context, ""),
                Some(_) => continue,
            };
            hunk.lines.push(Line {
                kind,
                text: rest.to_owned(),
            });
            continue;
        }
        if let Some(header) = line.strip_prefix("@@") {
            let (old_start, new_start) = ranges(header);
            file.hunks.push(Hunk {
                header: line.to_owned(),
                old_start,
                new_start,
                lines: Vec::new(),
            });
            continue;
        }
        file.header.push_str(line);
        file.header.push('\n');
        if line.starts_with("new file mode") {
            file.status = Status::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = Status::Deleted;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = Status::Renamed;
            file.old_path = Some(unquote(from));
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.path = unquote(to);
        } else if line.starts_with("GIT binary patch") || line.starts_with("Binary files ") {
            file.binary = true;
        } else if let Some(new) = line.strip_prefix("+++ ") {
            if new != "/dev/null" {
                file.path = strip_side(&unquote(new));
            }
        } else if let Some(old) = line.strip_prefix("--- ")
            && old != "/dev/null"
            && file.path.is_empty()
        {
            file.path = strip_side(&unquote(old));
        } else if let Some(rest) = line.strip_prefix("diff --git ")
            && file.path.is_empty()
        {
            // A binary or mode-only change has no `---` line; the second
            // half of an unquoted `a/x b/x` names it.
            let half = rest.len() / 2;
            file.path = strip_side(&unquote(rest[half..].trim()));
        }
    }
    files
}

fn ranges(header: &str) -> (u32, u32) {
    let mut starts = header.split_whitespace().take(2).map(|range| {
        range
            .trim_start_matches(['-', '+'])
            .split(',')
            .next()
            .and_then(|start| start.parse().ok())
            .unwrap_or(0)
    });
    (starts.next().unwrap_or(0), starts.next().unwrap_or(0))
}

fn strip_side(path: &str) -> String {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_owned()
}

/// A path as Git printed it: C-quoted when it holds special characters.
fn unquote(path: &str) -> String {
    let Some(inner) = path
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
    else {
        return path.to_owned();
    };
    let mut bytes = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buffer = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some(digit @ '0'..='7') => {
                let mut value = digit.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    if let Some(next) = chars.peek().and_then(|next| next.to_digit(8)) {
                        value = value * 8 + next;
                        chars.next();
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => {
                let mut buffer = [0; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
            }
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@ fn main() {
 one
-two
+TWO
 three
@@ -10,2 +10,3 @@
 ten
+ten and a half
 eleven
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+fresh
\\ No newline at end of file
diff --git \"a/sp ace.txt\" \"b/sp ace.txt\"
deleted file mode 100644
index 4444444..0000000
--- \"a/sp ace.txt\"
+++ /dev/null
@@ -1 +0,0 @@
-gone
diff --git a/img.png b/img.png
index 5555555..6666666 100644
GIT binary patch
literal 1
Ic${M1000H70RR91

literal 1
Ic${M2000H80RR91

";

    #[test]
    fn a_diff_parses_into_files_and_hunks_and_a_hunk_patch_is_made_again() {
        let files = parse(DIFF);
        let summary: Vec<(&str, Status, usize, bool)> = files
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.status,
                    file.hunks.len(),
                    file.binary,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("src/a.rs", Status::Modified, 2, false),
                ("new.txt", Status::Added, 1, false),
                ("sp ace.txt", Status::Deleted, 1, false),
                ("img.png", Status::Modified, 0, true),
            ]
        );
        let second = files[0].hunks[1].id();
        assert_eq!(
            files[0].patch(Some(&[second])),
            "diff --git a/src/a.rs b/src/a.rs\nindex 1111111..2222222 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,2 +10,3 @@\n ten\n+ten and a half\n eleven\n"
        );
        assert_eq!(files[0].hunks[1].old_start, 10);
        assert_eq!(files[1].hunks[0].lines[1].kind, LineKind::NoNewline);
        assert_eq!(
            files[3].patch(None),
            DIFF[DIFF.find("diff --git a/img").unwrap()..]
        );
        assert_ne!(files[0].hunks[0].id(), second);
    }
}
