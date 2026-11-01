//! UI components shared by GPUI apps. The Parquet viewer and other desktop tools use the same theme, inputs,
//! tables, tab bar and charts.
//!
//! Depends only on GPUI, not on the data engine. Apps using this crate must use the same GPUI commit.

pub mod chart;
pub mod format;
pub mod heatmap;
pub mod map;
pub mod tab_bar;
pub mod table;
pub mod text_input;
pub mod theme;
pub mod widgets;
