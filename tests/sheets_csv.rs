//! Reading CSV files into rows numbered as a spreadsheet shows them.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use nudgewright::sheets::{MAX_BYTES, MAX_ROWS, SheetError, read_csv, squash};

#[test]
fn rows_carry_their_cells() {
    let rows = read_csv(b"Name,Email\nReyes Creative,ana@reyes.test\n").unwrap();

    assert_eq!(
        rows,
        [
            (1, vec!["Name".to_owned(), "Email".to_owned()]),
            (2, vec!["Reyes Creative".to_owned(), "ana@reyes.test".to_owned()]),
        ]
    );
}

#[test]
fn a_byte_order_mark_is_dropped() {
    let rows = read_csv(b"\xef\xbb\xbfName\nReyes\n").unwrap();

    assert_eq!(rows[0].1, ["Name"]);
}

#[test]
fn windows_1252_text_is_read() {
    let rows = read_csv(b"Name\nPe\xf1a Studio\n").unwrap();

    assert_eq!(rows[1].1, ["Peña Studio"]);
}

#[test]
fn blank_lines_count_towards_row_numbers() {
    let rows = read_csv(b"Name\n\nReyes\n").unwrap();

    assert_eq!(rows[1].0, 3);
}

#[test]
fn a_line_break_inside_a_quoted_cell_keeps_the_numbering() {
    let rows = read_csv(b"Name,Note\n\"Reyes\",\"two\nlines\"\nNext,x\n").unwrap();

    let numbers: Vec<usize> = rows.iter().map(|(row, _)| *row).collect();
    assert_eq!(numbers, [1, 2, 3]);
}

#[test]
fn a_file_over_the_limit_is_refused() {
    let big = vec![b'a'; MAX_BYTES + 1];

    assert_eq!(read_csv(&big).unwrap_err(), SheetError::TooLarge);
}

#[test]
fn squash_keeps_lowercase_letters_and_digits() {
    assert_eq!(squash("*Contact Name (2)"), "contactname2");
}

#[test]
fn reading_stops_one_row_past_the_limit() {
    let csv = "x\n".repeat(MAX_ROWS + 10);

    let rows = read_csv(csv.as_bytes()).unwrap();

    assert_eq!(rows.len(), MAX_ROWS + 2);
}
