//! 앱 전체가 공유하는 글꼴, 색상과 고정 크기.

use gpui::{Pixels, Rgba, px, rgb};

/// 앱 전체의 글꼴. 한국어 Windows의 시스템 UI 글꼴(맑은 고딕)은 `\`를 `₩` 모양으로 그리므로
/// Segoe UI를 지정한다. 한글은 Windows의 대체 글꼴로 표시된다.
pub const UI_FONT: &str = "Segoe UI";
/// 값 상세 본문의 고정폭 글꼴.
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
/// 탭을 끌어 탭 막대 양 끝의 이 폭 안에 두면 막대가 그쪽으로 스크롤된다.
pub const TAB_DRAG_EDGE: Pixels = px(40.);
/// 탭을 끌어 막대 끝에 붙였을 때 한 frame에 스크롤하는 최대 거리.
pub const TAB_DRAG_SCROLL_STEP: Pixels = px(12.);
/// 한 줄 입력의 높이.
pub const INPUT_HEIGHT: Pixels = px(24.);
/// 헤더 오른쪽 필터 버튼의 너비.
pub const GRID_FILTER_BUTTON_WIDTH: Pixels = px(20.);
/// 정렬 방향 표시의 너비.
pub const GRID_SORT_MARK_WIDTH: Pixels = px(14.);
/// 필터 창의 너비.
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
    rgb(0x1e1f22)
}

pub fn surface() -> Rgba {
    rgb(0x2b2d30)
}

pub fn border() -> Rgba {
    rgb(0x393b40)
}

pub fn text() -> Rgba {
    rgb(0xdfe1e5)
}

pub fn text_muted() -> Rgba {
    rgb(0x8c8f94)
}

pub fn error() -> Rgba {
    rgb(0xf27c7c)
}

pub fn selection() -> Rgba {
    rgb(0x2e436e)
}

pub fn hover() -> Rgba {
    rgb(0x2a2c30)
}

pub fn scrollbar_track() -> Rgba {
    rgb(0x232427)
}

pub fn scrollbar_thumb() -> Rgba {
    rgb(0x4a4e55)
}

pub fn grid_line() -> Rgba {
    rgb(0x2f3135)
}

pub fn null_text() -> Rgba {
    rgb(0x7f8ea3)
}

pub fn summary_text() -> Rgba {
    rgb(0xb4a7d6)
}

pub fn button() -> Rgba {
    rgb(0x3c3f45)
}

pub fn button_hover() -> Rgba {
    rgb(0x4a4e55)
}

/// 끌고 있는 탭을 놓을 자리.
pub fn drop_target() -> Rgba {
    rgb(0x2e436e)
}

/// 포커스가 있는 입력의 테두리.
pub fn focus_border() -> Rgba {
    rgb(0x3574f0)
}

/// 찾기 입력과 일치하는 셀의 배경.
pub fn find_match() -> Rgba {
    rgb(0x5c4a1e)
}

/// 필터가 있는 컬럼의 필터 버튼, 정렬 방향 표시.
pub fn accent() -> Rgba {
    rgb(0x6ea2ff)
}

/// 떠 있는 창의 배경.
pub fn popover() -> Rgba {
    rgb(0x313338)
}
