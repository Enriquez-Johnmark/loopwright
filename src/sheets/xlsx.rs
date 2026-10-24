//! Excel (.xlsx) files. The parser can abort or exhaust memory on a crafted
//! file, so it runs in a child process that limits its own memory and CPU
//! time, starts with no environment, and can only answer in JSON of bounded
//! size. A product starts the child from its own binary: `main` calls
//! [`run_child`] first thing when its first argument is [`CHILD_COMMAND`].

use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use calamine::{Data, Reader, Xlsx};
use rustix::process::{Resource, Rlimit, setrlimit};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{MAX_BYTES, MAX_ROWS, SheetError};

/// The argument that makes a product's binary run the child.
pub const CHILD_COMMAND: &str = "parse-xlsx";

/// The child's address-space limit.
const MEMORY_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
/// The child's CPU-time limit.
const CPU_LIMIT_SECONDS: u64 = 10;
/// The most JSON read back from the child.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
/// How long to wait for the child.
const WAIT_LIMIT: Duration = Duration::from_secs(15);

/// Reads .xlsx files through the child process.
#[derive(Debug, Clone)]
pub struct XlsxReader {
    program: PathBuf,
}

impl XlsxReader {
    /// A reader that starts `program` with [`CHILD_COMMAND`] as the child:
    /// normally the product's own binary.
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    /// The first sheet's cells, header row first, up to one row past
    /// [`MAX_ROWS`].
    pub async fn cells(&self, bytes: &[u8]) -> Result<Vec<Vec<String>>, SheetError> {
        if bytes.len() > MAX_BYTES {
            return Err(SheetError::TooLarge);
        }
        let unreadable = || SheetError::Unreadable("it isn't a readable Excel (.xlsx) file".to_owned());
        // No environment: a parser taken over by a crafted file finds no
        // database URL or API keys.
        let mut child = tokio::process::Command::new(&self.program)
            .arg(CHILD_COMMAND)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| SheetError::Unreadable(format!("the Excel reader didn't start: {err}")))?;
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        let talk = async {
            if let Some(mut stdin) = stdin {
                // A child that dies early closes the pipe; its exit status says why.
                let _ = stdin.write_all(bytes).await;
            }
            let mut output = Vec::new();
            if let Some(stdout) = stdout {
                stdout.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut output).await?;
            }
            if output.len() as u64 > MAX_OUTPUT_BYTES {
                // Dropping the child kills it.
                return Ok(None);
            }
            Ok::<_, std::io::Error>(Some((output, child.wait().await?)))
        };
        let (output, status) = tokio::time::timeout(WAIT_LIMIT, talk)
            .await
            .map_err(|_| SheetError::Unreadable("it took too long to read".to_owned()))?
            .map_err(|err| SheetError::Unreadable(err.to_string()))?
            .ok_or_else(unreadable)?;
        if !status.success() {
            return Err(unreadable());
        }
        serde_json::from_slice(&output).map_err(|_| unreadable())
    }
}

/// The child: limits itself, reads a file on stdin and writes the first
/// sheet's cells to stdout as JSON. Call it before starting threads or
/// connecting to anything, so the limits cover nothing but the parsing.
pub fn run_child() -> ExitCode {
    for (resource, limit) in [(Resource::As, MEMORY_LIMIT_BYTES), (Resource::Cpu, CPU_LIMIT_SECONDS)] {
        // Some systems (macOS) refuse an address-space limit; the reader's
        // wait limit still applies there.
        let _ = setrlimit(
            resource,
            Rlimit {
                current: Some(limit),
                maximum: Some(limit),
            },
        );
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_BYTES).unwrap_or(u64::MAX);
    if std::io::stdin().take(limit).read_to_end(&mut bytes).is_err() {
        return ExitCode::from(2);
    }
    let Some(cells) = cells_from_xlsx(bytes) else {
        return ExitCode::from(2);
    };
    match serde_json::to_writer(std::io::stdout().lock(), &cells) {
        Ok(()) => std::io::stdout()
            .flush()
            .map_or(ExitCode::from(2), |()| ExitCode::SUCCESS),
        Err(_) => ExitCode::from(2),
    }
}

/// The first sheet's rows from row 1, stopping once there are more
/// non-blank rows than [`MAX_ROWS`] allows.
fn cells_from_xlsx(bytes: Vec<u8>) -> Option<Vec<Vec<String>>> {
    let mut workbook = Xlsx::new(Cursor::new(bytes)).ok()?;
    let range = workbook.worksheet_range_at(0)?.ok()?;
    // The range starts at its first non-empty cell; pad back to A1 so
    // positions match the spreadsheet's rows and columns.
    let (first_row, first_column) = range.start().unwrap_or((0, 0));
    let mut cells: Vec<Vec<String>> = (0..first_row).map(|_| Vec::new()).collect();
    let mut non_blank = 0;
    for row in range.rows() {
        let mut texts: Vec<String> = std::iter::repeat_n(String::new(), first_column as usize)
            .chain(row.iter().map(text))
            .collect();
        while texts.last().is_some_and(String::is_empty) {
            texts.pop();
        }
        non_blank += usize::from(!texts.is_empty());
        cells.push(texts);
        // The header, the allowed rows, and one more to report "too many".
        if non_blank > MAX_ROWS + 1 {
            break;
        }
    }
    Some(cells)
}

/// A cell as text. Dates read as ISO dates (`2026-11-15`, or
/// `2026-11-15 13:05:00` with a time), not as Excel's serial numbers.
fn text(cell: &Data) -> String {
    match cell {
        Data::DateTime(date) if date.is_datetime() => {
            let (year, month, day, hour, minute, second, _) = date.to_ymd_hms_milli();
            if (hour, minute, second) == (0, 0, 0) {
                format!("{year:04}-{month:02}-{day:02}")
            } else {
                format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
            }
        }
        other => other.to_string(),
    }
}
