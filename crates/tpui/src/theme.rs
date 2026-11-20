//! Colors, fonts and sizes of the shared components.
//!
//! Components read colors by role (background, surface, border, text and so on) and never hold color values
//! themselves. The values live in a [`Theme`]: a dark and a light [`Palette`], [`Metrics`] and [`Fonts`]. An app
//! that wants its own look starts from [`Theme::default`], changes the fields it needs and passes the result to
//! [`install`] once, before anything is drawn. Without [`install`], the default theme is used.
//!
//! There is one color mode per process, dark by default. Color functions read the current mode on every paint, so
//! redrawing the window after [`set_mode`] applies it at once.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{Pixels, Rgba, SharedString, WindowAppearance, px, rgb};

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

/// Colors of one mode, by role.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Palette {
    /// Window background.
    pub background: Rgba,
    /// Panels, bars and table headers on the background.
    pub surface: Rgba,
    /// Surfaces floating above the window: tooltips, menus and popovers.
    pub overlay: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    /// Secondary text: labels, captions, axis ticks.
    pub text_muted: Rgba,
    /// Places without a value: null values and missing bars.
    pub missing: Rgba,
    pub error: Rgba,
    pub selection: Rgba,
    pub hover: Rgba,
    pub button: Rgba,
    pub button_hover: Rgba,
    /// Marks that call attention, such as the filter and sort markers.
    pub accent: Rgba,
    pub grid_line: Rgba,
    /// Background of bar and strip chart tracks.
    pub track: Rgba,
    pub scrollbar_track: Rgba,
    pub scrollbar_thumb: Rgba,
    /// Drop position for a dragged tab.
    pub drop_target: Rgba,
}

impl Palette {
    fn default_dark() -> Self {
        Self {
            background: rgb(0x1e1f22),
            surface: rgb(0x2b2d30),
            overlay: rgb(0x313338),
            border: rgb(0x393b40),
            text: rgb(0xdfe1e5),
            text_muted: rgb(0x8c8f94),
            missing: rgb(0x7f8ea3),
            error: rgb(0xf27c7c),
            selection: rgb(0x2e436e),
            hover: rgb(0x2a2c30),
            button: rgb(0x3c3f45),
            button_hover: rgb(0x4a4e55),
            accent: rgb(0x6ea2ff),
            grid_line: rgb(0x2f3135),
            track: rgb(0x3a3d43),
            scrollbar_track: rgb(0x232427),
            scrollbar_thumb: rgb(0x4a4e55),
            drop_target: rgb(0x2e436e),
        }
    }

    fn default_light() -> Self {
        Self {
            background: rgb(0xf5f6f8),
            surface: rgb(0xffffff),
            overlay: rgb(0xffffff),
            border: rgb(0xd9dce1),
            text: rgb(0x1f2328),
            text_muted: rgb(0x646a73),
            missing: rgb(0x6b7a90),
            error: rgb(0xc62828),
            selection: rgb(0xd3e1fb),
            hover: rgb(0xeceef1),
            button: rgb(0xe6e8eb),
            button_hover: rgb(0xd6d9de),
            accent: rgb(0x2f6fde),
            grid_line: rgb(0xe5e7ea),
            track: rgb(0xe5e7ea),
            scrollbar_track: rgb(0xeef0f2),
            scrollbar_thumb: rgb(0xc3c7cd),
            drop_target: rgb(0xd3e1fb),
        }
    }
}

/// Sizes that several components follow together, setting how dense the UI is.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Metrics {
    /// Height of fixed table rows and table headers.
    pub list_row_height: Pixels,
    /// Height of panel headers, including the tab bar.
    pub panel_header_height: Pixels,
    /// Horizontal padding of table cells.
    pub cell_padding_x: Pixels,
    /// Vertical padding of wrapping table rows.
    pub wrapped_row_padding_y: Pixels,
    pub button_padding_x: Pixels,
    pub button_padding_y: Pixels,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            list_row_height: px(24.),
            panel_header_height: px(28.),
            cell_padding_x: px(8.),
            wrapped_row_padding_y: px(3.),
            button_padding_x: px(8.),
            button_padding_y: px(2.),
        }
    }
}

/// Font families.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Fonts {
    /// Font of the UI. On Windows this is Segoe UI, set explicitly because the system UI font of Korean Windows
    /// (Malgun Gothic) draws `\` as `₩`; Hangul falls back to the Windows fallback font.
    pub ui: SharedString,
    /// Monospace font, such as for value detail bodies.
    pub mono: SharedString,
}

impl Default for Fonts {
    #[cfg(target_os = "windows")]
    fn default() -> Self {
        Self {
            ui: SharedString::new_static("Segoe UI"),
            mono: SharedString::new_static("Consolas"),
        }
    }

    // Other systems have no common UI font; DejaVu ships with most Linux distributions.
    #[cfg(not(target_os = "windows"))]
    fn default() -> Self {
        Self {
            ui: SharedString::new_static("DejaVu Sans"),
            mono: SharedString::new_static("DejaVu Sans Mono"),
        }
    }
}

/// Everything an app can change about how the shared components look.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Theme {
    pub dark: Palette,
    pub light: Palette,
    pub metrics: Metrics,
    pub fonts: Fonts,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            dark: Palette::default_dark(),
            light: Palette::default_light(),
            metrics: Metrics::default(),
            fonts: Fonts::default(),
        }
    }
}

static THEME: OnceLock<Theme> = OnceLock::new();
static LIGHT: AtomicBool = AtomicBool::new(false);

/// Sets the theme of this process. Call it once, before anything is drawn.
///
/// # Panics
///
/// If a theme is already installed or has been read: the default theme is then in use, and replacing it partway
/// would mix two looks.
pub fn install(theme: Theme) {
    if THEME.set(theme).is_err() {
        panic!("theme::install must run once, before anything reads the theme");
    }
}

fn current() -> &'static Theme {
    THEME.get_or_init(Theme::default)
}

fn palette() -> &'static Palette {
    let theme = current();
    match mode() {
        Mode::Dark => &theme.dark,
        Mode::Light => &theme.light,
    }
}

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

/// Color for the current mode. Apps use it for colors of their own that are not roles of the theme.
pub fn pick(dark: u32, light: u32) -> Rgba {
    rgb(match mode() {
        Mode::Dark => dark,
        Mode::Light => light,
    })
}

pub fn metrics() -> &'static Metrics {
    &current().metrics
}

pub fn ui_font() -> SharedString {
    current().fonts.ui.clone()
}

pub fn mono_font() -> SharedString {
    current().fonts.mono.clone()
}

pub fn background() -> Rgba {
    palette().background
}

pub fn surface() -> Rgba {
    palette().surface
}

pub fn overlay() -> Rgba {
    palette().overlay
}

pub fn border() -> Rgba {
    palette().border
}

pub fn text() -> Rgba {
    palette().text
}

pub fn text_muted() -> Rgba {
    palette().text_muted
}

pub fn missing() -> Rgba {
    palette().missing
}

pub fn error() -> Rgba {
    palette().error
}

pub fn selection() -> Rgba {
    palette().selection
}

pub fn hover() -> Rgba {
    palette().hover
}

pub fn button() -> Rgba {
    palette().button
}

pub fn button_hover() -> Rgba {
    palette().button_hover
}

pub fn accent() -> Rgba {
    palette().accent
}

pub fn grid_line() -> Rgba {
    palette().grid_line
}

pub fn track() -> Rgba {
    palette().track
}

pub fn scrollbar_track() -> Rgba {
    palette().scrollbar_track
}

pub fn scrollbar_thumb() -> Rgba {
    palette().scrollbar_thumb
}

pub fn drop_target() -> Rgba {
    palette().drop_target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "theme::install")]
    fn refuses_a_theme_installed_after_use() {
        let _ = background();
        install(Theme::default());
    }

    #[test]
    fn apps_change_only_the_fields_they_need() {
        let mut theme = Theme::default();
        theme.light.text = rgb(0x345575);
        theme.metrics.list_row_height = px(32.);
        assert_eq!(theme.light.text, rgb(0x345575));
        assert_eq!(theme.light.background, Palette::default_light().background);
        assert_eq!(theme.dark.text, Palette::default_dark().text);
        assert_eq!(theme.metrics.panel_header_height, px(28.));
    }
}
