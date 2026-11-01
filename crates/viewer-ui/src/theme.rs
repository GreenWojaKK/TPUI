//! Fonts, colors and fixed sizes shared across the app.
//!
//! Colors come in a dark and a light set. There is one mode per process, dark by default.
//! Color functions read the current mode on every paint, so redrawing the window after [`set_mode`] applies it at once.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Pixels, Rgba, WindowAppearance, px, rgb};

/// Color mode of the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

impl Mode {
    /// Mode matching the OS app color setting.
    pub fn from_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Mode::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Mode::Dark,
        }
    }

    /// The opposite mode.
    pub fn toggled(self) -> Self {
        match self {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Dark,
        }
    }
}

static LIGHT: AtomicBool = AtomicBool::new(false);

/// Current color mode.
pub fn mode() -> Mode {
    if LIGHT.load(Ordering::Relaxed) {
        Mode::Light
    } else {
        Mode::Dark
    }
}

/// Changes the color mode. Open windows pick up the new colors on their next paint, so the caller redraws them.
pub fn set_mode(mode: Mode) {
    LIGHT.store(mode == Mode::Light, Ordering::Relaxed);
}

/// Color for the current mode. Apps also use it to define their own colors in two sets.
pub fn pick(dark: u32, light: u32) -> Rgba {
    rgb(match mode() {
        Mode::Dark => dark,
        Mode::Light => light,
    })
}

/// App-wide font. The system UI font of Korean Windows (Malgun Gothic) draws `\` as `₩`, so
/// Segoe UI is set explicitly. Hangul falls back to the Windows fallback font.
pub const UI_FONT: &str = "Segoe UI";
/// Monospace font of the value detail body.
pub const MONO_FONT: &str = "Consolas";

pub const TOOLBAR_HEIGHT: Pixels = px(36.);
pub const STATUS_BAR_HEIGHT: Pixels = px(24.);
pub const PANEL_HEADER_HEIGHT: Pixels = px(28.);
pub const EXPLORER_WIDTH: Pixels = px(240.);
pub const SIDE_PANEL_WIDTH: Pixels = px(320.);
pub const LIST_ROW_HEIGHT: Pixels = px(24.);
pub const SCHEMA_TYPE_MAX_WIDTH: Pixels = px(140.);
pub const SCHEMA_NULLABLE_WIDTH: Pixels = px(52.);
pub const TAB_MAX_WIDTH: Pixels = px(220.);
pub const TAB_MIN_WIDTH: Pixels = px(120.);
pub const TAB_SCROLL_INDICATOR_HEIGHT: Pixels = px(3.);
pub const TAB_SCROLL_INDICATOR_MIN_WIDTH: Pixels = px(24.);
pub const TAB_CLOSE_SIZE: Pixels = px(18.);
/// Holding a dragged tab within this width of either end of the tab bar scrolls the bar that way.
pub const TAB_DRAG_EDGE: Pixels = px(40.);
/// Maximum scroll per frame while a dragged tab is held at an end of the bar.
pub const TAB_DRAG_SCROLL_STEP: Pixels = px(12.);
/// Height of a single-line input.
pub const INPUT_HEIGHT: Pixels = px(24.);
/// Width of the filter button at the right of a header.
pub const GRID_FILTER_BUTTON_WIDTH: Pixels = px(20.);
/// Width of the sort direction marker.
pub const GRID_SORT_MARK_WIDTH: Pixels = px(14.);
/// Width of the filter popover.
pub const FILTER_POPOVER_WIDTH: Pixels = px(300.);
pub const TREE_INDENT: Pixels = px(14.);
pub const TREE_ROW_PADDING: Pixels = px(8.);
pub const TREE_CHEVRON_WIDTH: Pixels = px(12.);
pub const GRID_ROW_HEIGHT: Pixels = px(24.);
pub const GRID_HEADER_HEIGHT: Pixels = px(28.);
pub const GRID_COLUMN_WIDTH: Pixels = px(160.);
pub const GRID_CELL_PADDING: Pixels = px(8.);
pub const GRID_MIN_ROW_NUMBER_WIDTH: Pixels = px(48.);
pub const GRID_SCROLLBAR_SIZE: Pixels = px(12.);
pub const GRID_SCROLLBAR_MIN_THUMB: Pixels = px(24.);
pub const VALUE_LINE_HEIGHT: Pixels = px(20.);

pub fn background() -> Rgba {
    pick(0x1e1f22, 0xf5f6f8)
}

pub fn surface() -> Rgba {
    pick(0x2b2d30, 0xffffff)
}

pub fn border() -> Rgba {
    pick(0x393b40, 0xd9dce1)
}

pub fn text() -> Rgba {
    pick(0xdfe1e5, 0x1f2328)
}

pub fn text_muted() -> Rgba {
    pick(0x8c8f94, 0x646a73)
}

pub fn error() -> Rgba {
    pick(0xf27c7c, 0xc62828)
}

pub fn selection() -> Rgba {
    pick(0x2e436e, 0xd3e1fb)
}

pub fn hover() -> Rgba {
    pick(0x2a2c30, 0xeceef1)
}

pub fn scrollbar_track() -> Rgba {
    pick(0x232427, 0xeef0f2)
}

pub fn scrollbar_thumb() -> Rgba {
    pick(0x4a4e55, 0xc3c7cd)
}

pub fn grid_line() -> Rgba {
    pick(0x2f3135, 0xe5e7ea)
}

/// Background of bar and strip chart tracks.
pub fn track() -> Rgba {
    pick(0x3a3d43, 0xe5e7ea)
}

pub fn null_text() -> Rgba {
    pick(0x7f8ea3, 0x6b7a90)
}

pub fn summary_text() -> Rgba {
    pick(0xb4a7d6, 0x6a55a3)
}

pub fn button() -> Rgba {
    pick(0x3c3f45, 0xe6e8eb)
}

pub fn button_hover() -> Rgba {
    pick(0x4a4e55, 0xd6d9de)
}

/// Drop position for the dragged tab.
pub fn drop_target() -> Rgba {
    pick(0x2e436e, 0xd3e1fb)
}

/// Border of a focused input.
pub fn focus_border() -> Rgba {
    pick(0x3574f0, 0x3574f0)
}

/// Background of cells matching the find input.
pub fn find_match() -> Rgba {
    pick(0x5c4a1e, 0xffe7a0)
}

/// Filter button of filtered columns and the sort direction marker.
pub fn accent() -> Rgba {
    pick(0x6ea2ff, 0x2f6fde)
}

/// Background of floating windows.
pub fn popover() -> Rgba {
    pick(0x313338, 0xffffff)
}
