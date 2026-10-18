//! The input queue.
//!
//! Every byte written to a pane passes through its holder's input queue, on a
//! thread that the parser never waits for.

use std::io::Write;
use std::sync::mpsc::{Sender, channel};
use std::thread;

use anyhow::{Context, Result};

/// Writes bytes to a pane's terminal on its own thread.
#[derive(Debug, Clone)]
pub struct InputQueue {
    sender: Sender<Vec<u8>>,
}

impl InputQueue {
    pub fn start(mut writer: Box<dyn Write + Send>) -> Result<Self> {
        let (sender, receiver) = channel::<Vec<u8>>();
        thread::Builder::new()
            .name("twyx-input".to_owned())
            .spawn(move || {
                for bytes in receiver {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                    // A pane that does not read keeps its queue; dropping a
                    // write silently would corrupt its input state.
                    if writer.flush().is_err() {
                        break;
                    }
                }
            })
            .context("start the input thread")?;
        Ok(Self { sender })
    }

    pub fn send(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        // The queue is unbounded, so the caller never waits for the pane.
        let _ = self.sender.send(bytes);
    }
}
