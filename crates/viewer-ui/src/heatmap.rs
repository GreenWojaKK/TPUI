//! Heatmap that colors each row × column cell by value. Cells are drawn with `paint_quad`.
//!
//! The diverging color scale puts 0 at the middle color and deepens negatives and positives toward two different colors.
//! Cells without a value are left unpainted. A color scale bar with its bounds is drawn on the right.

use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseMoveEvent, Pixels, Point, Rgba,
    ShapedLine, SharedString, Style, TextAlign, Window, fill, outline, point, px, relative, size,
};

use crate::chart::TextShaper;
use crate::theme;

/// Diverging color scale centered at 0. Magnitudes at or above `bound` use the end colors.
#[derive(Clone, Copy, Debug)]
pub struct DivergingScale {
    pub bound: f64,
    pub negative: Rgba,
    pub middle: Rgba,
    pub positive: Rgba,
}

impl DivergingScale {
    pub fn color(&self, value: f64) -> Rgba {
        let t = if self.bound > 0. {
            (value / self.bound).clamp(-1., 1.)
        } else {
            0.
        } as f32;
        let end = if t < 0. { self.negative } else { self.positive };
        lerp(self.middle, end, t.abs())
    }
}

fn lerp(from: Rgba, to: Rgba, t: f32) -> Rgba {
    Rgba {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

/// Reports the hovered cell (row, column). `None` outside the cells.
pub type CellHoverHandler = Rc<dyn Fn(Option<(usize, usize)>, &mut Window, &mut App)>;

pub struct Heatmap {
    rows: Vec<SharedString>,
    columns: Vec<SharedString>,
    /// `values[row][column]`.
    values: Vec<Vec<Option<f64>>>,
    /// Columns whose labels are shown. Empty shows every column.
    column_ticks: Vec<usize>,
    scale: DivergingScale,
    format: Rc<dyn Fn(f64) -> String>,
    hover: Option<(Option<(usize, usize)>, CellHoverHandler)>,
}

impl Heatmap {
    pub fn new(
        rows: Vec<SharedString>,
        columns: Vec<SharedString>,
        values: Vec<Vec<Option<f64>>>,
        scale: DivergingScale,
    ) -> Self {
        Self {
            rows,
            columns,
            values,
            column_ticks: Vec::new(),
            scale,
            format: Rc::new(|value| format!("{value:.2}")),
            hover: None,
        }
    }

    pub fn column_ticks(mut self, ticks: Vec<usize>) -> Self {
        self.column_ticks = ticks;
        self
    }

    /// Label format of the color scale bounds.
    pub fn format(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.format = Rc::new(format);
        self
    }

    /// Reports the hovered cell and draws a border around the `hovered` cell.
    pub fn on_hover(
        mut self,
        hovered: Option<(usize, usize)>,
        handler: impl Fn(Option<(usize, usize)>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover = Some((hovered, Rc::new(handler)));
        self
    }
}

pub struct HeatmapLayout {
    grid: Bounds<Pixels>,
    legend: Bounds<Pixels>,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    hitbox: Hitbox,
    line_height: Pixels,
}

impl IntoElement for Heatmap {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Width and step count of the color scale bar.
const LEGEND_WIDTH: Pixels = px(12.);
const LEGEND_STEPS: usize = 48;

impl Element for Heatmap {
    type RequestLayoutState = ();
    type PrepaintState = HeatmapLayout;

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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
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
        let row_labels: Vec<_> = self
            .rows
            .iter()
            .map(|row| text.shape(row.clone()))
            .collect();
        let bound = self.scale.bound;
        let legend_labels: Vec<_> = [bound, 0., -bound]
            .into_iter()
            .map(|value| text.shape((self.format)(value).into()))
            .collect();
        let left = row_labels
            .iter()
            .map(ShapedLine::width)
            .fold(px(0.), Pixels::max)
            + gap * 2.;
        let legend_text = legend_labels
            .iter()
            .map(ShapedLine::width)
            .fold(px(0.), Pixels::max);
        let right = gap * 3. + LEGEND_WIDTH + legend_text;
        let bottom = text.line_height + gap;
        let grid = Bounds::from_corners(
            point(bounds.left() + left, bounds.top() + gap),
            point(
                (bounds.right() - right).max(bounds.left() + left + px(1.)),
                (bounds.bottom() - bottom).max(bounds.top() + gap + px(1.)),
            ),
        );
        let legend = Bounds::new(
            point(grid.right() + gap * 2., grid.top()),
            size(LEGEND_WIDTH, grid.size.height),
        );

        let row_height = grid.size.height / self.rows.len().max(1) as f32;
        let column_width = grid.size.width / self.columns.len().max(1) as f32;
        let mut labels = Vec::new();
        for (ix, line) in row_labels.into_iter().enumerate() {
            let y = grid.top() + row_height * (ix as f32 + 0.5) - text.line_height / 2.;
            labels.push((line.clone(), point(grid.left() - gap - line.width(), y)));
        }
        let ticks: Vec<usize> = if self.column_ticks.is_empty() {
            (0..self.columns.len()).collect()
        } else {
            self.column_ticks.clone()
        };
        for ix in ticks.into_iter().filter(|ix| *ix < self.columns.len()) {
            let line = text.shape(self.columns[ix].clone());
            let x = grid.left() + column_width * (ix as f32 + 0.5) - line.width() / 2.;
            labels.push((line, point(x, grid.bottom() + gap / 2.)));
        }
        for (ix, line) in legend_labels.into_iter().enumerate() {
            let y = legend.top() + legend.size.height * (ix as f32 / 2.) - text.line_height / 2.;
            labels.push((line, point(legend.right() + gap, y)));
        }

        HeatmapLayout {
            grid,
            legend,
            labels,
            hitbox: window.insert_hitbox(grid, HitboxBehavior::Normal),
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
        let grid = layout.grid;
        let rows = self.rows.len().max(1);
        let columns = self.columns.len().max(1);
        let cell = |row: usize, column: usize| {
            let width = grid.size.width / columns as f32;
            let height = grid.size.height / rows as f32;
            Bounds::new(
                point(
                    grid.left() + width * column as f32,
                    grid.top() + height * row as f32,
                ),
                size(width, height),
            )
        };
        for (row, values) in self.values.iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                if let Some(value) = value.filter(|value| value.is_finite()) {
                    window.paint_quad(fill(cell(row, column), self.scale.color(value)));
                }
            }
        }
        window.paint_quad(outline(grid, theme::border(), BorderStyle::Solid));
        if let Some((Some((row, column)), _)) = &self.hover {
            window.paint_quad(outline(
                cell(*row, *column),
                theme::text(),
                BorderStyle::Solid,
            ));
        }

        // Color scale bar with +bound at the top and -bound at the bottom.
        let legend = layout.legend;
        let step = legend.size.height / LEGEND_STEPS as f32;
        for ix in 0..LEGEND_STEPS {
            let t = 1. - 2. * (ix as f64 + 0.5) / LEGEND_STEPS as f64;
            window.paint_quad(fill(
                Bounds::new(
                    point(legend.left(), legend.top() + step * ix as f32),
                    size(legend.size.width, step + px(0.5)),
                ),
                self.scale.color(t * self.scale.bound),
            ));
        }
        window.paint_quad(outline(legend, theme::border(), BorderStyle::Solid));

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
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let hovered = hitbox.is_hovered(window).then(|| {
                    let x = f64::from((event.position.x - grid.left()) / grid.size.width);
                    let y = f64::from((event.position.y - grid.top()) / grid.size.height);
                    (
                        ((y * rows as f64) as usize).min(rows - 1),
                        ((x * columns as f64) as usize).min(columns - 1),
                    )
                });
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
    fn blends_from_the_middle_toward_each_end() {
        let scale = DivergingScale {
            bound: 2.,
            negative: rgb(0x0000ff),
            middle: rgb(0xffffff),
            positive: rgb(0xff0000),
        };
        assert_eq!(scale.color(0.), rgb(0xffffff));
        assert_eq!(scale.color(-2.), rgb(0x0000ff));
        assert_eq!(scale.color(5.), rgb(0xff0000));
        let half = scale.color(1.);
        assert!((half.g - 0.5).abs() < 1e-6 && half.r == 1.);
    }
}
