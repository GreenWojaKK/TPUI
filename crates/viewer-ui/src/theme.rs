//! 앱 전체가 공유하는 글꼴, 색상과 고정 크기.
//!
//! 색상은 어두운 모드와 밝은 모드 두 벌이다. 모드는 프로세스 전체에 하나이며 기본값은 어두운 모드다.
//! 색상 함수는 그릴 때마다 현재 모드를 읽으므로, [`set_mode`] 뒤 창을 다시 그리면 바로 바뀐다.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Pixels, Rgba, WindowAppearance, px, rgb};

/// 화면 색상 모드.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

impl Mode {
    /// 운영체제의 앱 색상 설정에 맞는 모드.
    pub fn from_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Mode::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Mode::Dark,
        }
    }

    /// 반대 모드.
    pub fn toggled(self) -> Self {
        match self {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Dark,
        }
    }
}

static LIGHT: AtomicBool = AtomicBool::new(false);

/// 현재 색상 모드.
pub fn mode() -> Mode {
    if LIGHT.load(Ordering::Relaxed) {
        Mode::Light
    } else {
        Mode::Dark
    }
}

/// 색상 모드를 바꾼다. 열린 창은 다음에 그릴 때 새 색상을 쓰므로, 호출한 쪽이 창을 다시 그리게 한다.
pub fn set_mode(mode: Mode) {
    LIGHT.store(mode == Mode::Light, Ordering::Relaxed);
}

/// 현재 모드에 맞는 색. 앱이 자기 색을 두 벌로 정할 때도 쓴다.
pub fn pick(dark: u32, light: u32) -> Rgba {
    rgb(match mode() {
        Mode::Dark => dark,
        Mode::Light => light,
    })
}

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

/// 끌고 있는 탭을 놓을 자리.
pub fn drop_target() -> Rgba {
    pick(0x2e436e, 0xd3e1fb)
}

/// 포커스가 있는 입력의 테두리.
pub fn focus_border() -> Rgba {
    pick(0x3574f0, 0x3574f0)
}

/// 찾기 입력과 일치하는 셀의 배경.
pub fn find_match() -> Rgba {
    pick(0x5c4a1e, 0xffe7a0)
}

/// 필터가 있는 컬럼의 필터 버튼, 정렬 방향 표시.
pub fn accent() -> Rgba {
    pick(0x6ea2ff, 0x2f6fde)
}

/// 떠 있는 창의 배경.
pub fn popover() -> Rgba {
    pick(0x313338, 0xffffff)
}
