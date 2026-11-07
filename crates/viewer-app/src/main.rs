#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cell_preview;
mod code_core;
mod open_requests;
mod pdf_worker;
mod query_log;
mod usage_log;
mod value_detail;
mod ui {
    pub mod code;
    pub mod doc_scrollbar;
    pub mod document;
    pub mod explorer;
    pub mod find_bar;
    pub mod grid;
    pub mod json_tree;
    pub mod markdown;
    pub mod pdf;
    pub mod schema;
    pub mod structure;
    pub mod style;
    pub mod value;
    pub mod view_bar;
}
mod worker;

use gpui::{
    App, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size,
};
use gpui_platform::application;

use crate::app::{
    AppController, CloseFind, CloseTab, Find, FindNext, FindPrevious, NextTab, OpenFile,
    PreviousTab,
};
use crate::ui::explorer::OpenSelected;
use crate::ui::grid::{
    MoveDown, MoveLeft, MoveRight, MoveToFirstRow, MoveToLastRow, MoveUp, PageDown, PageUp,
};
use crate::ui::markdown::SelectAll;
use crate::ui::value::CopyValue;

fn main() {
    let open_requests = match open_requests::begin() {
        open_requests::Startup::Run(requests) => requests,
        open_requests::Startup::HandedOver => return,
    };
    application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-o", OpenFile, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
            KeyBinding::new("ctrl-w", CloseTab, None),
            KeyBinding::new("ctrl-f", Find, None),
            KeyBinding::new("f3", FindNext, None),
            KeyBinding::new("shift-f3", FindPrevious, None),
            KeyBinding::new("escape", CloseFind, Some("Grid")),
            KeyBinding::new("enter", OpenSelected, Some("Explorer")),
            KeyBinding::new("up", MoveUp, Some("Grid")),
            KeyBinding::new("down", MoveDown, Some("Grid")),
            KeyBinding::new("left", MoveLeft, Some("Grid")),
            KeyBinding::new("right", MoveRight, Some("Grid")),
            KeyBinding::new("pageup", PageUp, Some("Grid")),
            KeyBinding::new("pagedown", PageDown, Some("Grid")),
            KeyBinding::new("ctrl-home", MoveToFirstRow, Some("Grid")),
            KeyBinding::new("ctrl-end", MoveToLastRow, Some("Grid")),
            KeyBinding::new("ctrl-c", CopyValue, Some("Grid")),
            KeyBinding::new("ctrl-c", CopyValue, Some("ValuePanel")),
            KeyBinding::new("ctrl-c", CopyValue, Some("Document")),
            KeyBinding::new("ctrl-a", SelectAll, Some("Document")),
            KeyBinding::new("enter", OpenSelected, Some("Document")),
            KeyBinding::new("up", MoveUp, Some("Document")),
            KeyBinding::new("down", MoveDown, Some("Document")),
            KeyBinding::new("left", MoveLeft, Some("Document")),
            KeyBinding::new("right", MoveRight, Some("Document")),
            KeyBinding::new("pageup", PageUp, Some("Document")),
            KeyBinding::new("pagedown", PageDown, Some("Document")),
            KeyBinding::new("ctrl-home", MoveToFirstRow, Some("Document")),
            KeyBinding::new("ctrl-end", MoveToLastRow, Some("Document")),
        ]);

        // Single-window app: quit when the window closes.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("TPUI".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AppController::new(open_requests, window, cx)),
        )
        .expect("cannot open the main window");
        cx.activate(true);
    });
}
