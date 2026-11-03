//! Text documents opened in full: Markdown and JSON files. A read takes the file as it is at that moment; nothing is
//! kept between reads.

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use crate::ViewerError;
use crate::json::JsonDocument;
use crate::markdown::MarkdownDocument;

/// The format of a document, decided by its file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocumentKind {
    Markdown,
    Json,
}

impl DocumentKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
        }
    }
}

/// The kind of document a path holds: `.md`, `.markdown` or `.json`, in any letter case.
pub fn document_kind(path: &Path) -> Option<DocumentKind> {
    let extension = path.extension()?;
    if extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown") {
        Some(DocumentKind::Markdown)
    } else if extension.eq_ignore_ascii_case("json") {
        Some(DocumentKind::Json)
    } else {
        None
    }
}

/// A parsed document.
#[derive(Clone, Debug)]
pub enum Document {
    Markdown(MarkdownDocument),
    Json(JsonDocument),
}

/// A file's size and modification time. Two equal stamps mean the file has not been rewritten between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub fn file_stamp(path: &Path) -> io::Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    Ok(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Reads and parses a whole file read-only. A UTF-8 byte order mark is dropped.
pub fn read_document(path: &Path, kind: DocumentKind) -> Result<Document, ViewerError> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|error| ViewerError::InvalidText {
        offset: error.utf8_error().valid_up_to(),
    })?;
    parse_document(text, kind)
}

fn parse_document(mut text: String, kind: DocumentKind) -> Result<Document, ViewerError> {
    if text.starts_with('\u{feff}') {
        text.drain(..'\u{feff}'.len_utf8());
    }
    match kind {
        DocumentKind::Markdown => Ok(Document::Markdown(MarkdownDocument::parse(&text))),
        DocumentKind::Json => JsonDocument::parse(text)
            .map(Document::Json)
            .map_err(|error| ViewerError::InvalidJson {
                line: error.line,
                column: error.column,
                reason: error.reason,
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_kinds_by_extension() {
        for (path, kind) in [
            ("a.md", Some(DocumentKind::Markdown)),
            ("A.MARKDOWN", Some(DocumentKind::Markdown)),
            ("b.Json", Some(DocumentKind::Json)),
            ("c.jsonl", None),
            ("d.parquet", None),
            ("md", None),
        ] {
            assert_eq!(document_kind(Path::new(path)), kind, "{path}");
        }
    }

    #[test]
    fn reads_documents_with_a_byte_order_mark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json");
        fs::write(&path, "\u{feff}{\"키\": 1}").unwrap();
        let Document::Json(json) = read_document(&path, DocumentKind::Json).unwrap() else {
            panic!("expected JSON");
        };
        assert_eq!(json.child_count(json.root()), 1);
    }

    #[test]
    fn stamps_change_when_a_file_is_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "하나").unwrap();
        let first = file_stamp(&path).unwrap();
        assert_eq!(first, file_stamp(&path).unwrap());
        fs::write(&path, "하나 둘").unwrap();
        assert_ne!(first, file_stamp(&path).unwrap());
        assert!(file_stamp(&dir.path().join("없음.md")).is_err());
    }

    #[test]
    fn reports_invalid_text_and_json_with_the_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, b"ab\xff").unwrap();
        assert!(matches!(
            read_document(&path, DocumentKind::Markdown),
            Err(ViewerError::InvalidText { offset: 2 })
        ));
        let path = dir.path().join("a.json");
        fs::write(&path, "{\n  \"키\": x\n}").unwrap();
        assert!(matches!(
            read_document(&path, DocumentKind::Json),
            Err(ViewerError::InvalidJson {
                line: 2,
                column: 8,
                reason: "expected a value"
            })
        ));
        assert!(matches!(
            read_document(&dir.path().join("없음.md"), DocumentKind::Markdown),
            Err(ViewerError::Io(_))
        ));
    }
}
