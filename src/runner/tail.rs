//! Reading a file another process is still appending to, line by line.

use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

/// How long to wait before looking again at a file that has nothing new.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct FileTail {
    reader: BufReader<tokio::fs::File>,
    /// A line read up to the end of the file so far, waiting for the rest.
    partial: Vec<u8>,
}

impl FileTail {
    /// # Errors
    ///
    /// When the file cannot be opened.
    pub fn open(path: &Path) -> std::io::Result<FileTail> {
        Ok(FileTail {
            reader: BufReader::new(tokio::fs::File::from_std(std::fs::File::open(path)?)),
            partial: Vec::new(),
        })
    }

    /// The next complete line, waiting for more while `still_writing` says
    /// the writer is alive; `None` once it is gone and the file is drained.
    /// A last line with no newline is returned as it is.
    ///
    /// `still_writing` is asked before each read, so a writer that finishes
    /// between a read and the question cannot strand its last lines: the
    /// next pass reads knowing it is gone, and drains.
    ///
    /// A loop: each read either finds a line or waits and reads again.
    pub async fn next_line(&mut self, mut still_writing: impl FnMut() -> bool) -> Option<String> {
        loop {
            let writer_alive = still_writing();
            match self.reader.read_until(b'\n', &mut self.partial).await {
                Ok(_) if self.partial.ends_with(b"\n") => {
                    let line = std::mem::take(&mut self.partial);
                    return Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
                }
                Ok(0) | Err(_) if !writer_alive => {
                    return (!self.partial.is_empty()).then(|| {
                        String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned()
                    });
                }
                Ok(_) | Err(_) => tokio::time::sleep(POLL).await,
            }
        }
    }
}
