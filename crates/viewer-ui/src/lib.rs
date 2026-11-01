//! UI components shared by GPUI apps. The Parquet viewer and other desktop tools use the same theme, inputs,
//! tables, tab bar and charts.
//!
//! Depends only on GPUI, not on the data engine. Apps using this crate must use the same GPUI commit.

pub mod bars;
pub mod chart;
pub mod format;
pub mod heatmap;
pub mod inbox;
#[cfg(target_os = "windows")]
pub mod input_window;
pub mod map;
pub mod menu;
pub mod storage;
pub mod strip;
pub mod tab_bar;
pub mod table;
pub mod theme;
pub mod widgets;
pub mod wrap;
