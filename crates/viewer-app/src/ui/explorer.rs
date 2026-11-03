//! Folder explorer. Lists only the direct children of expanded folders and databases, and draws only the
//! visible rows. Parquet files, SQLite databases, and Markdown and JSON documents are shown; a database expands into its
//! tables and views.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gpui::{
    ClickEvent, Context, Div, EventEmitter, FocusHandle, SharedString, Stateful, Window, actions,
    div, prelude::*, uniform_list,
};

use viewer_core::{
    DataSource, DocumentKind, SqliteObjectKind, document_kind, is_sqlite_file, sqlite_objects,
};
use viewer_ui::theme;

use super::style;

actions!(explorer, [OpenSelected]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    /// Parquet file.
    File,
    /// SQLite database. Expands into its tables and views.
    Database,
    /// Markdown or JSON file.
    Document(DocumentKind),
    /// Table or view of the database at the entry's path.
    Table {
        table: String,
        view: bool,
    },
}

impl EntryKind {
    /// Directories come before files and databases.
    fn rank(&self) -> u8 {
        match self {
            Self::Directory => 0,
            _ => 1,
        }
    }

    /// Directories and databases expand; files and tables open.
    fn expands(&self) -> bool {
        matches!(self, Self::Directory | Self::Database)
    }
}

/// Explorer entry: a directory, a Parquet file, a SQLite database (`.sqlite`, `.sqlite3`, `.db`, `.db3`,
/// `.s3db`, `.sl3` with a SQLite header) or one of its tables, or a Markdown or JSON document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Symbolic link or directory junction. Linked directories are not expanded.
    pub is_link: bool,
}

/// Reads the direct children of a directory, sorting directories first and by name within each kind.
///
/// Links are only checked for their target's kind and never followed. Dangling links and per-entry
/// read errors are skipped.
pub fn list_directory(dir: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let (kind, is_link) = if file_type.is_symlink() {
            match fs::metadata(&path) {
                Ok(target) if target.is_dir() => (EntryKind::Directory, true),
                Ok(target) if target.is_file() => (EntryKind::File, true),
                _ => continue,
            }
        } else if file_type.is_dir() {
            (EntryKind::Directory, false)
        } else if file_type.is_file() {
            (EntryKind::File, false)
        } else {
            continue;
        };
        let kind = match kind {
            EntryKind::File if is_parquet(&path) => EntryKind::File,
            EntryKind::File if is_database(&path) => EntryKind::Database,
            EntryKind::File => match document_kind(&path) {
                Some(kind) => EntryKind::Document(kind),
                None => continue,
            },
            other => other,
        };
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            kind,
            is_link,
        });
    }
    entries.sort_by(|a, b| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then_with(|| compare_names(&a.name, &b.name))
    });
    Ok(entries)
}

/// Tables and views of a database, by name.
pub fn list_database(path: &Path) -> Result<Vec<DirEntry>, viewer_core::ViewerError> {
    Ok(sqlite_objects(path)?
        .into_iter()
        .map(|object| DirEntry {
            name: object.name.clone(),
            path: path.to_path_buf(),
            kind: EntryKind::Table {
                table: object.name,
                view: object.kind == SqliteObjectKind::View,
            },
            is_link: false,
        })
        .collect())
}

fn is_parquet(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("parquet"))
}

/// SQLite database by extension and header. The header check hides other files that use `.db`.
fn is_database(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        ["sqlite", "sqlite3", "db", "db3", "s3db", "sl3"]
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    }) && is_sqlite_file(path)
}

impl DirEntry {
    /// The source this entry opens, for Parquet files and tables.
    fn source(&self) -> Option<DataSource> {
        match &self.kind {
            EntryKind::File => Some(DataSource::Parquet(self.path.clone())),
            EntryKind::Table { table, .. } => Some(DataSource::Sqlite {
                path: self.path.clone(),
                table: table.clone(),
            }),
            EntryKind::Directory | EntryKind::Database | EntryKind::Document(_) => None,
        }
    }

    /// Identity used for selection: the path, plus the table name for tables.
    fn key(&self) -> NodeKey {
        match &self.kind {
            EntryKind::Table { table, .. } => (self.path.clone(), Some(table.clone())),
            _ => (self.path.clone(), None),
        }
    }
}

/// Path of a directory, file or database, with the table name for tables.
type NodeKey = (PathBuf, Option<String>);

/// Case-insensitive name order, falling back to the original strings on ties.
fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

pub enum ExplorerEvent {
    Open(DataSource),
    OpenDocument(PathBuf, DocumentKind),
}

enum Listing {
    Loading,
    Loaded(Vec<DirEntry>),
    Failed(SharedString),
}

/// One row of the flattened expanded tree.
struct ExplorerRow {
    depth: usize,
    content: RowContent,
}

enum RowContent {
    Entry { entry: DirEntry, expanded: bool },
    Loading,
    Failed { dir: PathBuf, message: SharedString },
    Empty { database: bool },
}

pub struct ExplorerView {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    /// Incremented whenever the root changes. Used to drop listings for the previous root.
    generation: u64,
    /// Children of expanded directories and databases, by path.
    listings: HashMap<PathBuf, Listing>,
    expanded: HashSet<PathBuf>,
    selected: Option<NodeKey>,
    rows: Vec<ExplorerRow>,
}

impl EventEmitter<ExplorerEvent> for ExplorerView {}

impl ExplorerView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            root: None,
            generation: 0,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
        }
    }

    /// Changes the root folder and expands it. Drops the previous root's expanded state and in-flight listings.
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.generation += 1;
        self.listings.clear();
        self.expanded.clear();
        self.selected = None;
        self.expanded.insert(root.clone());
        self.root = Some(root.clone());
        self.start_listing(root, cx);
        self.rebuild_rows();
        cx.notify();
    }

    /// Called when a file or table was opened by another path. Without a root, sets the file's parent folder as the
    /// root. A table's database is expanded so the table shows in the tree.
    pub fn file_activated(&mut self, path: &Path, table: Option<&str>, cx: &mut Context<Self>) {
        if self.root.is_none()
            && let Some(parent) = path.parent()
        {
            self.set_root(parent.to_path_buf(), cx);
        }
        if table.is_some() && self.expanded.insert(path.to_path_buf()) {
            self.start_listing(path.to_path_buf(), cx);
            self.rebuild_rows();
        }
        self.selected = Some((path.to_path_buf(), table.map(str::to_string)));
        cx.notify();
    }

    fn toggle(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&dir) {
            self.expanded.insert(dir.clone());
            self.start_listing(dir, cx);
        }
        self.rebuild_rows();
        cx.notify();
    }

    /// Lists the direct children of a folder, or the tables of a database, in a background task. Reuses an
    /// in-flight listing of the same path.
    fn start_listing(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if matches!(self.listings.get(&dir), Some(Listing::Loading)) {
            return;
        }
        self.listings.insert(dir.clone(), Listing::Loading);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let target = dir.clone();
            let result = cx
                .background_spawn(async move {
                    if target.is_file() {
                        list_database(&target)
                            .map_err(|err| format!("Cannot read the database. ({err})"))
                    } else {
                        list_directory(&target)
                            .map_err(|err| format!("Cannot read the folder. ({err})"))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.finish_listing(generation, dir, result, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish_listing(
        &mut self,
        generation: u64,
        dir: PathBuf,
        result: Result<Vec<DirEntry>, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        let listing = match result {
            Ok(entries) => Listing::Loaded(entries),
            Err(message) => Listing::Failed(message.into()),
        };
        self.listings.insert(dir, listing);
        self.rebuild_rows();
        cx.notify();
    }

    fn activate(&mut self, entry: &DirEntry, cx: &mut Context<Self>) {
        if let Some(source) = entry.source() {
            cx.emit(ExplorerEvent::Open(source));
        } else if let EntryKind::Document(kind) = entry.kind {
            cx.emit(ExplorerEvent::OpenDocument(entry.path.clone(), kind));
        } else if entry.kind.expands() && !entry.is_link {
            self.toggle(entry.path.clone(), cx);
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.rows.iter().find_map(|row| match &row.content {
            RowContent::Entry { entry, .. } if Some(entry.key()) == self.selected => {
                Some(entry.clone())
            }
            _ => None,
        });
        if let Some(entry) = selected {
            self.activate(&entry, cx);
        }
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        if let Some(root) = &self.root {
            let entry = DirEntry {
                name: display_name(root),
                path: root.clone(),
                kind: EntryKind::Directory,
                is_link: false,
            };
            self.push_entry(&mut rows, entry, 0);
        }
        self.rows = rows;
    }

    fn push_entry(&self, rows: &mut Vec<ExplorerRow>, entry: DirEntry, depth: usize) {
        let expanded =
            entry.kind.expands() && !entry.is_link && self.expanded.contains(&entry.path);
        let dir = entry.path.clone();
        rows.push(ExplorerRow {
            depth,
            content: RowContent::Entry { entry, expanded },
        });
        if !expanded {
            return;
        }
        let child_depth = depth + 1;
        match self.listings.get(&dir) {
            None | Some(Listing::Loading) => rows.push(ExplorerRow {
                depth: child_depth,
                content: RowContent::Loading,
            }),
            Some(Listing::Failed(message)) => rows.push(ExplorerRow {
                depth: child_depth,
                content: RowContent::Failed {
                    dir,
                    message: message.clone(),
                },
            }),
            Some(Listing::Loaded(children)) if children.is_empty() => rows.push(ExplorerRow {
                depth: child_depth,
                content: RowContent::Empty {
                    database: dir.is_file(),
                },
            }),
            Some(Listing::Loaded(children)) => {
                for child in children {
                    self.push_entry(rows, child.clone(), child_depth);
                }
            }
        }
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = &self.rows[ix];
        let base = div()
            .id(ix)
            .w_full()
            .h(theme::metrics().list_row_height)
            .pl(style::TREE_INDENT * row.depth as f32 + style::TREE_ROW_PADDING)
            .pr(style::TREE_ROW_PADDING)
            .flex()
            .items_center()
            .gap_1();
        match &row.content {
            RowContent::Entry { entry, expanded } => {
                let selected = self.selected == Some(entry.key());
                let chevron = match (entry.kind.expands(), entry.is_link, expanded) {
                    (true, false, true) => "▾",
                    (true, false, false) => "▸",
                    _ => "",
                };
                let tag = match &entry.kind {
                    _ if entry.is_link => Some("link"),
                    EntryKind::Database => Some("SQLite"),
                    EntryKind::Table { view: true, .. } => Some("view"),
                    _ => None,
                };
                let entry = entry.clone();
                base.cursor_pointer()
                    .when(selected, |row| row.bg(theme::selection()))
                    .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
                    .child(
                        div()
                            .flex_none()
                            .w(style::TREE_CHEVRON_WIDTH)
                            .text_color(theme::text_muted())
                            .child(chevron),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(entry.is_link, |name| name.text_color(theme::text_muted()))
                            .child(entry.name.clone()),
                    )
                    .when_some(tag, |row, tag| {
                        row.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(tag),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.selected = Some(entry.key());
                        // Folders and databases expand on a single click; files and tables open on a double click.
                        let activate = entry.kind.expands() || event.click_count() >= 2;
                        if activate {
                            this.activate(&entry, cx);
                        }
                        cx.notify();
                    }))
            }
            RowContent::Loading => base
                .text_color(theme::text_muted())
                .child(div().w(style::TREE_CHEVRON_WIDTH))
                .child("Reading…"),
            RowContent::Empty { database } => base
                .text_color(theme::text_muted())
                .child(div().w(style::TREE_CHEVRON_WIDTH))
                .child(if *database {
                    "No tables or views"
                } else {
                    "No Parquet, SQLite, Markdown or JSON files"
                }),
            RowContent::Failed { dir, message } => {
                let dir = dir.clone();
                base.child(div().w(style::TREE_CHEVRON_WIDTH))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::error())
                            .child(message.clone()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("retry-{ix}")))
                            .flex_none()
                            .px_2()
                            .rounded_md()
                            .bg(theme::button())
                            .hover(|style| style.bg(theme::button_hover()))
                            .cursor_pointer()
                            .text_xs()
                            .child("Retry")
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.start_listing(dir.clone(), cx);
                                this.rebuild_rows();
                                cx.notify();
                            })),
                    )
            }
        }
    }
}

impl Render for ExplorerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let container = div()
            .size_full()
            .track_focus(&self.focus_handle)
            .key_context("Explorer")
            .on_action(cx.listener(Self::open_selected));
        if self.root.is_none() {
            return container
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_color(theme::text_muted())
                .child("Open a folder with Open Folder.");
        }
        container.child(
            uniform_list(
                "explorer-rows",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                    range.map(|ix| this.render_row(ix, cx)).collect()
                }),
            )
            .size_full(),
        )
    }
}

/// Folder name. Uses the full path when there is no name, as with a drive root.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_directories_first_then_parquet_files_by_name() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b_dir", "A_dir"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        for name in [
            "x.parquet",
            "Y.PARQUET",
            "notes.txt",
            "a.parquet",
            "parquet",
        ] {
            fs::write(dir.path().join(name), b"").unwrap();
        }

        let entries = list_directory(dir.path()).unwrap();
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["A_dir", "b_dir", "a.parquet", "x.parquet", "Y.PARQUET"]
        );
        assert_eq!(entries[0].kind, EntryKind::Directory);
        assert_eq!(entries[2].kind, EntryKind::File);
        assert!(entries.iter().all(|entry| !entry.is_link));
    }

    #[test]
    fn lists_sqlite_databases_by_header_and_their_tables() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE b (x); CREATE TABLE a (y); CREATE VIEW v AS SELECT x FROM b;",
        )
        .unwrap();
        drop(conn);
        // A `.db` file without the SQLite header is not shown.
        fs::write(dir.path().join("Thumbs.db"), b"thumbnails").unwrap();
        fs::write(dir.path().join("z.parquet"), b"").unwrap();

        let entries = list_directory(dir.path()).unwrap();
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["data.db", "z.parquet"]);
        assert_eq!(entries[0].kind, EntryKind::Database);

        let tables = list_database(&db).unwrap();
        let sources: Vec<_> = tables.iter().filter_map(DirEntry::source).collect();
        let table = |name: &str| DataSource::Sqlite {
            path: db.clone(),
            table: name.into(),
        };
        assert_eq!(sources, vec![table("a"), table("b"), table("v")]);
        assert_eq!(
            tables[2].kind,
            EntryKind::Table {
                table: "v".into(),
                view: true
            }
        );
    }

    #[test]
    fn lists_markdown_and_json_documents() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.md", "A.MARKDOWN", "c.json", "d.jsonl", "e.txt"] {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        let entries = list_directory(dir.path()).unwrap();
        let shown: Vec<_> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind.clone()))
            .collect();
        assert_eq!(
            shown,
            [
                ("A.MARKDOWN", EntryKind::Document(DocumentKind::Markdown)),
                ("b.md", EntryKind::Document(DocumentKind::Markdown)),
                ("c.json", EntryKind::Document(DocumentKind::Json)),
            ]
        );
        assert_eq!(entries[0].source(), None);
    }

    #[test]
    fn reports_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_directory(&dir.path().join("missing")).is_err());
    }
}
