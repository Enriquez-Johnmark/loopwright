//! Virus scanning with ClamAV. Files are streamed to its daemon, clamd,
//! with the INSTREAM command; the answer is read strictly, so anything but
//! a clear "OK" or "FOUND" is an error the caller must refuse on.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long a scan may take, connection included. A 10 MB file takes about
/// a second.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes per INSTREAM chunk.
const CHUNK_BYTES: usize = 64 * 1024;
/// clamd's answers are one short line.
const MAX_ANSWER_BYTES: u64 = 1024;

/// What clamd found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No virus found.
    Clean,
    /// A virus, by ClamAV's name for it.
    Infected(String),
}

/// Why a file couldn't be scanned.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// clamd couldn't be reached, or the connection broke.
    #[error("clamd unreachable: {0}")]
    Unreachable(#[from] std::io::Error),
    /// clamd didn't answer in time.
    #[error("clamd didn't answer in time")]
    TimedOut,
    /// clamd answered something other than a verdict.
    #[error("clamd answered {0:?}")]
    Unexpected(String),
}

/// A clamd reached over TCP.
#[derive(Debug, Clone)]
pub struct Clamd {
    addr: String,
    timeout: Duration,
}

impl Clamd {
    /// clamd listening at `addr` (`host:port`).
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// The same clamd, giving up on a scan after `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Scans `file`.
    pub async fn scan(&self, file: &[u8]) -> Result<Verdict, ScanError> {
        let answer = tokio::time::timeout(self.timeout, self.instream(file))
            .await
            .map_err(|_| ScanError::TimedOut)??;
        verdict(&answer)
    }

    async fn instream(&self, file: &[u8]) -> Result<String, ScanError> {
        let mut socket = TcpStream::connect(&self.addr).await?;
        socket.write_all(b"zINSTREAM\0").await?;
        for chunk in file.chunks(CHUNK_BYTES) {
            let length = u32::try_from(chunk.len()).map_err(|_| ScanError::Unexpected("chunk too long".to_owned()))?;
            socket.write_all(&length.to_be_bytes()).await?;
            socket.write_all(chunk).await?;
        }
        socket.write_all(&0u32.to_be_bytes()).await?;
        // clamd answers one NUL-terminated line, then closes the connection.
        let mut answer = Vec::new();
        socket.take(MAX_ANSWER_BYTES).read_to_end(&mut answer).await?;
        Ok(String::from_utf8_lossy(&answer)
            .trim_end_matches(['\0', '\n'])
            .to_owned())
    }
}

fn verdict(answer: &str) -> Result<Verdict, ScanError> {
    let unexpected = || ScanError::Unexpected(answer.to_owned());
    let result = answer.strip_prefix("stream: ").ok_or_else(unexpected)?;
    if result == "OK" {
        return Ok(Verdict::Clean);
    }
    match result.strip_suffix(" FOUND") {
        Some(name) if !name.is_empty() => Ok(Verdict::Infected(name.to_owned())),
        _ => Err(unexpected()),
    }
}
