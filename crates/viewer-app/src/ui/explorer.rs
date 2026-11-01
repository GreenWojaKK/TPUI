//! Folder explorer. Lists only the direct children of expanded folders and draws only the visible rows.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gpui::{
    ClickEvent, Context, Div, EventEmitter, FocusHandle, SharedString, Stateful, Window, actions,
    div, prelude::*, uniform_list,
};

use viewer_ui::theme;

actions!(explorer, [OpenSelected]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Directory,
    File,
}

/// Directory entry shown in the explorer. Files are limited to the `.parquet` extension.
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
        if kind == EntryKind::File && !is_parquet(&path) {
            continue;
        }
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path,
            kind,
            is_link,
        });
    }
    entries.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| compare_names(&a.name, &b.name))
    });
    Ok(entries)
}

fn is_parquet(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("parquet"))
}

/// Case-insensitive name order, falling back to the original strings on ties.
fn compare_names(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

pub enum ExplorerEvent {
    OpenFile(PathBuf),
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
    Empty,
}

pub struct ExplorerView {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    /// Incremented whenever the root changes. Used to drop listings for the previous root.
    generation: u64,
    listings: HashMap<PathBuf, Listing>,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
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

    /// Called when a file was opened by another path. Without a root, sets the file's parent folder as the root.
    pub fn file_activated(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.root.is_none()
            && let Some(parent) = path.parent()
        {
            self.set_root(parent.to_path_buf(), cx);
        }
        self.selected = Some(path.to_path_buf());
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

    /// Lists the direct children of a folder in a background task. Reuses an in-flight listing of the same folder.
    fn start_listing(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if matches!(self.listings.get(&dir), Some(Listing::Loading)) {
            return;
        }
        self.listings.insert(dir.clone(), Listing::Loading);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let target = dir.clone();
            let result = cx
                .background_spawn(async move { list_directory(&target) })
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
        result: io::Result<Vec<DirEntry>>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        let listing = match result {
            Ok(entries) => Listing::Loaded(entries),
            Err(err) => Listing::Failed(format!("Cannot read the folder. ({err})").into()),
        };
        self.listings.insert(dir, listing);
        self.rebuild_rows();
        cx.notify();
    }

    fn activate(&mut self, entry: &DirEntry, cx: &mut Context<Self>) {
        match entry.kind {
            EntryKind::File => cx.emit(ExplorerEvent::OpenFile(entry.path.clone())),
            EntryKind::Directory if !entry.is_link => self.toggle(entry.path.clone(), cx),
            EntryKind::Directory => {}
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.rows.iter().find_map(|row| match &row.content {
            RowContent::Entry { entry, .. } if Some(&entry.path) == self.selected.as_ref() => {
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
        let expanded = entry.kind == EntryKind::Directory
            && !entry.is_link
            && self.expanded.contains(&entry.path);
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
                content: RowContent::Empty,
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
            .h(theme::LIST_ROW_HEIGHT)
            .pl(theme::TREE_INDENT * row.depth as f32 + theme::TREE_ROW_PADDING)
            .pr(theme::TREE_ROW_PADDING)
            .flex()
            .items_center()
            .gap_1();
        match &row.content {
            RowContent::Entry { entry, expanded } => {
                let selected = self.selected.as_ref() == Some(&entry.path);
                let chevron = match (entry.kind, entry.is_link, expanded) {
                    (EntryKind::Directory, false, true) => "▾",
                    (EntryKind::Directory, false, false) => "▸",
                    _ => "",
                };
                let entry = entry.clone();
                base.cursor_pointer()
                    .when(selected, |row| row.bg(theme::selection()))
                    .when(!selected, |row| row.hover(|style| style.bg(theme::hover())))
                    .child(
                        div()
                            .flex_none()
                            .w(theme::TREE_CHEVRON_WIDTH)
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
                    .when(entry.is_link, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("link"),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.selected = Some(entry.path.clone());
                        // Folders expand on a single click; files open on a double click.
                        let activate = match entry.kind {
                            EntryKind::Directory => true,
                            EntryKind::File => event.click_count() >= 2,
                        };
                        if activate {
                            this.activate(&entry, cx);
                        }
                        cx.notify();
                    }))
            }
            RowContent::Loading => base
                .text_color(theme::text_muted())
                .child(div().w(theme::TREE_CHEVRON_WIDTH))
                .child("Reading…"),
            RowContent::Empty => base
                .text_color(theme::text_muted())
                .child(div().w(theme::TREE_CHEVRON_WIDTH))
                .child("No Parquet files"),
            RowContent::Failed { dir, message } => {
                let dir = dir.clone();
                base.child(div().w(theme::TREE_CHEVRON_WIDTH))
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
    fn reports_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_directory(&dir.path().join("missing")).is_err());
    }
}
