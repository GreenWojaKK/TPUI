//! 선·띠·기준선을 겹쳐 그리는 차트. GPUI의 `paint_quad`와 `PathBuilder`로 직접 그린다.
//!
//! 축의 값 범위와 눈금, 그릴 층(띠·선·기준선)을 받아 그린다. 사각형으로 끝나는 틀·눈금선·점은
//! `paint_quad`로, 띠는 `PathBuilder::fill`로, 선은 `PathBuilder::stroke`로 그린다. 값이 없는
//! 점에서는 띠와 선을 끊는다. 마우스를 올리면 가장 가까운 점의 위치를 호출한 쪽에 알린다.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Corners, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseMoveEvent, PathBuilder, Pixels, Point, SharedString, ShapedLine, Style, TextAlign,
    TextRun, Window, fill, outline, point, px, relative, size,
};

use crate::ui::theme;

/// 눈금 하나와 그 표시.
pub type Tick = (f64, SharedString);

/// 축 하나의 값 범위, 눈금, 이름.
#[derive(Clone, Debug)]
pub struct Axis {
    min: f64,
    max: f64,
    ticks: Vec<Tick>,
    title: Option<SharedString>,
}

impl Axis {
    /// `[min, max]` 범위의 축. 범위가 비면 1만큼 넓힌다.
    pub fn new(min: f64, max: f64) -> Self {
        let (min, max) = if max > min { (min, max) } else { (min, min + 1.) };
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

    /// 범위 안에 1·2·2.5·5×10ⁿ 간격의 눈금을 `count`개 안팎으로 둔다.
    pub fn nice_ticks(mut self, count: usize, format: impl Fn(f64) -> String) -> Self {
        self.ticks = nice_ticks(self.min, self.max, count)
            .into_iter()
            .map(|value| (value, format(value).into()))
            .collect();
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 값을 `[start, start + len]` 구간의 위치로 바꾼다. 범위 밖의 값도 같은 비율로 바꾼다.
    fn position(&self, value: f64, start: Pixels, len: Pixels) -> Pixels {
        let ratio = (value - self.min) / (self.max - self.min);
        start + len * ratio as f32
    }
}

/// 아래·위 경계 사이를 채운 띠. 예측구간 같은 범위를 그린다.
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

/// 점을 이은 선. 점선과 점 표시를 고를 수 있다.
#[derive(Clone, Debug)]
pub struct Line {
    pub x: Vec<f64>,
    pub y: Vec<Option<f64>>,
    pub color: Hsla,
    pub width: Pixels,
    /// 점선의 그린 길이와 빈 길이.
    pub dash: Option<(Pixels, Pixels)>,
    /// 점마다 원을 그린다.
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

/// 값 좌표의 두 점을 잇는 기준선. 명목값이나 대각선 같은 비교선을 그린다.
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

/// 차트에 겹쳐 그리는 층. 넣은 순서대로 그린다.
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

/// 마우스 위치를 가장 가까운 점의 순번으로 알린다. 그래프 밖이면 `None`이다.
pub type HoverHandler = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;

struct Hover {
    /// 마우스를 맞출 x 값.
    positions: Vec<f64>,
    /// 세로 안내선을 그릴 순번.
    hovered: Option<usize>,
    handler: HoverHandler,
}

/// 선·띠·기준선 차트. 부모가 정한 크기를 모두 쓴다.
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

    /// 마우스를 `positions` 중 가장 가까운 x 값에 맞춰 알린다. `hovered`가 있으면 그 위치에
    /// 세로 안내선을 그린다.
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

/// prepaint에서 정한 배치. 눈금 글자는 미리 모양을 잡아 둔다.
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
        // 가로 눈금선은 층보다 먼저 그린다.
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
        // 화면의 y는 아래로 커지므로 위에서부터 뒤집는다.
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
        // 이웃이 없어 선이 되지 않는 점과 점 표시는 원으로 그린다.
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

/// 창의 글자 모양으로 한 줄 텍스트를 만든다.
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

    pub(crate) fn shape(&self, text: SharedString) -> ShapedLine {
        let run = TextRun {
            len: text.len(),
            font: self.font.clone(),
            color: self.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        self.text_system
            .shape_line(text, self.font_size, &[run], None)
    }
}

/// 조건을 만족하는 연속 구간들.
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

/// `value`에 가장 가까운 위치의 순번.
pub(crate) fn nearest(positions: &[f64], value: f64) -> Option<usize> {
    positions
        .iter()
        .enumerate()
        .filter(|(_, position)| position.is_finite())
        .min_by(|(_, a), (_, b)| (*a - value).abs().total_cmp(&(*b - value).abs()))
        .map(|(ix, _)| ix)
}

/// `[min, max]` 안에서 1·2·2.5·5×10ⁿ 간격으로 `count`개 안팎의 눈금 값.
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
            // 부동소수 오차로 0.30000000000000004 같은 값이 나오지 않도록 자릿수를 맞춘다.
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
        assert_eq!(runs(values.len(), |ix| values[ix].is_some()), vec![0..1, 2..4]);
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
    fn maps_values_onto_the_plot() {
        let axis = Axis::new(0., 10.);
        assert_eq!(axis.position(5., px(100.), px(200.)), px(200.));
        let empty = Axis::new(3., 3.);
        assert_eq!(empty.position(3., px(0.), px(10.)), px(0.));
    }
}
