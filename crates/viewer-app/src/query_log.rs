//! Query log: every query applied to a tab, one JSON line each, in a file per day such as `2026-11-02.jsonl`.
//!
//! Lines are only ever appended. A line holds the local time, the source file and table, where the query came from,
//! the whole query text, and the resulting row count or the error. The folder is the app's own folder, never the
//! folder of the data, and the user deletes old files when they want to.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{Local, SecondsFormat};

/// Where a query came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Mouse actions: context menus, header clicks and the filter and sort bar.
    Click,
    /// The query input window.
    Window,
    /// The message file.
    File,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Click => "click",
            Origin::Window => "window",
            Origin::File => "file",
        }
    }
}

/// One applied query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source: PathBuf,
    pub table: Option<String>,
    pub origin: Origin,
    /// The whole query text, or the text as received when it did not parse.
    pub query: String,
    /// Rows of the resulting row view.
    pub rows: Option<u64>,
    pub error: Option<String>,
}

pub struct QueryLog {
    dir: PathBuf,
}

impl QueryLog {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends the entry to today's file.
    pub fn append(&self, entry: &Entry) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let now = Local::now();
        let line = serde_json::json!({
            "time": now.to_rfc3339_opts(SecondsFormat::Secs, false),
            "source": entry.source.display().to_string(),
            "table": entry.table,
            "origin": entry.origin.as_str(),
            "query": entry.query,
            "rows": entry.rows,
            "error": entry.error,
        });
        let path = self.dir.join(format!("{}.jsonl", now.format("%Y-%m-%d")));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{line}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_one_json_line_per_query() {
        let dir = tempfile::tempdir().unwrap();
        let log = QueryLog::new(dir.path().join("query-log"));
        let entry = Entry {
            source: PathBuf::from(r"C:\data\운행.parquet"),
            table: None,
            origin: Origin::Window,
            query: "filter route_name == \"중구01\"\nsort -id".into(),
            rows: Some(1081),
            error: None,
        };
        log.append(&entry).unwrap();
        log.append(&Entry {
            table: Some("edges".into()),
            origin: Origin::File,
            query: "filter a or b".into(),
            rows: None,
            error: Some("line 1, column 10: `or` is not supported".into()),
            ..entry.clone()
        })
        .unwrap();
        let files: Vec<_> = fs::read_dir(log.dir()).unwrap().collect();
        assert_eq!(files.len(), 1);
        let text = fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["origin"], "window");
        assert_eq!(
            lines[0]["query"],
            "filter route_name == \"중구01\"\nsort -id"
        );
        assert_eq!(lines[0]["rows"], 1081);
        assert!(lines[0]["table"].is_null() && lines[0]["error"].is_null());
        assert_eq!(lines[1]["table"], "edges");
        assert_eq!(lines[1]["origin"], "file");
        assert!(lines[1]["rows"].is_null());
        assert!(lines[1]["time"].as_str().unwrap().len() >= 25);
    }
}
