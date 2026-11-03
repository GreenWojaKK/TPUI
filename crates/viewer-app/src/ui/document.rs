//! Body of a Markdown or JSON tab. Holds the document of the active tab only: leaving a tab drops it, and returning reads
//! the file again. While a document is shown, a change to its file shows up by itself.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, ListOffset, SharedString, Subscription, Window, div,
    prelude::*,
};
use viewer_core::{Document, DocumentKind, FileStamp, ViewerError, file_stamp, read_document};
use viewer_ui::format::{format_bytes, format_quantity};
use viewer_ui::theme;

use super::json_tree::{JsonEvent, JsonPosition, JsonView};
use super::markdown::MarkdownView;
use crate::value_detail::ValueDetail;

/// Where a document tab was left, kept by the tab while another tab is shown.
#[derive(Clone, Debug, Default)]
pub enum DocumentPosition {
    #[default]
    Start,
    Markdown(ListOffset),
    Json(JsonPosition),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentStatus {
    Reading,
    Open,
    Failed,
}

pub enum DocumentEvent {
    /// The selected JSON value, for the Value panel.
    Value(Box<ValueDetail>),
    /// The file could not be read, so no value is selected.
    ValueGone,
}

/// Tells when a file has been rewritten. A new stamp counts once it has stayed the same for a second check, which skips
/// a file caught halfway through being written. `None` stands for a file that cannot be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChangeWatch {
    current: Option<FileStamp>,
    seen: Option<Option<FileStamp>>,
}

impl ChangeWatch {
    fn new(current: Option<FileStamp>) -> Self {
        Self {
            current,
            seen: None,
        }
    }

    /// Takes the stamp of a check. Returns whether the file should be read again.
    fn observe(&mut self, stamp: Option<FileStamp>) -> bool {
        if stamp == self.current {
            self.seen = None;
            return false;
        }
        if self.seen == Some(stamp) {
            self.current = stamp;
            self.seen = None;
            return true;
        }
        self.seen = Some(stamp);
        false
    }
}

enum Body {
    Empty,
    Reading,
    Failed(SharedString),
    Markdown(Entity<MarkdownView>),
    Json(Entity<JsonView>),
}

pub struct DocumentView {
    path: Option<PathBuf>,
    kind: DocumentKind,
    /// Incremented whenever another file is shown. Used to drop reads of the previous one.
    generation: u64,
    body: Body,
    /// Where to show the document once it is read.
    restore: DocumentPosition,
    watch: ChangeWatch,
    /// A check of the file is running.
    checking: bool,
    size: Option<u64>,
    _json_events: Option<Subscription>,
}

impl EventEmitter<DocumentEvent> for DocumentView {}

impl DocumentView {
    pub fn new() -> Self {
        Self {
            path: None,
            kind: DocumentKind::Markdown,
            generation: 0,
            body: Body::Empty,
            restore: DocumentPosition::Start,
            watch: ChangeWatch::default(),
            checking: false,
            size: None,
            _json_events: None,
        }
    }

    /// Reads a file and shows it from `position`.
    pub fn open(
        &mut self,
        path: PathBuf,
        kind: DocumentKind,
        position: DocumentPosition,
        cx: &mut Context<Self>,
    ) {
        self.generation += 1;
        self.path = Some(path);
        self.kind = kind;
        self.restore = position;
        self.body = Body::Reading;
        self.watch = ChangeWatch::default();
        self.checking = false;
        self.size = None;
        self.read(cx);
        cx.notify();
    }

    /// Drops the document.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.path = None;
        self.body = Body::Empty;
        self.watch = ChangeWatch::default();
        self.checking = false;
        self.size = None;
        cx.notify();
    }

    /// Reads the file in a background task. What is on screen stays until the result arrives.
    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (kind, generation) = (self.kind, self.generation);
        cx.spawn(async move |this, cx| {
            let (stamp, result) = cx
                .background_spawn(async move {
                    // Stamped before reading, so a write landing during the read shows as a change afterwards.
                    let stamp = file_stamp(&path).ok();
                    (stamp, read_document(&path, kind))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.finish_read(stamp, result, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn finish_read(
        &mut self,
        stamp: Option<FileStamp>,
        result: Result<Document, ViewerError>,
        cx: &mut Context<Self>,
    ) {
        self.watch = ChangeWatch::new(stamp);
        self.size = stamp.map(|stamp| stamp.len);
        match result {
            Ok(Document::Markdown(doc)) => {
                let doc = Arc::new(doc);
                if let Body::Markdown(view) = &self.body {
                    view.update(cx, |view, cx| view.set_document(doc, cx));
                } else {
                    let top = match &self.restore {
                        DocumentPosition::Markdown(top) => Some(*top),
                        _ => None,
                    };
                    let path = self.path.clone().unwrap_or_default();
                    self.body = Body::Markdown(cx.new(|cx| MarkdownView::new(doc, &path, top, cx)));
                }
            }
            Ok(Document::Json(doc)) => {
                let doc = Arc::new(doc);
                if let Body::Json(view) = &self.body {
                    view.update(cx, |view, cx| view.set_document(doc, cx));
                } else {
                    let position = match &self.restore {
                        DocumentPosition::Json(position) => position.clone(),
                        _ => JsonPosition::default(),
                    };
                    let view = cx.new(|cx| JsonView::new(doc, position, cx));
                    self._json_events = Some(cx.subscribe(&view, |_, _, event: &JsonEvent, cx| {
                        let JsonEvent::Selected(detail) = event;
                        cx.emit(DocumentEvent::Value(Box::new(detail.clone())));
                    }));
                    view.update(cx, |view, cx| view.emit_selected(cx));
                    self.body = Body::Json(view);
                }
            }
            Err(error) => {
                self.restore = self.position(cx);
                self.body = Body::Failed(error_message(&error).into());
                cx.emit(DocumentEvent::ValueGone);
            }
        }
        cx.notify();
    }

    /// Checks the file in a background task, and reads it again once it has changed.
    pub fn check_for_changes(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.checking || matches!(self.body, Body::Reading) {
            return;
        }
        self.checking = true;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let stamp = cx
                .background_spawn(async move { file_stamp(&path).ok() })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.checking = false;
                if this.watch.observe(stamp) {
                    this.read(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Where the document stands now, for the tab to remember.
    pub fn position(&self, cx: &gpui::App) -> DocumentPosition {
        match &self.body {
            Body::Markdown(view) => DocumentPosition::Markdown(view.read(cx).scroll_top()),
            Body::Json(view) => DocumentPosition::Json(view.read(cx).position()),
            Body::Empty | Body::Reading | Body::Failed(_) => self.restore.clone(),
        }
    }

    pub fn status(&self) -> DocumentStatus {
        match self.body {
            Body::Empty | Body::Reading => DocumentStatus::Reading,
            Body::Failed(_) => DocumentStatus::Failed,
            Body::Markdown(_) | Body::Json(_) => DocumentStatus::Open,
        }
    }

    /// Facts about the open document for the status bar.
    pub fn summary(&self, cx: &gpui::App) -> Vec<String> {
        let mut summary = vec![self.kind.name().to_string()];
        match &self.body {
            Body::Markdown(view) => summary.push(format_quantity(
                view.read(cx).block_count() as u64,
                "block",
                "blocks",
            )),
            Body::Json(view) => summary.push(format_quantity(
                view.read(cx).value_count() as u64,
                "value",
                "values",
            )),
            _ => {}
        }
        if let Some(size) = self.size {
            summary.push(format_bytes(size));
        }
        summary
    }

    /// Copies the selected JSON value.
    pub fn copy_selected(&self, cx: &mut Context<Self>) {
        if let Body::Json(view) = &self.body {
            view.update(cx, |view, cx| view.copy_selected(cx));
        }
    }
}

fn error_message(error: &ViewerError) -> String {
    match error {
        ViewerError::Io(error) => match error.kind() {
            io::ErrorKind::NotFound => "File not found.".into(),
            io::ErrorKind::PermissionDenied => "No permission to read the file.".into(),
            _ => format!("Cannot read the file. ({error})"),
        },
        ViewerError::InvalidText { offset } => {
            format!("This file is not UTF-8 text (byte {offset}).")
        }
        ViewerError::InvalidJson {
            line,
            column,
            reason,
        } => format!("Not valid JSON: line {line}, column {column}: {reason}."),
        other => other.to_string(),
    }
}

impl Render for DocumentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let message = || {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
        };
        match &self.body {
            Body::Empty | Body::Reading => message()
                .text_color(theme::text_muted())
                .child("Reading…")
                .into_any_element(),
            Body::Failed(error) => message()
                .child(div().text_color(theme::error()).child(error.clone()))
                .children(self.path.as_ref().map(|path| {
                    div()
                        .text_color(theme::text_muted())
                        .child(path.display().to_string())
                }))
                .child(
                    div()
                        .id("document-retry")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme::button())
                        .hover(|style| style.bg(theme::button_hover()))
                        .cursor_pointer()
                        .child("Retry")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.read(cx))),
                )
                .into_any_element(),
            Body::Markdown(view) => view.clone().into_any_element(),
            Body::Json(view) => view.clone().into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(len: u64) -> Option<FileStamp> {
        Some(FileStamp {
            len,
            modified: None,
        })
    }

    #[test]
    fn counts_a_change_once_it_has_settled() {
        let mut watch = ChangeWatch::new(stamp(1));
        assert!(!watch.observe(stamp(1)));
        // A file still being written changes between checks.
        assert!(!watch.observe(stamp(2)));
        assert!(!watch.observe(stamp(3)));
        assert!(watch.observe(stamp(3)));
        assert!(!watch.observe(stamp(3)));
    }

    #[test]
    fn a_write_that_is_undone_is_not_a_change() {
        let mut watch = ChangeWatch::new(stamp(1));
        assert!(!watch.observe(stamp(2)));
        assert!(!watch.observe(stamp(1)));
        assert!(!watch.observe(stamp(2)));
    }

    #[test]
    fn notices_a_file_going_away_and_coming_back() {
        let mut watch = ChangeWatch::new(stamp(1));
        // Saving by replacing the file leaves it missing for a moment.
        assert!(!watch.observe(None));
        assert!(!watch.observe(stamp(1)));
        assert!(!watch.observe(None));
        assert!(watch.observe(None));
        assert!(!watch.observe(stamp(2)));
        assert!(watch.observe(stamp(2)));
    }

    #[test]
    fn explains_read_failures() {
        let missing = ViewerError::Io(io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(error_message(&missing), "File not found.");
        assert_eq!(
            error_message(&ViewerError::InvalidText { offset: 7 }),
            "This file is not UTF-8 text (byte 7)."
        );
        assert_eq!(
            error_message(&ViewerError::InvalidJson {
                line: 2,
                column: 8,
                reason: "expected a value"
            }),
            "Not valid JSON: line 2, column 8: expected a value."
        );
    }
}
