//! Body of a document tab. Holds the document of the active tab only: leaving a tab drops it, and returning reads the file
//! again. While a document is shown, a change to its file shows up by itself.
//!
//! A code tab shows its text as soon as it is read. Its colors come from the code core, which runs only while a code tab
//! is shown. Whatever happens to the core, the text stays on screen.

use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, ListOffset, SharedString, Subscription, Task,
    Window, div, prelude::*,
};
use viewer_core::{Document, DocumentKind, FileStamp, ViewerError, file_stamp, read_document};
use viewer_ui::format::{format_bytes, format_quantity};
use viewer_ui::theme;

use super::code::{CodeEvent, CodeScroll, CodeView};
use super::json_tree::{JsonEvent, JsonPosition, JsonView};
use super::markdown::MarkdownView;
use super::pdf::{PdfScroll, PdfView, PdfViewEvent};
use crate::code_core::{CodeCore, CoreEvent, TokenRequest, core_path};
use crate::pdf_worker::{PdfWorker, WorkerEvent, worker_path};
use crate::usage_log::ProcessUsage;
use crate::value_detail::ValueDetail;

/// How many times the core may start for one opened file. A file that keeps ending the core stays plain text.
const MAX_CORE_STARTS: u32 = 3;

/// Where a document tab was left, kept by the tab while another tab is shown.
#[derive(Clone, Debug, Default)]
pub enum DocumentPosition {
    #[default]
    Start,
    Markdown(ListOffset),
    Json(JsonPosition),
    Code(CodeScroll),
    Pdf(PdfScroll),
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
    /// A read finished. `again` tells a read after a change from the first read in this tab.
    Read { again: bool },
    /// The first tokens of the read document arrived.
    TokensReady,
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
    Code(Entity<CodeView>),
    Pdf(Entity<PdfView>),
}

/// A running core with the task that receives its answers.
struct CoreLink {
    core: CodeCore,
    _answers: Task<()>,
}

/// A running PDF worker with the task that receives its answers.
struct PdfLink {
    worker: PdfWorker,
    _answers: Task<()>,
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
    /// The version of the file on screen.
    shown: Option<FileStamp>,
    _body_events: Option<Subscription>,
    core: Option<CoreLink>,
    /// Times the core started for the opened file.
    core_starts: u32,
    /// Why the core last ended.
    core_stop: Option<String>,
    /// The latest token request. Only its answer counts.
    request_id: u64,
    /// The core found the file differs from the version on screen.
    tokens_stale: bool,
    /// Why a code file shows without colors, for the status bar.
    plain_reason: Option<String>,
    /// Tokens have arrived for the document as last read.
    tokens_arrived: bool,
    /// The PDF worker, while a PDF tab is shown.
    pdf: Option<PdfLink>,
    /// The worker's name for the latest reading of the PDF file.
    pdf_doc: u64,
    /// A reading of the PDF file waiting for its page sizes, with the stamp taken before asking.
    pdf_pending: Option<(u64, Option<FileStamp>)>,
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
            shown: None,
            _body_events: None,
            core: None,
            core_starts: 0,
            core_stop: None,
            request_id: 0,
            tokens_stale: false,
            plain_reason: None,
            tokens_arrived: false,
            pdf: None,
            pdf_doc: 0,
            pdf_pending: None,
        }
    }

    /// Reads a file and shows it from `position`. The code core keeps running between code tabs, and the PDF worker
    /// between PDF tabs.
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
        self.shown = None;
        self.core_starts = 0;
        self.core_stop = None;
        self.tokens_stale = false;
        self.plain_reason = None;
        if kind != DocumentKind::Code {
            self.core = None;
        }
        if kind != DocumentKind::Pdf {
            self.pdf = None;
        }
        self.pdf_pending = None;
        self.read(cx);
        cx.notify();
    }

    /// Drops the document and ends the code core and the PDF worker.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.path = None;
        self.body = Body::Empty;
        self.watch = ChangeWatch::default();
        self.checking = false;
        self.shown = None;
        self.core = None;
        self.pdf = None;
        self.pdf_pending = None;
        cx.notify();
    }

    /// Reads the file in a background task. What is on screen stays until the result arrives.
    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (kind, generation) = (self.kind, self.generation);
        if kind == DocumentKind::Pdf {
            return self.read_pdf(path, generation, cx);
        }
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

    /// Asks the PDF worker for the page sizes, starting it if needed. The answer finishes the read.
    fn read_pdf(&mut self, path: PathBuf, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let stamped = path.clone();
            // Stamped before asking, so a write landing during the read shows as a change afterwards.
            let stamp = cx
                .background_spawn(async move { file_stamp(&stamped).ok() })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if this.pdf.is_none()
                    && let Err(message) = this.start_pdf(cx)
                {
                    return this.finish_read(stamp, Err(ViewerError::Pdf(message)), cx);
                }
                this.pdf_doc += 1;
                this.pdf_pending = Some((this.pdf_doc, stamp));
                if let Some(link) = &this.pdf {
                    link.worker.open(this.pdf_doc, path);
                }
            })
            .ok();
        })
        .detach();
    }

    fn start_pdf(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let (worker, answers) = worker_path()
            .and_then(|path| PdfWorker::start(&path))
            .map_err(|_| "the PDF worker cannot start".to_string())?;
        let answers = cx.spawn(async move |this, cx| {
            while let Ok(event) = answers.recv().await {
                if this
                    .update(cx, |this, cx| this.pdf_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.pdf = Some(PdfLink {
            worker,
            _answers: answers,
        });
        Ok(())
    }

    fn pdf_event(&mut self, event: WorkerEvent, cx: &mut Context<Self>) {
        let view = match &self.body {
            Body::Pdf(view) => Some(view.clone()),
            _ => None,
        };
        match event {
            WorkerEvent::Info { doc, info } => {
                if let Some((_, stamp)) = self.pdf_pending.take_if(|(pending, _)| *pending == doc) {
                    self.finish_read(stamp, Ok(Document::Pdf(info)), cx);
                }
            }
            WorkerEvent::Failed {
                doc,
                page: None,
                message,
            } => {
                if let Some((_, stamp)) = self.pdf_pending.take_if(|(pending, _)| *pending == doc) {
                    self.finish_read(stamp, Err(ViewerError::Pdf(message)), cx);
                } else if doc == self.pdf_doc
                    && let Some(view) = view
                {
                    view.update(cx, |view, cx| view.page_failed(message, cx));
                }
            }
            WorkerEvent::Failed {
                doc,
                page: Some(page),
                message,
            } => {
                if doc == self.pdf_doc
                    && let Some(view) = view
                {
                    let message = format!("page {}: {message}", page + 1);
                    view.update(cx, |view, cx| view.page_failed(message, cx));
                }
            }
            WorkerEvent::Page {
                doc,
                page,
                width,
                image,
            } => {
                if doc == self.pdf_doc
                    && self.pdf_pending.is_none()
                    && let Some(view) = view
                {
                    view.update(cx, |view, cx| view.take_page(page, width, image, cx));
                }
            }
            WorkerEvent::Stopped => {
                self.pdf = None;
                let ended = ViewerError::Pdf("the PDF worker ended".into());
                if let Some((_, stamp)) = self.pdf_pending.take() {
                    self.finish_read(stamp, Err(ended), cx);
                } else if view.is_some() {
                    // Retry reads the file again, which starts a new worker.
                    self.restore = self.position(cx);
                    self.body = Body::Failed(error_message(&ended).into());
                    cx.notify();
                }
            }
        }
    }

    fn finish_read(
        &mut self,
        stamp: Option<FileStamp>,
        result: Result<Document, ViewerError>,
        cx: &mut Context<Self>,
    ) {
        let again = !matches!(self.body, Body::Reading);
        self.watch = ChangeWatch::new(stamp);
        self.shown = stamp;
        self.tokens_stale = false;
        self.tokens_arrived = false;
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
                    self._body_events = Some(cx.subscribe(&view, |_, _, event: &JsonEvent, cx| {
                        let JsonEvent::Selected(detail) = event;
                        cx.emit(DocumentEvent::Value(Box::new(detail.clone())));
                    }));
                    view.update(cx, |view, cx| view.emit_selected(cx));
                    self.body = Body::Json(view);
                }
            }
            Ok(Document::Code(doc)) => {
                let doc = Arc::new(doc);
                if let Body::Code(view) = &self.body {
                    view.update(cx, |view, cx| view.set_document(doc, cx));
                } else {
                    let scroll = match &self.restore {
                        DocumentPosition::Code(scroll) => *scroll,
                        _ => CodeScroll::default(),
                    };
                    let view = cx.new(|cx| CodeView::new(doc, scroll, cx));
                    self._body_events =
                        Some(cx.subscribe(&view, |this, _, event: &CodeEvent, cx| {
                            let CodeEvent::NeedTokens(range) = event;
                            this.request_tokens(range.clone(), cx);
                        }));
                    self.body = Body::Code(view);
                }
            }
            Ok(Document::Pdf(info)) => {
                let info = Arc::new(info);
                if let Body::Pdf(view) = &self.body {
                    view.update(cx, |view, cx| view.set_document(info, cx));
                } else {
                    let position = match &self.restore {
                        DocumentPosition::Pdf(position) => *position,
                        _ => PdfScroll::default(),
                    };
                    let view = cx.new(|cx| PdfView::new(info, position, cx));
                    self._body_events =
                        Some(cx.subscribe(&view, |this, _, event: &PdfViewEvent, _| {
                            let PdfViewEvent::Draw { width, pages } = event;
                            if let Some(link) = &this.pdf {
                                link.worker.draw(this.pdf_doc, *width, pages.clone());
                            }
                        }));
                    self.body = Body::Pdf(view);
                }
            }
            Err(error) => {
                self.restore = self.position(cx);
                self.body = Body::Failed(error_message(&error).into());
                cx.emit(DocumentEvent::ValueGone);
            }
        }
        cx.emit(DocumentEvent::Read { again });
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
                } else if this.tokens_stale && stamp == this.shown {
                    // The file is back to the version on screen, so no read will come to ask again.
                    this.tokens_stale = false;
                    if let Body::Code(view) = &this.body {
                        view.update(cx, |view, cx| view.retry_tokens(cx));
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Passes a code view's request to the core, starting the core if needed.
    fn request_tokens(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let (Some(path), Some(stamp)) = (self.path.clone(), self.shown) else {
            return self.show_plain("the file cannot be read".to_string(), cx);
        };
        if self.core.is_none()
            && let Err(reason) = self.start_core(cx)
        {
            return self.show_plain(reason, cx);
        }
        self.request_id += 1;
        if let Some(link) = &self.core {
            link.core.request(TokenRequest {
                id: self.request_id,
                path,
                stamp,
                start: range.start,
                end: range.end,
            });
        }
    }

    fn start_core(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if self.core_starts >= MAX_CORE_STARTS {
            let reason = self.core_stop.clone();
            return Err(reason.unwrap_or_else(|| "the code core keeps ending".to_string()));
        }
        self.core_starts += 1;
        let (core, answers) = core_path()
            .and_then(|path| CodeCore::start(&path))
            .map_err(|_| "the code core cannot start".to_string())?;
        let answers = cx.spawn(async move |this, cx| {
            while let Ok(event) = answers.recv().await {
                if this
                    .update(cx, |this, cx| this.core_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.core = Some(CoreLink {
            core,
            _answers: answers,
        });
        Ok(())
    }

    /// Stops asking the core about this version of the file, and says why in the status bar.
    fn show_plain(&mut self, reason: String, cx: &mut Context<Self>) {
        self.plain_reason = Some(reason);
        if let Body::Code(view) = &self.body {
            view.update(cx, |view, cx| view.tokens_unavailable(cx));
        }
        cx.notify();
    }

    fn core_event(&mut self, event: CoreEvent, cx: &mut Context<Self>) {
        match event {
            CoreEvent::Tokens {
                id,
                language,
                tokens,
            } if id == self.request_id => {
                self.plain_reason = None;
                if let Body::Code(view) = &self.body {
                    view.update(cx, |view, cx| view.set_tokens(&tokens, language, cx));
                }
                if !self.tokens_arrived {
                    self.tokens_arrived = true;
                    cx.emit(DocumentEvent::TokensReady);
                }
                cx.notify();
            }
            CoreEvent::Stale { id } if id == self.request_id => {
                // A read of the new version is coming, and until then the text on screen has no tokens.
                self.tokens_stale = true;
                if let Body::Code(view) = &self.body {
                    view.update(cx, |view, cx| view.tokens_unavailable(cx));
                }
            }
            CoreEvent::Failed { id, message } if id == self.request_id => {
                self.show_plain(message, cx);
            }
            CoreEvent::Stopped { reason } => {
                self.core = None;
                self.core_stop = Some(reason);
                // The view asks again when it needs tokens, which starts the core while starts are left.
                if let Body::Code(view) = &self.body {
                    view.update(cx, |view, cx| view.retry_tokens(cx));
                }
                cx.notify();
            }
            // The answer to a request that has been replaced.
            CoreEvent::Tokens { .. } | CoreEvent::Stale { .. } | CoreEvent::Failed { .. } => {}
        }
    }

    /// What the PDF worker uses now, when it runs.
    pub fn pdf_usage(&self) -> Option<ProcessUsage> {
        self.pdf.as_ref().and_then(|link| link.worker.usage())
    }

    /// What the code core uses now, when it runs.
    pub fn core_usage(&self) -> Option<ProcessUsage> {
        self.core.as_ref().and_then(|link| link.core.usage())
    }

    /// The size of the file as last read.
    pub fn file_size(&self) -> Option<u64> {
        self.shown.map(|stamp| stamp.len)
    }

    /// Where the document stands now, for the tab to remember.
    pub fn position(&self, cx: &gpui::App) -> DocumentPosition {
        match &self.body {
            Body::Markdown(view) => DocumentPosition::Markdown(view.read(cx).scroll_top()),
            Body::Json(view) => DocumentPosition::Json(view.read(cx).position()),
            Body::Code(view) => DocumentPosition::Code(view.read(cx).scroll()),
            Body::Pdf(view) => DocumentPosition::Pdf(view.read(cx).position()),
            Body::Empty | Body::Reading | Body::Failed(_) => self.restore.clone(),
        }
    }

    pub fn status(&self) -> DocumentStatus {
        match self.body {
            Body::Empty | Body::Reading => DocumentStatus::Reading,
            Body::Failed(_) => DocumentStatus::Failed,
            Body::Markdown(_) | Body::Json(_) | Body::Code(_) | Body::Pdf(_) => {
                DocumentStatus::Open
            }
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
            Body::Json(view) => summary.extend(view.read(cx).summary()),
            Body::Pdf(view) => summary.extend(super::pdf::summary(view.read(cx))),
            Body::Code(view) => {
                let view = view.read(cx);
                if let Some(language) = view.language() {
                    summary[0] = language.to_string();
                }
                summary.push(format_quantity(view.line_count() as u64, "line", "lines"));
            }
            _ => {}
        }
        if let Some(stamp) = self.shown {
            summary.push(format_bytes(stamp.len));
        }
        summary
    }

    /// Why a code file shows without colors, for the status bar.
    pub fn plain_note(&self) -> Option<String> {
        let reason = self.plain_reason.as_ref()?;
        Some(format!("Plain text: {reason}"))
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
        ViewerError::Pdf(detail) => format!("Windows cannot show this PDF ({detail})."),
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
            Body::Code(view) => view.clone().into_any_element(),
            Body::Pdf(view) => view.clone().into_any_element(),
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
