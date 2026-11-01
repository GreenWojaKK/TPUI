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

/// Summaries in place of nested values, such as `{3 fields}`.
pub fn summary_text() -> Rgba {
    theme::pick(0xb4a7d6, 0x6a55a3)
}

/// Background of cells matching the find input.
pub fn find_match() -> Rgba {
    theme::pick(0x5c4a1e, 0xffe7a0)
}
