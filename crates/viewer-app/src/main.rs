#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cell_preview;
mod query_log;
mod value_detail;
mod ui {
    pub mod explorer;
    pub mod find_bar;
    pub mod grid;
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
use crate::ui::value::CopyValue;

fn main() {
    application().run(|cx: &mut App| {
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
                    title: Some("Parquet Viewer".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AppController::new(window, cx)),
        )
        .expect("cannot open the main window");
        cx.activate(true);
    });
}
