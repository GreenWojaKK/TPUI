//! GPUI 앱이 함께 쓰는 화면 부품. Parquet 뷰어와 다른 데스크톱 도구가 같은 테마와 입력·표·
//! 탭 막대·차트를 쓴다.
//!
//! 데이터 엔진에 의존하지 않고 GPUI만 쓴다. 이 crate를 쓰는 앱은 같은 GPUI 커밋을 써야 한다.

pub mod chart;
pub mod format;
pub mod heatmap;
pub mod tab_bar;
pub mod table;
pub mod text_input;
pub mod theme;
pub mod widgets;
