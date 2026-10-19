//! Pane identity.

use std::fmt;
use std::str::FromStr;

const BASE: u64 = 36;
const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// One pane. The daemon assigns the raw value from the database row id, and
/// zero is never a valid pane.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct PaneId(u64);

impl PaneId {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The base36 form used for the holder socket and the history directory.
    ///
    /// Short enough that a socket path stays under the 104-byte limit on macOS.
    pub fn stem(self) -> String {
        let mut digits = [0_u8; 13];
        let mut index = digits.len();
        let mut value = self.0;
        loop {
            index -= 1;
            digits[index] = DIGITS[(value % BASE) as usize];
            value /= BASE;
            if value == 0 {
                break;
            }
        }
        String::from_utf8_lossy(&digits[index..]).into_owned()
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.stem())
    }
}

impl FromStr for PaneId {
    type Err = PaneIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.is_empty() {
            return Err(PaneIdError);
        }
        let mut value = 0_u64;
        for byte in text.bytes() {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'z' => byte - b'a' + 10,
                _ => return Err(PaneIdError),
            };
            value = value
                .checked_mul(BASE)
                .and_then(|value| value.checked_add(u64::from(digit)))
                .ok_or(PaneIdError)?;
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneIdError;

impl fmt::Display for PaneIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a pane id")
    }
}

impl std::error::Error for PaneIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_id_round_trips_through_its_stem() {
        for raw in [1, 2, 35, 36, 1295, 1296, 1_000_000, u64::MAX] {
            let id = PaneId::from_raw(raw);
            assert_eq!(PaneId::from_str(&id.stem()), Ok(id), "raw {raw}");
        }
    }

    #[test]
    fn pane_id_stem_stays_short_enough_for_a_socket_path() {
        for raw in [1, 1_000_000, u64::MAX] {
            assert!(PaneId::from_raw(raw).stem().len() <= 13);
        }
    }

    #[test]
    fn pane_id_rejects_what_is_not_a_pane_id() {
        assert!(PaneId::from_str("").is_err());
        assert!(PaneId::from_str("Twyx").is_err());
        assert!(PaneId::from_str("zzzzzzzzzzzzzz").is_err());
    }
}
