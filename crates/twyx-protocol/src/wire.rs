//! One message on a socket: a four-byte length and a postcard body.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

use twyx_core::tuning;

/// Encodes a frame or snapshot body that the daemon relays without reading.
pub fn encode_body<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let body = postcard::to_stdvec(message).context("encode a body")?;
    if body.len() > tuning::MAX_MESSAGE_BYTES {
        bail!(
            "body of {} bytes exceeds the {} byte limit",
            body.len(),
            tuning::MAX_MESSAGE_BYTES
        );
    }
    Ok(body)
}

/// Decodes a frame or snapshot body a holder published.
pub fn decode_body<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    postcard::from_bytes(body).context("decode a body")
}

pub fn write<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<()> {
    let body = postcard::to_stdvec(message).context("encode a message")?;
    if body.len() > tuning::MAX_MESSAGE_BYTES {
        bail!(
            "message of {} bytes exceeds the {} byte limit",
            body.len(),
            tuning::MAX_MESSAGE_BYTES
        );
    }
    let len = u32::try_from(body.len()).context("message length does not fit in 32 bits")?;
    writer
        .write_all(&len.to_be_bytes())
        .and_then(|()| writer.write_all(&body))
        .and_then(|()| writer.flush())
        .context("write a message")
}

/// Reads one message, or `None` when the peer closed the stream cleanly.
pub fn read<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let mut len = [0_u8; 4];
    match reader.read(&mut len[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            return read(reader);
        }
        Err(e) => return Err(e).context("read a message length"),
    }
    reader
        .read_exact(&mut len[1..])
        .context("read a message length")?;

    let len = u32::from_be_bytes(len) as usize;
    if len > tuning::MAX_MESSAGE_BYTES {
        bail!(
            "message of {len} bytes exceeds the {} byte limit",
            tuning::MAX_MESSAGE_BYTES
        );
    }

    let mut body = vec![0_u8; len];
    reader
        .read_exact(&mut body)
        .context("read a message body")?;
    let message = postcard::from_bytes(&body).context("decode a message")?;
    Ok(Some(message))
}
