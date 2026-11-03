//! Sizes and colors of the viewer's own views. Shared roles and sizes come from `viewer_ui::theme`.

use gpui::{Pixels, Rgba, px};
use viewer_ui::theme;

pub const TOOLBAR_HEIGHT: Pixels = px(36.);
pub const STATUS_BAR_HEIGHT: Pixels = px(24.);
pub const EXPLORER_WIDTH: Pixels = px(240.);
pub const SIDE_PANEL_WIDTH: Pixels = px(320.);
pub const SCHEMA_TYPE_MAX_WIDTH: Pixels = px(140.);
pub const SCHEMA_NULLABLE_WIDTH: Pixels = px(52.);
/// Width of the sort direction marker.
pub const GRID_SORT_MARK_WIDTH: Pixels = px(14.);
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
/// Widest line of a Markdown document. Wider windows center the column.
pub const DOC_READING_WIDTH: Pixels = px(860.);
pub const DOC_PADDING_X: Pixels = px(32.);
pub const DOC_PADDING_Y: Pixels = px(24.);
pub const DOC_TEXT_SIZE: Pixels = px(15.);
pub const DOC_LINE_HEIGHT: f32 = 1.65;
pub const DOC_HEADING_LINE_HEIGHT: f32 = 1.3;
pub const DOC_CODE_SIZE: Pixels = px(13.);
pub const DOC_CODE_LINE_HEIGHT: f32 = 1.5;
pub const DOC_BLOCK_GAP: Pixels = px(12.);
pub const DOC_LIST_GAP: Pixels = px(3.);
pub const DOC_LIST_INDENT: Pixels = px(26.);
pub const DOC_QUOTE_INDENT: Pixels = px(16.);
pub const DOC_QUOTE_BAR: Pixels = px(3.);
pub const DOC_TASK_BOX: Pixels = px(14.);
/// Space above a task box, centering it on the first line of text.
pub const DOC_TASK_BOX_TOP: Pixels = px(5.);
pub const DOC_TASK_MARK_SIZE: Pixels = px(11.);
/// Assumed height of a block not yet drawn, for sizing the scrollbar thumb.
pub const DOC_BLOCK_HEIGHT_HINT: Pixels = px(44.);
/// Distance one press of an arrow key scrolls.
pub const DOC_SCROLL_STEP: Pixels = px(48.);

/// Text size and space above a Markdown heading of a level.
pub fn doc_heading(level: u8) -> (Pixels, Pixels) {
    match level {
        1 => (px(28.), px(16.)),
        2 => (px(22.), px(14.)),
        3 => (px(18.), px(10.)),
        4 => (px(16.), px(8.)),
        _ => (DOC_TEXT_SIZE, px(6.)),
    }
}

/// Summaries in place of nested values, such as `{3 fields}`.
pub fn summary_text() -> Rgba {
    theme::pick(0xb4a7d6, 0x6a55a3)
}

/// Background of cells matching the find input.
pub fn find_match() -> Rgba {
    theme::pick(0x5c4a1e, 0xffe7a0)
}
