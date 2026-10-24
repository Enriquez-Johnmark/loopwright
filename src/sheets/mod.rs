//! Reading uploaded spreadsheets (CSV and Excel) into rows of text. Each
//! product checks the columns itself; the engine only turns an untrusted
//! file into cells, within size limits, without letting a crafted file take
//! the server down.

mod xlsx;

pub use xlsx::{CHILD_COMMAND, XlsxReader, run_child};

/// The largest file accepted, in bytes.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
/// The most non-blank rows read from a file, besides its header. Products
/// may allow fewer; one more is read so they can say "too many rows".
pub const MAX_ROWS: usize = 500;

/// Why a file couldn't be read.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SheetError {
    /// Larger than [`MAX_BYTES`].
    #[error("the file is larger than 2 MB")]
    TooLarge,
    /// Not a readable CSV or Excel file.
    #[error("couldn't read the file: {0}")]
    Unreadable(String),
}

/// A CSV file's records, each with its row number as a spreadsheet shows
/// it (the first line is row 1). The file may be UTF-8, with or without a
/// byte-order mark, or Windows-1252 as older Excel saves it. Reading stops
/// one row past [`MAX_ROWS`].
pub fn read_csv(bytes: &[u8]) -> Result<Vec<(usize, Vec<String>)>, SheetError> {
    if bytes.len() > MAX_BYTES {
        return Err(SheetError::TooLarge);
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
    };
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut records = Vec::new();
    // Rows as a spreadsheet numbers them: blank lines count (the csv crate
    // skips them), line breaks inside a quoted cell don't, and \r, \n and
    // \r\n all end a line.
    let (mut counted_to, mut line, mut breaks_in_previous) = (0, 1, 0);
    for record in reader.records() {
        let record = record.map_err(|err| SheetError::Unreadable(err.to_string()))?;
        // Its position can point at blank lines before it.
        let start = record.position().map_or(0, |p| usize::try_from(p.byte()).unwrap_or(0));
        let bytes = text.as_bytes();
        let start = start
            + bytes
                .iter()
                .skip(start)
                .take_while(|b| matches!(b, b'\r' | b'\n'))
                .count();
        line += line_breaks(bytes.get(counted_to..start).unwrap_or_default()).saturating_sub(breaks_in_previous);
        counted_to = start;
        breaks_in_previous = record.iter().map(|cell| line_breaks(cell.as_bytes())).sum();
        records.push((line, record.iter().map(str::to_owned).collect()));
        // The header, the allowed rows, and one more to report "too many",
        // as for Excel files.
        if records.len() > MAX_ROWS + 1 {
            break;
        }
    }
    Ok(records)
}

/// A sheet's rows numbered from 1, header first, as [`read_csv`] numbers
/// them.
pub fn numbered(cells: Vec<Vec<String>>) -> Vec<(usize, Vec<String>)> {
    cells.into_iter().enumerate().map(|(i, row)| (i + 1, row)).collect()
}

/// A header name reduced to lowercase ASCII letters and digits, for
/// matching columns whatever their spacing, case or punctuation.
pub fn squash(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Line breaks in `bytes`, counting `\r\n` once.
fn line_breaks(bytes: &[u8]) -> usize {
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
    let lone_returns = bytes
        .iter()
        .enumerate()
        .filter(|&(i, &b)| b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))
        .count();
    newlines + lone_returns
}
