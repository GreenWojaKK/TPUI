//! Charts that layer lines, bands and rules. Drawn directly with GPUI's `paint_quad` and `PathBuilder`.
//!
//! Takes axis ranges and ticks plus the layers to draw (bands, lines, rules). Rectangular frames, grid lines and dots
//! use `paint_quad`, bands use `PathBuilder::fill`, and lines use `PathBuilder::stroke`. Bands and lines break at
//! missing points. On hover, the index of the nearest point is reported to the caller.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Corners, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseMoveEvent, PathBuilder, Pixels, Point, ShapedLine, SharedString, Style, TextAlign,
    TextRun, Window, fill, outline, point, px, relative, size,
};

use crate::theme;

/// A tick and its label.
pub type Tick = (f64, SharedString);

/// Value range, ticks and title of one axis.
#[derive(Clone, Debug)]
pub struct Axis {
    pub(crate) min: f64,
    pub(crate) max: f64,
    pub(crate) ticks: Vec<Tick>,
    pub(crate) title: Option<SharedString>,
}

impl Axis {
    /// Axis over `[min, max]`. An empty range is widened by 1.
    pub fn new(min: f64, max: f64) -> Self {
        let (min, max) = if max > min {
            (min, max)
        } else {
            (min, min + 1.)
        };
        Self {
            min,
            max,
            ticks: Vec::new(),
            title: None,
        }
    }

    pub fn ticks(mut self, ticks: impl IntoIterator<Item = Tick>) -> Self {
        self.ticks = ticks.into_iter().collect();
        self
    }

    /// Places about `count` ticks within the range at 1·2·2.5·5×10ⁿ steps. Labels use the fewest decimals
    /// that can express the step; for example, a step of 0.25 uses two decimals.
    pub fn nice_ticks(self, count: usize) -> Self {
        self.nice_ticks_with(count, |value, decimals| format!("{value:.decimals$}"))
    }

    /// Same ticks as [`Self::nice_ticks`], labeled with `format(value, decimals)`. Used to add units or
    /// signs.
    pub fn nice_ticks_with(mut self, count: usize, format: impl Fn(f64, usize) -> String) -> Self {
        let ticks = nice_ticks(self.min, self.max, count);
        let decimals = match ticks.as_slice() {
            [first, second, ..] => step_decimals(second - first),
            [only] => step_decimals(only.abs()),
            [] => 0,
        };
        self.ticks = ticks
            .into_iter()
            // Add 0 so that -0.0 does not show as "-0".
            .map(|value| (value, format(value + 0., decimals).into()))
            .collect();
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Maps a value to a position in `[start, start + len]`. Values outside the range map with the same ratio.
    pub(crate) fn position(&self, value: f64, start: Pixels, len: Pixels) -> Pixels {
        let ratio = (value - self.min) / (self.max - self.min);
        start + len * ratio as f32
    }
}

/// Band filled between lower and upper bounds. Draws ranges such as prediction intervals.
#[derive(Clone, Debug)]
pub struct Band {
    pub x: Vec<f64>,
    pub lower: Vec<Option<f64>>,
    pub upper: Vec<Option<f64>>,
    pub color: Hsla,
}

impl Band {
    pub fn new(x: Vec<f64>, lower: Vec<Option<f64>>, upper: Vec<Option<f64>>, color: Hsla) -> Self {
        Self {
            x,
            lower,
            upper,
            color,
        }
    }
}

/// Line through points. Can be dashed and show point markers.
#[derive(Clone, Debug)]
pub struct Line {
    pub x: Vec<f64>,
    pub y: Vec<Option<f64>>,
    pub color: Hsla,
    pub width: Pixels,
    /// Drawn and gap lengths of the dash pattern.
    pub dash: Option<(Pixels, Pixels)>,
    /// Draws a circle at each point.
    pub markers: bool,
}

impl Line {
    pub fn new(x: Vec<f64>, y: Vec<Option<f64>>, color: Hsla) -> Self {
        Self {
            x,
            y,
            color,
            width: px(2.),
            dash: None,
            markers: false,
        }
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    pub fn dashed(mut self, on: Pixels, off: Pixels) -> Self {
        self.dash = Some((on, off));
        self
    }

    pub fn markers(mut self) -> Self {
        self.markers = true;
        self
    }
}

/// Rule between two points in value coordinates. Draws reference lines such as a nominal value or a diagonal.
#[derive(Clone, Debug)]
pub struct Rule {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub color: Hsla,
    pub dash: Option<(Pixels, Pixels)>,
}

impl Rule {
    pub fn new(from: (f64, f64), to: (f64, f64), color: Hsla) -> Self {
        Self {
            from,
            to,
            color,
            dash: None,
        }
    }

    pub fn dashed(mut self, on: Pixels, off: Pixels) -> Self {
        self.dash = Some((on, off));
        self
    }
}

/// Layer drawn on the chart, in insertion order.
#[derive(Clone, Debug)]
pub enum Layer {
    Band(Band),
    Line(Line),
    Rule(Rule),
}

impl From<Band> for Layer {
    fn from(band: Band) -> Self {
        Self::Band(band)
    }
}

impl From<Line> for Layer {
    fn from(line: Line) -> Self {
        Self::Line(line)
    }
}

impl From<Rule> for Layer {
    fn from(rule: Rule) -> Self {
        Self::Rule(rule)
    }
}

/// Reports the mouse position as the index of the nearest point. `None` outside the plot.
pub type HoverHandler = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;

struct Hover {
    /// x values the mouse snaps to.
    positions: Vec<f64>,
    /// Index at which to draw the vertical guide.
    hovered: Option<usize>,
    handler: HoverHandler,
}

/// Line, band and rule chart. Fills the size given by its parent.
pub struct LineChart {
    x: Axis,
    y: Axis,
    layers: Vec<Layer>,
    hover: Option<Hover>,
}

impl LineChart {
    pub fn new(x: Axis, y: Axis) -> Self {
        Self {
            x,
            y,
            layers: Vec::new(),
            hover: None,
        }
    }

    pub fn layer(mut self, layer: impl Into<Layer>) -> Self {
        self.layers.push(layer.into());
        self
    }

    /// Reports the mouse snapped to the nearest x among `positions`. If `hovered` is set, draws a vertical
    /// guide at that position.
    pub fn on_hover(
        mut self,
        positions: Vec<f64>,
        hovered: Option<usize>,
        handler: impl Fn(Option<usize>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover = Some(Hover {
            positions,
            hovered,
            handler: Rc::new(handler),
        });
        self
    }
}

/// Layout decided in prepaint. Tick labels are shaped ahead of time.
pub struct ChartLayout {
    plot: Bounds<Pixels>,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    hitbox: Hitbox,
    line_height: Pixels,
}

impl IntoElement for LineChart {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LineChart {
    type RequestLayoutState = ();
    type PrepaintState = ChartLayout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(full_size(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let text = TextShaper::new(window);
        let gap = px(6.);
        let y_labels: Vec<_> = self
            .y
            .ticks
            .iter()
            .map(|(value, label)| (*value, text.shape(label.clone())))
            .collect();
        let x_labels: Vec<_> = self
            .x
            .ticks
            .iter()
            .map(|(value, label)| (*value, text.shape(label.clone())))
            .collect();
        let widest_y = y_labels
            .iter()
            .map(|(_, line)| line.width())
            .fold(px(0.), Pixels::max);
        let last_x = x_labels.last().map_or(px(0.), |(_, line)| line.width());
        let top = if self.y.title.is_some() {
            text.line_height + gap
        } else {
            gap
        };
        let bottom = text.line_height
            + gap
            + if self.x.title.is_some() {
                text.line_height + gap / 2.
            } else {
                px(0.)
            };
        let left = widest_y + gap * 2.;
        let right = (last_x / 2.).max(gap * 2.);
        let plot = Bounds::from_corners(
            point(bounds.left() + left, bounds.top() + top),
            point(
                (bounds.right() - right).max(bounds.left() + left + px(1.)),
                (bounds.bottom() - bottom).max(bounds.top() + top + px(1.)),
            ),
        );

        let mut labels = Vec::new();
        for (value, line) in y_labels {
            let y = self.y_position(value, &plot);
            let origin = point(plot.left() - gap - line.width(), y - text.line_height / 2.);
            labels.push((line, origin));
        }
        for (value, line) in x_labels {
            let x = self.x_position(value, &plot);
            let origin = point(x - line.width() / 2., plot.bottom() + gap / 2.);
            labels.push((line, origin));
        }
        if let Some(title) = &self.x.title {
            let line = text.shape(title.clone());
            let origin = point(
                plot.left() + (plot.size.width - line.width()) / 2.,
                plot.bottom() + text.line_height + gap,
            );
            labels.push((line, origin));
        }
        if let Some(title) = &self.y.title {
            let line = text.shape(title.clone());
            labels.push((line, point(plot.left(), bounds.top())));
        }

        ChartLayout {
            plot,
            labels,
            hitbox: window.insert_hitbox(plot, HitboxBehavior::Normal),
            line_height: text.line_height,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let plot = layout.plot;
        // Horizontal grid lines go below the layers.
        for (value, _) in &self.y.ticks {
            let y = self.y_position(*value, &plot);
            window.paint_quad(fill(
                Bounds::new(point(plot.left(), y), size(plot.size.width, px(1.))),
                theme::grid_line(),
            ));
        }
        window.with_content_mask(Some(ContentMask { bounds: plot }), |window| {
            for layer in &self.layers {
                match layer {
                    Layer::Band(band) => self.paint_band(band, &plot, window),
                    Layer::Line(line) => self.paint_line(line, &plot, window),
                    Layer::Rule(rule) => self.paint_rule(rule, &plot, window),
                }
            }
            if let Some(hover) = &self.hover
                && let Some(x) = hover.hovered.and_then(|ix| hover.positions.get(ix))
            {
                let x = self.x_position(*x, &plot);
                window.paint_quad(fill(
                    Bounds::new(point(x, plot.top()), size(px(1.), plot.size.height)),
                    theme::text_muted(),
                ));
            }
        });
        window.paint_quad(outline(plot, theme::border(), BorderStyle::Solid));
        for (line, origin) in &layout.labels {
            let _ = line.paint(
                *origin,
                layout.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        if let Some(hover) = &self.hover {
            let hitbox = layout.hitbox.clone();
            let positions = hover.positions.clone();
            let handler = hover.handler.clone();
            let x_axis = self.x.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let index = hitbox.is_hovered(window).then(|| {
                    let ratio = f64::from((event.position.x - plot.left()) / plot.size.width);
                    let value = x_axis.min + ratio * (x_axis.max - x_axis.min);
                    nearest(&positions, value)
                });
                handler(index.flatten(), window, cx);
            });
        }
    }
}

impl LineChart {
    fn x_position(&self, value: f64, plot: &Bounds<Pixels>) -> Pixels {
        self.x.position(value, plot.left(), plot.size.width)
    }

    fn y_position(&self, value: f64, plot: &Bounds<Pixels>) -> Pixels {
        // Screen y grows downward, so flip from the top.
        plot.bottom() - (self.y.position(value, plot.top(), plot.size.height) - plot.top())
    }

    fn point(&self, x: f64, y: f64, plot: &Bounds<Pixels>) -> Point<Pixels> {
        point(self.x_position(x, plot), self.y_position(y, plot))
    }

    fn paint_band(&self, band: &Band, plot: &Bounds<Pixels>, window: &mut Window) {
        let len = band.x.len().min(band.lower.len()).min(band.upper.len());
        let valid = |ix: usize| band.lower[ix].is_some() && band.upper[ix].is_some();
        for run in runs(len, valid) {
            if run.len() < 2 {
                continue;
            }
            let mut path = PathBuilder::fill();
            path.move_to(self.point(band.x[run.start], band.upper[run.start].unwrap(), plot));
            for ix in run.clone().skip(1) {
                path.line_to(self.point(band.x[ix], band.upper[ix].unwrap(), plot));
            }
            for ix in run.clone().rev() {
                path.line_to(self.point(band.x[ix], band.lower[ix].unwrap(), plot));
            }
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, band.color);
            }
        }
    }

    fn paint_line(&self, line: &Line, plot: &Bounds<Pixels>, window: &mut Window) {
        let len = line.x.len().min(line.y.len());
        let runs = runs(len, |ix| line.y[ix].is_some());
        let mut path = stroke(line.width, line.dash);
        let mut has_segment = false;
        for run in &runs {
            if run.len() < 2 {
                continue;
            }
            path.move_to(self.point(line.x[run.start], line.y[run.start].unwrap(), plot));
            for ix in run.clone().skip(1) {
                path.line_to(self.point(line.x[ix], line.y[ix].unwrap(), plot));
            }
            has_segment = true;
        }
        if has_segment && let Ok(path) = path.build() {
            window.paint_path(path, line.color);
        }
        // Points without neighbours, which form no line, and point markers are drawn as circles.
        let radius = (line.width * 1.6).max(px(2.5));
        for run in &runs {
            if !line.markers && run.len() > 1 {
                continue;
            }
            for ix in run.clone() {
                let center = self.point(line.x[ix], line.y[ix].unwrap(), plot);
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(center.x - radius, center.y - radius),
                            size(radius * 2., radius * 2.),
                        ),
                        line.color,
                    )
                    .corner_radii(Corners::all(radius)),
                );
            }
        }
    }

    fn paint_rule(&self, rule: &Rule, plot: &Bounds<Pixels>, window: &mut Window) {
        let mut path = stroke(px(1.), rule.dash);
        path.move_to(self.point(rule.from.0, rule.from.1, plot));
        path.line_to(self.point(rule.to.0, rule.to.1, plot));
        if let Ok(path) = path.build() {
            window.paint_path(path, rule.color);
        }
    }
}

fn stroke(width: Pixels, dash: Option<(Pixels, Pixels)>) -> PathBuilder {
    let builder = PathBuilder::stroke(width);
    match dash {
        Some((on, off)) => builder.dash_array(&[on, off]),
        None => builder,
    }
}

fn full_size() -> Style {
    let mut style = Style::default();
    style.size.width = relative(1.).into();
    style.size.height = relative(1.).into();
    style
}

/// Shapes and measures single lines of text with the window's text style. Text is muted unless another
/// color is set.
#[derive(Clone)]
pub(crate) struct TextShaper {
    font: gpui::Font,
    font_size: Pixels,
    pub(crate) line_height: Pixels,
    color: Hsla,
    text_system: std::sync::Arc<gpui::WindowTextSystem>,
}

impl TextShaper {
    pub(crate) fn new(window: &Window) -> Self {
        let style = window.text_style();
        Self {
            font: style.font(),
            font_size: style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
            color: theme::text_muted().into(),
            text_system: window.text_system().clone(),
        }
    }

    pub(crate) fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = color.into();
        self
    }

    fn run(&self, len: usize) -> TextRun {
        TextRun {
            len,
            font: self.font.clone(),
            color: self.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    pub(crate) fn shape(&self, text: SharedString) -> ShapedLine {
        let run = self.run(text.len());
        self.text_system
            .shape_line(text, self.font_size, &[run], None)
    }

    /// Width of `text` laid out on one line.
    pub(crate) fn width(&self, text: &str) -> Pixels {
        if text.is_empty() {
            return px(0.);
        }
        self.text_system
            .layout_line(text, self.font_size, &[self.run(text.len())], None)
            .width
    }
}

/// Consecutive runs that satisfy the predicate.
pub(crate) fn runs(len: usize, valid: impl Fn(usize) -> bool) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut start = None;
    for ix in 0..len {
        match (valid(ix), start) {
            (true, None) => start = Some(ix),
            (false, Some(begin)) => {
                runs.push(begin..ix);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        runs.push(begin..len);
    }
    runs
}

/// Index of the position nearest to `value`.
pub(crate) fn nearest(positions: &[f64], value: f64) -> Option<usize> {
    positions
        .iter()
        .enumerate()
        .filter(|(_, position)| position.is_finite())
        .min_by(|(_, a), (_, b)| (*a - value).abs().total_cmp(&(*b - value).abs()))
        .map(|(ix, _)| ix)
}

/// Fewest decimals that represent every multiple of `step` exactly.
pub fn step_decimals(step: f64) -> usize {
    if !step.is_finite() || step == 0. {
        return 0;
    }
    (0..=12)
        .find(|decimals| {
            let scaled = step.abs() * 10f64.powi(*decimals as i32);
            (scaled - scaled.round()).abs() < 1e-6 * scaled.max(1.)
        })
        .unwrap_or(12)
}

/// About `count` tick values within `[min, max]` at 1·2·2.5·5×10ⁿ steps.
pub fn nice_ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    if !(min.is_finite() && max.is_finite()) || max <= min || count == 0 {
        return Vec::new();
    }
    let raw = (max - min) / count as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1., 2., 2.5, 5., 10.]
        .into_iter()
        .map(|factor| factor * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10. * magnitude);
    let first = (min / step).ceil();
    let last = (max / step + 1e-9).floor();
    (first as i64..=last as i64)
        .map(|ix| {
            // Round to the decimals so floating-point error does not produce values like 0.30000000000000004.
            let value = ix as f64 * step;
            (value / step).round() * step
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_runs_at_missing_values() {
        let values = [Some(1), None, Some(2), Some(3), None];
        assert_eq!(
            runs(values.len(), |ix| values[ix].is_some()),
            vec![0..1, 2..4]
        );
        assert!(runs(3, |_| false).is_empty());
        assert_eq!(runs(3, |_| true), vec![0..3]);
    }

    #[test]
    fn picks_the_nearest_position() {
        let positions = [1., 2., 3., f64::NAN, 10.];
        assert_eq!(nearest(&positions, 2.4), Some(1));
        assert_eq!(nearest(&positions, 7.), Some(4));
        assert_eq!(nearest(&[], 1.), None);
    }

    #[test]
    fn chooses_round_tick_steps() {
        assert_eq!(nice_ticks(0., 12.4, 4), vec![0., 5., 10.]);
        assert_eq!(nice_ticks(0., 1., 4), vec![0., 0.25, 0.5, 0.75, 1.]);
        assert_eq!(nice_ticks(-3., 3., 6), vec![-3., -2., -1., 0., 1., 2., 3.]);
        assert!(nice_ticks(1., 1., 4).is_empty());
    }

    #[test]
    fn labels_ticks_with_enough_decimals() {
        assert_eq!(step_decimals(0.25), 2);
        assert_eq!(step_decimals(0.2), 1);
        assert_eq!(step_decimals(5.), 0);
        assert_eq!(step_decimals(2.5), 1);
        let labels: Vec<String> = Axis::new(-0.415, 0.415)
            .nice_ticks(4)
            .ticks
            .into_iter()
            .map(|(_, label)| label.to_string())
            .collect();
        assert_eq!(labels, ["-0.25", "0.00", "0.25"]);
    }

    #[test]
    fn maps_values_onto_the_plot() {
        let axis = Axis::new(0., 10.);
        assert_eq!(axis.position(5., px(100.), px(200.)), px(200.));
        let empty = Axis::new(3., 3.);
        assert_eq!(empty.position(3., px(0.), px(10.)), px(0.));
    }
}
