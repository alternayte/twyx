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
    /// The lines the hunk changes on one side, without its context, on a
    /// doubled scale: line `n` is `2n` and the gap before it `2n - 1`, so a
    /// change that only adds or only removes has a place on the other side.
    pub fn span(&self, new: bool) -> (u64, u64) {
        let (start, mine, theirs) = if new {
            (self.new_start, LineKind::Added, LineKind::Removed)
        } else {
            (self.old_start, LineKind::Removed, LineKind::Added)
        };
        let has_lines = self
            .lines
            .iter()
            .any(|line| matches!(line.kind, LineKind::Context) || line.kind == mine);
        // A side with no lines names the line the change comes after.
        let mut at = u64::from(start) + u64::from(!has_lines);
        let mut span: Option<(u64, u64)> = None;
        let mut take = |pos: u64| {
            span = Some(span.map_or((pos, pos), |(low, high)| (low.min(pos), high.max(pos))));
        };
        for line in &self.lines {
            if line.kind == LineKind::Context {
                at += 1;
            } else if line.kind == mine {
                take(2 * at);
                at += 1;
            } else if line.kind == theirs {
                take((2 * at).saturating_sub(1));
            }
        }
        span.unwrap_or((2 * at, 2 * at))
    }

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

/// Where the lines `first..=last` of the old file are in the new one, for a
/// diff made with `-U0`: shifted past the hunks before them, or `None` when
/// a hunk changes any of them.
pub fn move_lines(first: u32, last: u32, hunks: &[Hunk]) -> Option<(u32, u32)> {
    let mut shift: i64 = 0;
    for hunk in hunks {
        let removed = hunk
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::Removed)
            .count() as u32;
        let added = hunk
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::Added)
            .count() as u32;
        // A pure insertion's old start is the line it comes after.
        let (start, end) = if removed == 0 {
            (hunk.old_start + 1, hunk.old_start)
        } else {
            (hunk.old_start, hunk.old_start + removed - 1)
        };
        if removed > 0 && start <= last && end >= first {
            return None;
        }
        if removed == 0 && start > first && start <= last {
            return None;
        }
        if end < first || (removed == 0 && start <= first) {
            shift += i64::from(added) - i64::from(removed);
        }
    }
    let moved = |line: u32| u32::try_from(i64::from(line) + shift).ok();
    Some((moved(first)?, moved(last)?))
}

/// Whether two spans from `Hunk::span` share a line or a gap.
pub fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// Where an old-side span of a `-U0` diff's preimage lies in its postimage:
/// shifted past the hunks wholly before it.
pub fn carry_span(span: (u64, u64), hunks: &[Hunk]) -> (u64, u64) {
    let mut shift: i64 = 0;
    for hunk in hunks {
        if hunk.span(false).1 < span.0 {
            let count = |kind: LineKind| {
                i64::try_from(hunk.lines.iter().filter(|line| line.kind == kind).count())
                    .unwrap_or(0)
            };
            shift += 2 * (count(LineKind::Added) - count(LineKind::Removed));
        }
    }
    let carry = |pos: u64| u64::try_from(i64::try_from(pos).unwrap_or(0) + shift).unwrap_or(0);
    (carry(span.0), carry(span.1))
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

    #[test]
    fn a_note_moves_past_changes_above_it_and_is_outdated_by_changes_to_it() {
        let hunks = |text: &str| {
            parse(&format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n{text}"))[0]
                .hunks
                .clone()
        };
        // Two lines added after line 2.
        let added = hunks("@@ -2,0 +3,2 @@\n+a\n+b\n");
        assert_eq!(move_lines(5, 6, &added), Some((7, 8)));
        assert_eq!(move_lines(1, 2, &added), Some((1, 2)));
        assert_eq!(
            move_lines(2, 4, &added),
            None,
            "lines added inside the range"
        );
        // Line 4 changed, line 9 removed.
        let changed = hunks("@@ -4 +4 @@\n-x\n+y\n@@ -9 +8,0 @@\n-z\n");
        assert_eq!(move_lines(4, 4, &changed), None);
        assert_eq!(move_lines(10, 11, &changed), Some((9, 10)));
        assert_eq!(move_lines(5, 8, &changed), Some((5, 8)));
    }

    #[test]
    fn spans_place_changes_on_either_side_and_carry_past_earlier_hunks() {
        let context = &parse(DIFF)[0].hunks;
        // `-two +TWO` at line 2 on both sides; the removal sits before line 2.
        assert_eq!(context[0].span(true), (3, 4));
        assert_eq!(context[0].span(false), (4, 5));
        // An addition after old line 10 is new line 11, and a gap at old 11.
        assert_eq!(context[1].span(true), (22, 22));
        assert_eq!(context[1].span(false), (21, 21));

        let zero = parse(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2,0 +3,2 @@\n+x\n+y\n@@ -5 +6,0 @@\n-gone\n@@ -9 +9 @@\n-old\n+new\n",
        );
        let hunks = &zero[0].hunks;
        assert_eq!(hunks[0].span(true), (6, 8), "two lines added at 3 and 4");
        assert_eq!(hunks[0].span(false), (5, 5), "after old line 2");
        assert_eq!(hunks[1].span(true), (13, 13), "removed after new line 6");
        assert_eq!(hunks[1].span(false), (10, 10));
        // Old line 7 is past +2 and -1: new line 8.
        assert_eq!(carry_span((14, 14), hunks), (16, 16));
        assert_eq!(carry_span((2, 2), hunks), (2, 2), "before every hunk");
        assert!(overlaps((3, 4), (4, 8)));
        assert!(!overlaps((3, 4), (5, 8)));
    }
}
