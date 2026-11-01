//! Horizontal bar chart: one group of bars per row, one bar per series. Drawn with `paint_quad` and `PathBuilder`.
//!
//! Row labels sit in a left column and wrap at spaces when they are wider than the column. Bars start at 0, and
//! the scale ends at the largest value unless the caller sets it. Each bar's value is written in a column on the
//! right. A missing value leaves a hatched track and the series' missing label. The chart fills its parent's
//! width and is as tall as its rows.

use std::rc::Rc;

use gpui::{
    App, Bounds, ContentMask, Corners, DispatchPhase, Div, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseMoveEvent,
    PathBuilder, Pixels, Point, ShapedLine, SharedString, Style, TextAlign, Window, div, fill,
    outline, point, prelude::*, px, relative, size,
};

use crate::chart::TextShaper;
use crate::theme;
use crate::wrap::{LabelColumn, definite_width};

/// One series: a value for each row of the chart.
#[derive(Clone, Debug)]
pub struct BarSeries {
    pub label: SharedString,
    pub color: Hsla,
    /// Value of each row. `None` is a missing value; non-finite values count as missing.
    pub values: Vec<Option<f64>>,
    /// Written in place of a missing value.
    pub missing_label: Option<SharedString>,
}

impl BarSeries {
    pub fn new(label: impl Into<SharedString>, color: Hsla, values: Vec<Option<f64>>) -> Self {
        Self {
            label: label.into(),
            color,
            values,
            missing_label: None,
        }
    }

    pub fn missing_label(mut self, label: impl Into<SharedString>) -> Self {
        self.missing_label = Some(label.into());
        self
    }
}

/// Reports the hovered bar as (row, series). `None` outside the bars.
pub type BarHoverHandler = Rc<dyn Fn(Option<(usize, usize)>, &mut Window, &mut App)>;

/// Horizontal bar chart. Rows are drawn top to bottom, and series in order within each row.
pub struct BarChart {
    rows: Vec<SharedString>,
    series: Vec<BarSeries>,
    max: Option<f64>,
    format: Rc<dyn Fn(f64) -> String>,
    hover: Option<(Option<(usize, usize)>, BarHoverHandler)>,
}

const BAR_HEIGHT: Pixels = px(12.);
const BAR_GAP: Pixels = px(4.);
const ROW_GAP: Pixels = px(10.);
const COLUMN_GAP: Pixels = px(10.);
const BAR_RADIUS: Pixels = px(3.);
const HATCH_STEP: Pixels = px(5.);
/// Largest share of the width given to row labels.
const LABEL_SHARE: f32 = 0.35;

impl BarChart {
    pub fn new(rows: Vec<SharedString>, series: Vec<BarSeries>) -> Self {
        Self {
            rows,
            series,
            max: None,
            format: Rc::new(|value| value.to_string()),
            hover: None,
        }
    }

    /// Value at the right end of the scale. Larger values fill the whole track.
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    /// Format of the value labels, such as adding a unit.
    pub fn format(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.format = Rc::new(format);
        self
    }

    /// Reports the hovered bar and outlines the `hovered` bar.
    pub fn on_hover(
        mut self,
        hovered: Option<(usize, usize)>,
        handler: impl Fn(Option<(usize, usize)>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover = Some((hovered, Rc::new(handler)));
        self
    }

    /// Color swatch and label of each series, for the caller to place next to the chart.
    pub fn legend(&self) -> Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .children(self.series.iter().map(|series| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().w(px(12.)).h(px(9.)).rounded_sm().bg(series.color))
                    .child(series.label.clone())
            }))
    }

    fn value(&self, row: usize, series: usize) -> Option<f64> {
        self.series[series]
            .values
            .get(row)
            .copied()
            .flatten()
            .filter(|value| value.is_finite())
    }

    fn scale_max(&self) -> f64 {
        let max = self.max.unwrap_or_else(|| {
            (0..self.rows.len())
                .flat_map(|row| (0..self.series.len()).filter_map(move |s| self.value(row, s)))
                .fold(0., f64::max)
        });
        if max > 0. { max } else { 1. }
    }
}

/// Vertical placement of the rows for one width.
struct Rows {
    labels: LabelColumn,
    /// Top of each row, measured from the chart top.
    tops: Vec<Pixels>,
    heights: Vec<Pixels>,
    /// Height given to each bar, which also fits its value label.
    slot: Pixels,
    height: Pixels,
}

impl Rows {
    fn new(
        labels: &[SharedString],
        series: usize,
        width: Option<Pixels>,
        text: &TextShaper,
    ) -> Self {
        let max_label = width.map_or(px(f32::MAX), |width| width * LABEL_SHARE);
        let labels = LabelColumn::new(labels, max_label, text);
        let slot = (BAR_HEIGHT + BAR_GAP).max(text.line_height);
        let group = slot * series as f32;
        let mut tops = Vec::new();
        let mut heights = Vec::new();
        let mut y = px(0.);
        for row in 0..labels.lines.len() {
            let height = group.max(labels.height(row, text.line_height));
            tops.push(y);
            heights.push(height);
            y += height + ROW_GAP;
        }
        Self {
            labels,
            tops,
            heights,
            slot,
            height: (y - ROW_GAP).max(px(0.)),
        }
    }
}

struct Bar {
    row: usize,
    series: usize,
    track: Bounds<Pixels>,
    /// Filled length. `None` is a missing value.
    fill: Option<Pixels>,
    /// Band of the chart width that hovers this bar.
    slot: Bounds<Pixels>,
}

pub struct BarChartLayout {
    bars: Vec<Bar>,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    hitbox: Hitbox,
    line_height: Pixels,
}

impl IntoElement for BarChart {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for BarChart {
    type RequestLayoutState = ();
    type PrepaintState = BarChartLayout;

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
        let labels = self.rows.clone();
        let series = self.series.len();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, _window, _cx| {
                let width = definite_width(known, available);
                let rows = Rows::new(&labels, series, width, &text);
                size(width.unwrap_or(rows.labels.width + px(240.)), rows.height)
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
        let rows = Rows::new(
            &self.rows,
            self.series.len(),
            Some(bounds.size.width),
            &text,
        );

        // Value labels first, since the value column takes the width of the widest.
        let mut values = Vec::new();
        for row in 0..self.rows.len() {
            for (s, series) in self.series.iter().enumerate() {
                let line = match self.value(row, s) {
                    Some(value) => Some(text.shape((self.format)(value).into())),
                    None => series.missing_label.clone().map(|label| muted.shape(label)),
                };
                values.push(line);
            }
        }
        let value_width = values
            .iter()
            .flatten()
            .map(ShapedLine::width)
            .fold(px(0.), Pixels::max);
        let label_gap = if rows.labels.width > px(0.) {
            COLUMN_GAP
        } else {
            px(0.)
        };
        let track_left = bounds.left() + rows.labels.width + label_gap;
        let track_right = (bounds.right() - value_width - COLUMN_GAP).max(track_left + px(1.));
        let max = self.scale_max();

        let mut labels = Vec::new();
        let mut bars = Vec::new();
        let mut values = values.into_iter();
        for (row, label) in self.rows.iter().enumerate() {
            let top = bounds.top() + rows.tops[row];
            let height = rows.heights[row];
            let label_top = top + (height - rows.labels.height(row, line_height)) / 2.;
            for (ix, range) in rows.labels.lines[row].iter().enumerate() {
                let line = text.shape(label[range.clone()].to_string().into());
                labels.push((
                    line,
                    point(bounds.left(), label_top + line_height * ix as f32),
                ));
            }
            let group_top = top + (height - rows.slot * self.series.len() as f32) / 2.;
            for series in 0..self.series.len() {
                let slot_top = group_top + rows.slot * series as f32;
                let track = Bounds::new(
                    point(track_left, slot_top + (rows.slot - BAR_HEIGHT) / 2.),
                    size(track_right - track_left, BAR_HEIGHT),
                );
                let fill = self
                    .value(row, series)
                    .map(|value| track.size.width * (value.max(0.) / max).min(1.) as f32);
                if let Some(line) = values.next().flatten() {
                    labels.push((
                        line,
                        point(
                            track_right + COLUMN_GAP,
                            slot_top + (rows.slot - line_height) / 2.,
                        ),
                    ));
                }
                bars.push(Bar {
                    row,
                    series,
                    track,
                    fill,
                    slot: Bounds::new(
                        point(bounds.left(), slot_top),
                        size(bounds.size.width, rows.slot),
                    ),
                });
            }
        }

        BarChartLayout {
            bars,
            labels,
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
        let hovered = self.hover.as_ref().and_then(|(hovered, _)| *hovered);
        for bar in &layout.bars {
            window
                .paint_quad(fill(bar.track, theme::track()).corner_radii(Corners::all(BAR_RADIUS)));
            match bar.fill {
                Some(width) if width > px(0.) => window.paint_quad(
                    fill(
                        Bounds::new(bar.track.origin, size(width, bar.track.size.height)),
                        self.series[bar.series].color,
                    )
                    .corner_radii(Corners::all(BAR_RADIUS)),
                ),
                Some(_) => {}
                None => paint_hatch(bar.track, window),
            }
            if hovered == Some((bar.row, bar.series)) {
                window.paint_quad(
                    outline(bar.track, theme::text(), gpui::BorderStyle::Solid)
                        .corner_radii(Corners::all(BAR_RADIUS)),
                );
            }
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
            let slots: Vec<_> = layout
                .bars
                .iter()
                .map(|bar| (bar.slot, (bar.row, bar.series)))
                .collect();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let hovered = hitbox
                    .is_hovered(window)
                    .then(|| {
                        slots
                            .iter()
                            .find(|(slot, _)| slot.contains(&event.position))
                            .map(|(_, bar)| *bar)
                    })
                    .flatten();
                handler(hovered, window, cx);
            });
        }
    }
}

/// Diagonal stripes over the track of a missing value.
fn paint_hatch(track: Bounds<Pixels>, window: &mut Window) {
    let height = track.size.height;
    let mut path = PathBuilder::stroke(px(1.));
    let mut x = track.left() - height;
    while x < track.right() {
        path.move_to(point(x, track.bottom()));
        path.line_to(point(x + height, track.top()));
        x += HATCH_STEP;
    }
    if let Ok(path) = path.build() {
        window.with_content_mask(Some(ContentMask { bounds: track }), |window| {
            window.paint_path(path, theme::border());
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::rgb;

    use super::*;

    fn chart(values: Vec<Option<f64>>) -> BarChart {
        let rows = (0..values.len())
            .map(|ix| SharedString::from(format!("row {ix}")))
            .collect();
        BarChart::new(
            rows,
            vec![BarSeries::new("a", rgb(0x488be8).into(), values)],
        )
    }

    #[test]
    fn scales_to_the_largest_finite_value() {
        assert_eq!(
            chart(vec![Some(3.), None, Some(f64::NAN), Some(8.)]).scale_max(),
            8.
        );
        assert_eq!(chart(vec![None, Some(0.)]).scale_max(), 1.);
        assert_eq!(chart(vec![Some(3.)]).max(10.).scale_max(), 10.);
    }

    #[test]
    fn treats_non_finite_and_absent_values_as_missing() {
        let chart = chart(vec![Some(f64::INFINITY), Some(2.)]);
        assert_eq!(chart.value(0, 0), None);
        assert_eq!(chart.value(1, 0), Some(2.));
        assert_eq!(chart.value(5, 0), None);
    }
}
