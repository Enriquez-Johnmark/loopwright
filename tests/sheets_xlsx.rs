//! Excel files are parsed in a child process with time and memory limits,
//! no environment and a cap on its output, so a crafted file can't take the
//! server down or reach its secrets.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nudgewright::sheets::{MAX_BYTES, MAX_ROWS, SheetError, XlsxReader};
use zip::write::SimpleFileOptions;

/// The engine's stand-alone copy of the child; products run the child from
/// their own binary.
fn reader() -> XlsxReader {
    XlsxReader::new(PathBuf::from(env!("CARGO_BIN_EXE_nudgewright-xlsx-child")))
}

/// A minimal .xlsx, with cell style 1 formatting dates, whose first sheet's XML is `sheet_data` (with any
/// `<dimension>` given before it).
fn xlsx(dimension: &str, sheet_data: &str) -> Vec<u8> {
    let mut file = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let parts = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#.to_owned(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_owned(),
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_owned(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_owned(),
        ),
        (
            "xl/styles.xml",
            // Cell style 1 is Excel's built-in date format (number format 14).
            r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#.to_owned(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{dimension}<sheetData>{sheet_data}</sheetData></worksheet>"#
            ),
        ),
    ];
    for (name, content) in parts {
        file.start_file(name, SimpleFileOptions::default()).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }
    file.finish().unwrap().into_inner()
}

fn row(n: usize, cells: &[&str]) -> String {
    let cells: String = cells
        .iter()
        .zip('A'..)
        .map(|(text, column)| format!(r#"<c r="{column}{n}" t="inlineStr"><is><t>{text}</t></is></c>"#))
        .collect();
    format!(r#"<row r="{n}">{cells}</row>"#)
}

/// An executable script in `dir`, standing in for a parser a crafted file
/// has taken over.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn the_first_sheet_is_read_header_first() {
    let sheet = [
        row(1, &["Name", "Email"]),
        row(2, &["Reyes Creative", "ana@reyes.test"]),
    ]
    .concat();

    let cells = reader().cells(&xlsx("", &sheet)).await.unwrap();

    assert_eq!(cells, [["Name", "Email"], ["Reyes Creative", "ana@reyes.test"]]);
}

#[tokio::test]
async fn reading_stops_one_row_past_the_limit() {
    let sheet: String = (1..=MAX_ROWS + 10).map(|n| row(n, &["x"])).collect();

    let cells = reader().cells(&xlsx("", &sheet)).await.unwrap();

    assert_eq!(cells.len(), MAX_ROWS + 2);
}

#[tokio::test]
async fn a_file_that_is_not_excel_is_unreadable() {
    let err = reader().cells(b"Name,Email\n").await.unwrap_err();

    assert!(matches!(err, SheetError::Unreadable(_)), "got {err:?}");
}

#[tokio::test]
async fn a_file_over_the_limit_is_refused_without_starting_the_child() {
    let err = XlsxReader::new(PathBuf::from("/nonexistent"))
        .cells(&vec![0; MAX_BYTES + 1])
        .await
        .unwrap_err();

    assert_eq!(err, SheetError::TooLarge);
}

// One test, so the scripts are written before any child process starts.
#[tokio::test]
async fn a_hijacked_parser_gets_no_secrets_and_cant_flood_the_server() {
    let dir = std::env::temp_dir().join(format!("nudgewright-sandbox-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let env_file = dir.join("child.env");
    let snoop = script(&dir, "snoop", &format!("env > '{}'; echo '[]'", env_file.display()));
    let flood = script(&dir, "flood", "head -c 100000000 /dev/zero");
    assert!(
        std::env::var("DATABASE_URL").is_ok(),
        "the test itself has secrets to leak"
    );

    XlsxReader::new(snoop).cells(b"PK").await.unwrap();
    let flooded = XlsxReader::new(flood).cells(b"PK").await;

    let child_env = std::fs::read_to_string(&env_file).unwrap();
    assert!(!child_env.contains("DATABASE_URL"), "{child_env}");
    assert!(matches!(flooded, Err(SheetError::Unreadable(_))), "{flooded:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_date_cell_reads_as_an_iso_date() {
    let sheet = [
        row(1, &["Due date"]),
        r#"<row r="2"><c r="A2" s="1"><v>46341</v></c></row>"#.to_owned(),
    ]
    .concat();

    let cells = reader().cells(&xlsx("", &sheet)).await.unwrap();

    assert_eq!(cells[1], ["2026-11-15"]);
}

#[tokio::test]
async fn a_sheet_starting_at_column_b_keeps_its_columns() {
    let sheet = r#"<row r="1"><c r="B1" t="inlineStr"><is><t>Name</t></is></c></row>"#;

    let cells = reader().cells(&xlsx("", sheet)).await.unwrap();

    assert_eq!(cells[0], ["", "Name"]);
}
