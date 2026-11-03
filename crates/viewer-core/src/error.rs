use std::error::Error;
use std::io;

use parquet::errors::ParquetError;

/// Why an engine operation failed. The app builds the message shown to the user from this variant.
#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    /// The file could not be opened or its size read. Includes missing paths and permission errors.
    #[error("file I/O error: {0}")]
    Io(#[from] io::Error),

    /// The footer or metadata is not valid Parquet, or cannot be converted to an Arrow schema.
    #[error("Parquet metadata error: {0}")]
    InvalidParquet(#[from] ParquetError),

    /// A Row Group has a negative row count, or the cumulative row count exceeds `u64`.
    #[error("invalid Row Group row count")]
    InvalidRowCount,

    /// The requested session is not the session of the currently open file.
    #[error("request for a previous file session")]
    StaleSession,

    /// A row range or column ID violated the request contract.
    #[error("invalid request: {0}")]
    InvalidRequest(&'static str),

    /// Decompressing or decoding a data page failed. Includes unsupported codecs.
    #[error("data decoding error: {0}")]
    Decode(Box<dyn Error + Send + Sync>),

    /// The file's size or modification time changed after it was opened.
    #[error("the file changed after it was opened")]
    FileChanged,

    /// A filter value cannot be parsed as the column type, or the operator does not apply to the column type.
    #[error("{0}")]
    InvalidCondition(String),

    /// The column type cannot be sorted, or the number of rows to sort exceeds `u32`.
    #[error("{0}")]
    Unsortable(String),

    /// A SQLite database could not be opened or queried, or the table does not exist.
    #[error("database error: {0}")]
    Database(String),

    /// A Markdown or JSON file is not UTF-8. `offset` is the first byte that is not.
    #[error("not UTF-8 text (byte {offset})")]
    InvalidText { offset: usize },

    /// A JSON file does not parse. Line and character column are one-based.
    #[error("not valid JSON: line {line}, column {column}: {reason}")]
    InvalidJson {
        line: usize,
        column: usize,
        reason: &'static str,
    },

    /// The cancellation flag was set and the work stopped.
    #[error("the operation was cancelled")]
    Cancelled,
}
