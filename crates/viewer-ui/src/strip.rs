//! Strip chart: points on a shared horizontal axis, one row per category, and vertical markers across the rows.
//! Shows when events happen, such as the times buses pass a stop. Drawn with `paint_quad` and `PathBuilder`.
//!
//! Row labels sit in a left column and wrap at spaces when they are wider than the column. Points may carry
//! labels, written alternately above and below the track so that neighbouring labels do not collide. Axis ticks
//! are marked on every track and labeled under the last row. Points outside the axis range are not drawn. The
//! chart fills its parent's width and is as tall as its rows.

use std::rc::Rc;

use gpui::{
    App, Bounds, Corners, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseMoveEvent, PathBuilder,
    Pixels, Point, ShapedLine, SharedString, Style, TextAlign, Window, fill, point, px, relative,
    size,
};

use crate::chart::{Axis, TextShaper};
use crate::theme;
use crate::wrap::{LabelColumn, definite_width};

/// One row: a label and the axis values of its points.
#[derive(Clone, Debug)]
pub struct StripRow {
    pub label: SharedString,
    pub values: Vec<f64>,
    /// Label of each point, in the order of `values`.
    pub value_labels: Vec<SharedString>,
    pub color: Hsla,
}

impl StripRow {
    pub fn new(label: impl Into<SharedString>, values: Vec<f64>, color: Hsla) -> Self {
        Self {
            label: label.into(),
            values,
            value_labels: Vec::new(),
            color,
        }
    }

    /// Writes a label next to each point.
    pub fn value_labels(mut self, labels: Vec<SharedString>) -> Self {
        self.value_labels = labels;
        self
    }
}

/// Dashed vertical line across every row at one axis value, with an optional label above the rows.
#[derive(Clone, Debug)]
pub struct StripMarker {
    pub value: f64,
    pub label: Option<SharedString>,
    pub color: Hsla,
}

impl StripMarker {
    pub fn new(value: f64, color: Hsla) -> Self {
        Self {
            value,
            label: None,
            color,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Reports the hovered point as (row, index in the row's values). `None` when no point is near the mouse.
pub type PointHoverHandler = Rc<dyn Fn(Option<(usize, usize)>, &mut Window, &mut App)>;

pub struct StripChart {
    axis: Axis,
    rows: Vec<StripRow>,
    markers: Vec<StripMarker>,
    empty_label: Option<SharedString>,
    track_thickness: Pixels,
    dot_radius: Pixels,
    background: Option<Hsla>,
    hover: Option<(Option<(usize, usize)>, PointHoverHandler)>,
}

/// Ring around each point in the background color, so that overlapping points stay apart.
const DOT_RING: Pixels = px(2.);
/// Length of tick and marker lines beyond the track.
const OVERHANG: Pixels = px(4.);
const LABEL_GAP: Pixels = px(2.);
const ROW_GAP: Pixels = px(14.);
const COLUMN_GAP: Pixels = px(12.);
/// Largest share of the width given to row labels.
const LABEL_SHARE: f32 = 0.3;
/// Largest horizontal distance at which the mouse hovers a point.
const HOVER_REACH: Pixels = px(10.);

impl StripChart {
    pub fn new(axis: Axis, rows: Vec<StripRow>) -> Self {
        Self {
            axis,
            rows,
            markers: Vec::new(),
            empty_label: None,
            track_thickness: px(8.),
            dot_radius: px(5.5),
            background: None,
            hover: None,
        }
    }

    /// Thickness of each row's track. 8 px by default.
    pub fn track_thickness(mut self, thickness: Pixels) -> Self {
        self.track_thickness = thickness;
        self
    }

    /// Radius of each point. 5.5 px by default.
    pub fn dot_radius(mut self, radius: Pixels) -> Self {
        self.dot_radius = radius;
        self
    }

    /// Color behind the chart. Points get a ring of this color so that overlapping points stay apart. The surface
    /// color of the theme by default.
    pub fn background(mut self, color: impl Into<Hsla>) -> Self {
        self.background = Some(color.into());
        self
    }

    pub fn marker(mut self, marker: StripMarker) -> Self {
        self.markers.push(marker);
        self
    }

    /// Written on rows without points, such as "no departures in this range".
    pub fn empty_label(mut self, label: impl Into<SharedString>) -> Self {
        self.empty_label = Some(label.into());
        self
    }

    /// Reports the point nearest the mouse within its row and rings the `hovered` point.
    pub fn on_hover(
        mut self,
        hovered: Option<(usize, usize)>,
        handler: impl Fn(Option<(usize, usize)>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover = Some((hovered, Rc::new(handler)));
        self
    }

    fn shape(&self) -> Shape {
        Shape {
            labels: self.rows.iter().map(|row| row.label.clone()).collect(),
            rows: self
                .rows
                .iter()
                .map(|row| RowShape {
                    above: !row.value_labels.is_empty(),
                    below: !row.value_labels.is_empty()
                        || (row.values.is_empty() && self.empty_label.is_some()),
                })
                .collect(),
            marker_labels: self.markers.iter().any(|marker| marker.label.is_some()),
            ticks: !self.axis.ticks.is_empty(),
            title: self.axis.title.is_some(),
            point_band: ((self.dot_radius + DOT_RING) * 2.).max(self.track_thickness),
        }
    }

    fn in_range(&self, value: f64) -> bool {
        value.is_finite() && value >= self.axis.min && value <= self.axis.max
    }
}

/// What the height of the chart depends on, kept apart so that layout can measure without the values.
#[derive(Clone)]
struct Shape {
    labels: Vec<SharedString>,
    rows: Vec<RowShape>,
    marker_labels: bool,
    ticks: bool,
    title: bool,
    /// Height of the band holding a row's track and points.
    point_band: Pixels,
}

/// Whether a row writes text above and below its track.
#[derive(Clone, Copy)]
struct RowShape {
    above: bool,
    below: bool,
}

/// Vertical placement for one width, measured from the chart top.
struct Frame {
    labels: LabelColumn,
    /// Top and height of each row.
    rows: Vec<(Pixels, Pixels)>,
    /// Vertical center of each row's track.
    centers: Vec<Pixels>,
    rows_top: Pixels,
    rows_bottom: Pixels,
    axis_top: Pixels,
    height: Pixels,
}

impl Shape {
    fn frame(&self, width: Option<Pixels>, text: &TextShaper) -> Frame {
        let line_height = text.line_height;
        let max_label = width.map_or(px(f32::MAX), |width| width * LABEL_SHARE);
        let labels = LabelColumn::new(&self.labels, max_label, text);
        let dot = self.point_band;
        let text_line = line_height + LABEL_GAP;
        let rows_top = if self.marker_labels {
            text_line + OVERHANG
        } else {
            OVERHANG
        };
        let mut rows = Vec::new();
        let mut centers = Vec::new();
        let mut y = rows_top;
        for (ix, row) in self.rows.iter().enumerate() {
            let above = if row.above { text_line } else { px(0.) };
            let below = if row.below { text_line } else { px(0.) };
            let band = above + dot + below;
            let height = band.max(labels.height(ix, line_height));
            rows.push((y, height));
            centers.push(y + (height - band) / 2. + above + dot / 2.);
            y += height + ROW_GAP;
        }
        let rows_bottom = if rows.is_empty() { y } else { y - ROW_GAP };
        let axis_top = rows_bottom + OVERHANG;
        let mut height = axis_top;
        if self.ticks {
            height += line_height;
        }
        if self.title {
            height += text_line;
        }
        Frame {
            labels,
            rows,
            centers,
            rows_top,
            rows_bottom,
            axis_top,
            height,
        }
    }
}

struct Dot {
    row: usize,
    index: usize,
    center: Point<Pixels>,
    color: Hsla,
}

pub struct StripChartLayout {
    tracks: Vec<Bounds<Pixels>>,
    ticks: Vec<Bounds<Pixels>>,
    markers: Vec<(Pixels, Hsla)>,
    dots: Vec<Dot>,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    rows_top: Pixels,
    rows_bottom: Pixels,
    /// Vertical extent of each row for hovering.
    row_bands: Vec<(Pixels, Pixels)>,
    hitbox: Hitbox,
    line_height: Pixels,
}

impl IntoElement for StripChart {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for StripChart {
    type RequestLayoutState = ();
    type PrepaintState = StripChartLayout;

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
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = TextShaper::new(window);
        let shape = self.shape();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, _window, _cx| {
                let width = definite_width(known, available);
                let frame = shape.frame(width, &text);
                size(width.unwrap_or(frame.labels.width + px(320.)), frame.height)
            });
        (layout_id, ())
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
        let muted = TextShaper::new(window);
        let text = muted.clone().color(theme::text());
        let line_height = text.line_height;
        let frame = self.shape().frame(Some(bounds.size.width), &text);
        let top = bounds.top();

        let tick_labels: Vec<_> = self
            .axis
            .ticks
            .iter()
            .map(|(value, label)| (*value, muted.shape(label.clone())))
            .collect();
        let last_tick = tick_labels.last().map_or(px(0.), |(_, line)| line.width());
        let label_gap = if frame.labels.width > px(0.) {
            COLUMN_GAP
        } else {
            px(0.)
        };
        let plot_left = bounds.left() + frame.labels.width + label_gap + self.dot_radius;
        let plot_right = (bounds.right() - (last_tick / 2.).max(self.dot_radius + DOT_RING))
            .max(plot_left + px(1.));
        let x = |value: f64| self.axis.position(value, plot_left, plot_right - plot_left);
        // Keeps a label of `width` inside the chart.
        let clamp =
            |left: Pixels, width: Pixels| left.min(bounds.right() - width).max(bounds.left());

        let mut labels = Vec::new();
        let mut tracks = Vec::new();
        let mut ticks = Vec::new();
        let mut dots = Vec::new();
        let dot = self.dot_radius + DOT_RING;
        let track = self.track_thickness;
        for (ix, row) in self.rows.iter().enumerate() {
            let (row_top, row_height) = frame.rows[ix];
            let center = top + frame.centers[ix];
            tracks.push(Bounds::new(
                point(plot_left, center - track / 2.),
                size(plot_right - plot_left, track),
            ));
            for (value, _) in &self.axis.ticks {
                ticks.push(Bounds::new(
                    point(x(*value), center - track / 2. - OVERHANG),
                    size(px(1.), track + OVERHANG * 2.),
                ));
            }

            // The row label is centered on the track, within the row.
            let label_height = frame.labels.height(ix, line_height);
            let label_top = (center - label_height / 2.)
                .min(top + row_top + row_height - label_height)
                .max(top + row_top);
            for (line_ix, range) in frame.labels.lines[ix].iter().enumerate() {
                let line = text.shape(row.label[range.clone()].to_string().into());
                labels.push((
                    line,
                    point(bounds.left(), label_top + line_height * line_ix as f32),
                ));
            }

            for (index, value) in row.values.iter().enumerate() {
                if !self.in_range(*value) {
                    continue;
                }
                let at = point(x(*value), center);
                dots.push(Dot {
                    row: ix,
                    index,
                    center: at,
                    color: row.color,
                });
                if let Some(label) = row.value_labels.get(index) {
                    let line = text.shape(label.clone());
                    let y = if index % 2 == 0 {
                        center - dot - LABEL_GAP - line_height
                    } else {
                        center + dot + LABEL_GAP
                    };
                    let left = clamp(at.x - line.width() / 2., line.width());
                    labels.push((line, point(left, y)));
                }
            }
            if row.values.is_empty()
                && let Some(empty) = &self.empty_label
            {
                labels.push((
                    muted.shape(empty.clone()),
                    point(plot_left + px(8.), center + dot + LABEL_GAP),
                ));
            }
        }

        let mut markers = Vec::new();
        for marker in &self.markers {
            if !self.in_range(marker.value) {
                continue;
            }
            let at = x(marker.value);
            markers.push((at, marker.color));
            if let Some(label) = &marker.label {
                let line = muted.clone().color(marker.color).shape(label.clone());
                let left = clamp(at - line.width() / 2., line.width());
                labels.push((line, point(left, top)));
            }
        }

        let axis_top = top + frame.axis_top;
        for (value, line) in tick_labels {
            let left = clamp(x(value) - line.width() / 2., line.width());
            labels.push((line, point(left, axis_top)));
        }
        if let Some(title) = &self.axis.title {
            let line = muted.shape(title.clone());
            let left = plot_left + (plot_right - plot_left - line.width()) / 2.;
            labels.push((line, point(left, axis_top + line_height + LABEL_GAP)));
        }

        StripChartLayout {
            tracks,
            ticks,
            markers,
            dots,
            labels,
            rows_top: top + frame.rows_top,
            rows_bottom: top + frame.rows_bottom,
            row_bands: frame
                .rows
                .iter()
                .map(|(row_top, height)| {
                    (
                        top + *row_top - ROW_GAP / 2.,
                        top + *row_top + *height + ROW_GAP / 2.,
                    )
                })
                .collect(),
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
            line_height,
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
        for track in &layout.tracks {
            window.paint_quad(
                fill(*track, theme::track()).corner_radii(Corners::all(self.track_thickness / 2.)),
            );
        }
        for tick in &layout.ticks {
            window.paint_quad(fill(*tick, theme::border()));
        }
        for (x, color) in &layout.markers {
            let mut path = PathBuilder::stroke(px(2.)).dash_array(&[px(4.), px(3.)]);
            path.move_to(point(*x, layout.rows_top - OVERHANG));
            path.line_to(point(*x, layout.rows_bottom + OVERHANG));
            if let Ok(path) = path.build() {
                window.paint_path(path, *color);
            }
        }
        let hovered = self.hover.as_ref().and_then(|(hovered, _)| *hovered);
        let radius = self.dot_radius + DOT_RING;
        let background = self.background.unwrap_or_else(|| theme::surface().into());
        for dot in &layout.dots {
            let ring = if hovered == Some((dot.row, dot.index)) {
                theme::text().into()
            } else {
                background
            };
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(dot.center.x - radius, dot.center.y - radius),
                        size(radius * 2., radius * 2.),
                    ),
                    dot.color,
                )
                .corner_radii(Corners::all(radius))
                .border_widths(DOT_RING)
                .border_color(ring),
            );
        }
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

        if let Some((_, handler)) = &self.hover {
            let hitbox = layout.hitbox.clone();
            let handler = handler.clone();
            let bands = layout.row_bands.clone();
            let dots: Vec<_> = layout
                .dots
                .iter()
                .map(|dot| (dot.row, dot.index, dot.center.x))
                .collect();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let position = event.position;
                let hovered = hitbox
                    .is_hovered(window)
                    .then(|| {
                        let row = bands
                            .iter()
                            .position(|(top, bottom)| position.y >= *top && position.y < *bottom)?;
                        dots.iter()
                            .filter(|(dot_row, _, x)| {
                                *dot_row == row && (*x - position.x).abs() <= HOVER_REACH
                            })
                            .min_by(|a, b| {
                                f32::from((a.2 - position.x).abs())
                                    .total_cmp(&f32::from((b.2 - position.x).abs()))
                            })
                            .map(|(row, index, _)| (*row, *index))
                    })
                    .flatten();
                handler(hovered, window, cx);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::rgb;

    use super::*;

    #[test]
    fn draws_only_points_inside_the_axis() {
        let chart = StripChart::new(Axis::new(360., 600.), Vec::new());
        assert!(chart.in_range(360.) && chart.in_range(600.) && chart.in_range(437.));
        assert!(!chart.in_range(359.) && !chart.in_range(601.) && !chart.in_range(f64::NAN));
    }

    #[test]
    fn reserves_text_lines_only_where_rows_write_them() {
        let color: Hsla = rgb(0x1c7ed6).into();
        let chart = StripChart::new(
            Axis::new(0., 10.),
            vec![
                StripRow::new("labeled", vec![1., 2.], color)
                    .value_labels(vec!["1".into(), "2".into()]),
                StripRow::new("plain", vec![3.], color),
                StripRow::new("empty", Vec::new(), color),
            ],
        )
        .empty_label("none");
        let shape = chart.shape();
        let flags: Vec<_> = shape
            .rows
            .iter()
            .map(|row| (row.above, row.below))
            .collect();
        assert_eq!(flags, [(true, true), (false, false), (false, true)]);
        assert!(!shape.marker_labels && !shape.ticks && !shape.title);
        let marked = StripChart::new(Axis::new(0., 10.).nice_ticks(2), Vec::new())
            .marker(StripMarker::new(5., color).label("now"))
            .shape();
        assert!(marked.marker_labels && marked.ticks);
    }
}
