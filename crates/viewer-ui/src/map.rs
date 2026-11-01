//! Map that draws areas, routes, markers and labels at geographic coordinates. Drawn directly with GPUI's
//! `paint_quad` and `PathBuilder`, without map tiles.
//!
//! Coordinates are `[latitude, longitude]` in degrees. The map uses an equirectangular projection around the
//! center latitude of the fitted region, which keeps shapes true at city scale. The view fits the routes,
//! markers and labels, keeps the aspect ratio and centers them; areas are background and do not affect the fit.
//!
//! Areas can hold many vertices, so their geometry is prepared once per view and kept across frames. Areas
//! outside the view are skipped, rings are clipped to the view, vertices are snapped to the device pixel grid
//! with repeated points removed, and the tessellated paths are reused until the area set, the fitted region,
//! the element size or the border width changes. Snapping every area to the same grid keeps the shared borders
//! of neighbouring areas coincident.

use std::sync::Arc;

use gpui::{
    App, Bounds, ContentMask, Corners, Element, ElementId, FillOptions, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Path, PathBuilder, PathStyle,
    Pixels, Point, ShapedLine, SharedString, StrokeOptions, Style, TextAlign, TextRun, Window,
    fill, point, px, relative, size,
};

/// `[latitude, longitude]` in degrees.
pub type LatLon = [f64; 2];

/// Smallest extent of the fitted region, in degrees, so a single point does not zoom in without bound.
const MIN_EXTENT: f64 = 0.002;

/// Polygon area such as an administrative district. Each ring is closed implicitly, and rings are filled with
/// the even-odd rule, so holes need no particular winding.
#[derive(Clone, Debug, Default)]
pub struct Area {
    pub rings: Vec<Vec<LatLon>>,
}

/// Line through points, such as a bus route.
#[derive(Clone, Debug)]
pub struct Route {
    pub points: Vec<LatLon>,
    pub color: Hsla,
    pub width: Pixels,
    /// Drawn and gap lengths of the dash pattern.
    pub dash: Option<(Pixels, Pixels)>,
}

impl Route {
    pub fn new(points: Vec<LatLon>, color: impl Into<Hsla>) -> Self {
        Self {
            points,
            color: color.into(),
            width: px(3.),
            dash: None,
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
}

/// Circle at a point, with an optional border.
#[derive(Clone, Debug)]
pub struct Marker {
    pub point: LatLon,
    pub radius: Pixels,
    pub fill: Hsla,
    pub border: Option<(Pixels, Hsla)>,
}

impl Marker {
    pub fn new(point: LatLon, radius: Pixels, fill: impl Into<Hsla>) -> Self {
        Self {
            point,
            radius,
            fill: fill.into(),
            border: None,
        }
    }

    pub fn border(mut self, width: Pixels, color: impl Into<Hsla>) -> Self {
        self.border = Some((width, color.into()));
        self
    }
}

/// Horizontal placement of a label relative to its anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LabelAlign {
    /// The text starts at the anchor.
    #[default]
    Start,
    /// The text is centered on the anchor.
    Center,
    /// The text ends at the anchor.
    End,
}

/// One line of text at a point. The anchor is the point moved by `offset`; the text is vertically centered on it.
#[derive(Clone, Debug)]
pub struct Label {
    pub point: LatLon,
    pub text: SharedString,
    pub color: Hsla,
    pub align: LabelAlign,
    pub offset: Point<Pixels>,
    /// Outline color drawn around the glyphs so the text stays readable over routes and areas.
    pub halo: Option<Hsla>,
    pub weight: FontWeight,
    /// Font size. The window's text size if `None`.
    pub size: Option<Pixels>,
}

impl Label {
    pub fn new(point: LatLon, text: impl Into<SharedString>, color: impl Into<Hsla>) -> Self {
        Self {
            point,
            text: text.into(),
            color: color.into(),
            align: LabelAlign::Start,
            offset: Point::default(),
            halo: None,
            weight: FontWeight::NORMAL,
            size: None,
        }
    }

    pub fn align(mut self, align: LabelAlign) -> Self {
        self.align = align;
        self
    }

    pub fn offset(mut self, x: Pixels, y: Pixels) -> Self {
        self.offset = point(x, y);
        self
    }

    pub fn halo(mut self, color: impl Into<Hsla>) -> Self {
        self.halo = Some(color.into());
        self
    }

    pub fn bold(mut self) -> Self {
        self.weight = FontWeight::BOLD;
        self
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }
}

struct AreaStyle {
    areas: Arc<[Area]>,
    fill: Hsla,
    border: Option<(Pixels, Hsla)>,
}

/// Map element. Fills the size given by its parent. The id keeps prepared area geometry across frames, so it
/// must be unique among its siblings.
pub struct GeoMap {
    id: ElementId,
    background: Option<Hsla>,
    areas: Option<AreaStyle>,
    routes: Vec<Route>,
    markers: Vec<Marker>,
    labels: Vec<Label>,
    padding: f64,
}

impl GeoMap {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            background: None,
            areas: None,
            routes: Vec::new(),
            markers: Vec::new(),
            labels: Vec::new(),
            padding: 0.1,
        }
    }

    /// Fills the whole element before anything else.
    pub fn background(mut self, color: impl Into<Hsla>) -> Self {
        self.background = Some(color.into());
        self
    }

    /// Background areas. Keep the same `Arc` across renders so the prepared geometry is reused.
    pub fn areas(mut self, areas: Arc<[Area]>, fill: impl Into<Hsla>) -> Self {
        self.areas = Some(AreaStyle {
            areas,
            fill: fill.into(),
            border: None,
        });
        self
    }

    /// Border drawn around each area. Requires [`Self::areas`] first.
    pub fn area_border(mut self, width: Pixels, color: impl Into<Hsla>) -> Self {
        if let Some(areas) = &mut self.areas {
            areas.border = Some((width, color.into()));
        }
        self
    }

    /// Routes are drawn in insertion order, after the areas.
    pub fn route(mut self, route: Route) -> Self {
        self.routes.push(route);
        self
    }

    /// Markers are drawn after all routes, in insertion order.
    pub fn marker(mut self, marker: Marker) -> Self {
        self.markers.push(marker);
        self
    }

    /// Labels are drawn last, in insertion order.
    pub fn label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    /// Margin around the fitted region, as a fraction of its larger side.
    pub fn padding(mut self, fraction: f64) -> Self {
        self.padding = fraction.max(0.);
        self
    }

    fn fit_points(&self) -> Vec<LatLon> {
        let points: Vec<LatLon> = self
            .routes
            .iter()
            .flat_map(|route| route.points.iter().copied())
            .chain(self.markers.iter().map(|marker| marker.point))
            .chain(self.labels.iter().map(|label| label.point))
            .collect();
        if !points.is_empty() {
            return points;
        }
        self.areas
            .iter()
            .flat_map(|style| style.areas.iter())
            .flat_map(|area| area.rings.iter().flatten().copied())
            .collect()
    }
}

/// Projection from geographic coordinates to pixels relative to the element's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    cos_lat: f64,
    center_x: f64,
    center_y: f64,
    /// Pixels per projected unit.
    scale: f64,
    width: f64,
    height: f64,
}

impl View {
    /// Fits the points into `width × height` with `padding` around them, keeping the aspect ratio.
    fn fit(points: &[LatLon], padding: f64, width: f64, height: f64) -> Option<Self> {
        let valid = points
            .iter()
            .filter(|[lat, lon]| lat.is_finite() && lon.is_finite());
        let (lat_min, lat_max) = valid
            .clone()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), [lat, _]| {
                (lo.min(*lat), hi.max(*lat))
            });
        if !lat_min.is_finite() || width <= 0. || height <= 0. {
            return None;
        }
        let cos_lat = ((lat_min + lat_max) / 2.).to_radians().cos();
        let (x_min, x_max) = valid
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), [_, lon]| {
                (lo.min(lon * cos_lat), hi.max(lon * cos_lat))
            });
        let extent = (x_max - x_min).max(lat_max - lat_min).max(MIN_EXTENT);
        let pad = extent * padding;
        let view_width = (x_max - x_min).max(MIN_EXTENT) + pad * 2.;
        let view_height = (lat_max - lat_min).max(MIN_EXTENT) + pad * 2.;
        Some(Self {
            cos_lat,
            center_x: (x_min + x_max) / 2.,
            center_y: (lat_min + lat_max) / 2.,
            scale: (width / view_width).min(height / view_height),
            width,
            height,
        })
    }

    /// Local pixel position of a geographic point.
    fn project(&self, [lat, lon]: LatLon) -> (f64, f64) {
        (
            self.width / 2. + (lon * self.cos_lat - self.center_x) * self.scale,
            self.height / 2. - (lat - self.center_y) * self.scale,
        )
    }

    fn point(&self, origin: Point<Pixels>, location: LatLon) -> Point<Pixels> {
        let (x, y) = self.project(location);
        point(origin.x + px(x as f32), origin.y + px(y as f32))
    }
}

/// Axis-aligned rectangle in local pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    min: (f64, f64),
    max: (f64, f64),
}

impl Rect {
    fn intersects(&self, other: &Rect) -> bool {
        self.min.0 <= other.max.0
            && other.min.0 <= self.max.0
            && self.min.1 <= other.max.1
            && other.min.1 <= self.max.1
    }
}

/// Clips a closed ring to a rectangle (Sutherland–Hodgman). Exact for filling; edges created along the
/// rectangle lie on its sides.
fn clip_ring(ring: &[(f64, f64)], clip: &Rect) -> Vec<(f64, f64)> {
    let mut output = ring.to_vec();
    for side in 0..4 {
        if output.is_empty() {
            break;
        }
        let input = std::mem::take(&mut output);
        let inside = |(x, y): (f64, f64)| match side {
            0 => x >= clip.min.0,
            1 => x <= clip.max.0,
            2 => y >= clip.min.1,
            _ => y <= clip.max.1,
        };
        let cross = |(ax, ay): (f64, f64), (bx, by): (f64, f64)| {
            let t = match side {
                0 => (clip.min.0 - ax) / (bx - ax),
                1 => (clip.max.0 - ax) / (bx - ax),
                2 => (clip.min.1 - ay) / (by - ay),
                _ => (clip.max.1 - ay) / (by - ay),
            };
            (ax + t * (bx - ax), ay + t * (by - ay))
        };
        let mut previous = *input.last().expect("non-empty ring");
        for &current in &input {
            match (inside(previous), inside(current)) {
                (true, true) => output.push(current),
                (true, false) => output.push(cross(previous, current)),
                (false, true) => {
                    output.push(cross(previous, current));
                    output.push(current);
                }
                (false, false) => {}
            }
            previous = current;
        }
    }
    output
}

/// Snaps a ring to a grid of `1 / grid` pixels and removes repeated points. Rings that collapse below three
/// points are dropped.
fn snap_ring(ring: &[(f64, f64)], grid: f64) -> Vec<(f64, f64)> {
    let mut output: Vec<(f64, f64)> = Vec::with_capacity(ring.len());
    for &(x, y) in ring {
        let snapped = ((x * grid).round() / grid, (y * grid).round() / grid);
        if output.last() != Some(&snapped) {
            output.push(snapped);
        }
    }
    while output.len() > 1 && output.first() == output.last() {
        output.pop();
    }
    if output.len() < 3 {
        output.clear();
    }
    output
}

/// Rings of one area prepared for drawing: projected, culled, clipped and snapped. Empty if the area is
/// outside `clip`.
fn prepare_area(area: &Area, view: &View, clip: &Rect, grid: f64) -> Vec<Vec<(f64, f64)>> {
    let mut rings = Vec::new();
    for ring in &area.rings {
        let projected: Vec<(f64, f64)> = ring.iter().map(|p| view.project(*p)).collect();
        let Some(bounds) = bounding_rect(&projected) else {
            continue;
        };
        if !bounds.intersects(clip) {
            continue;
        }
        let snapped = snap_ring(&clip_ring(&projected, clip), grid);
        if !snapped.is_empty() {
            rings.push(snapped);
        }
    }
    rings
}

fn bounding_rect(points: &[(f64, f64)]) -> Option<Rect> {
    let first = *points.first()?;
    Some(points.iter().fold(
        Rect {
            min: first,
            max: first,
        },
        |rect, &(x, y)| Rect {
            min: (rect.min.0.min(x), rect.min.1.min(y)),
            max: (rect.max.0.max(x), rect.max.1.max(y)),
        },
    ))
}

fn to_point((x, y): (f64, f64)) -> Point<Pixels> {
    point(px(x as f32), px(y as f32))
}

fn ring_path(mut builder: PathBuilder, rings: &[Vec<(f64, f64)>]) -> Option<Path<Pixels>> {
    for ring in rings {
        builder.move_to(to_point(ring[0]));
        for &vertex in &ring[1..] {
            builder.line_to(to_point(vertex));
        }
        builder.close();
    }
    builder.build().ok()
}

/// Stroke builder with bevel joins, so sharp turns of dense polylines do not grow miter spikes.
fn stroke(width: Pixels, dash: Option<(Pixels, Pixels)>) -> PathBuilder {
    let options = StrokeOptions::default()
        .with_line_width(width.as_f32())
        .with_miter_limit(StrokeOptions::MINIMUM_MITER_LIMIT);
    let builder = PathBuilder::stroke(width).with_style(PathStyle::Stroke(options));
    match dash {
        Some((on, off)) => builder.dash_array(&[on, off]),
        None => builder,
    }
}

/// Inputs that decide the prepared area geometry. Colors are applied at paint time and are not part of it.
struct AreaKey {
    areas: Arc<[Area]>,
    view: View,
    grid: f64,
    border: Option<Pixels>,
}

impl AreaKey {
    fn matches(&self, other: &AreaKey) -> bool {
        Arc::ptr_eq(&self.areas, &other.areas)
            && self.view == other.view
            && self.grid == other.grid
            && self.border == other.border
    }
}

/// Tessellated area paths in local pixels, kept in element state across frames.
struct AreaCache {
    key: AreaKey,
    fills: Vec<Path<Pixels>>,
    borders: Vec<Path<Pixels>>,
}

impl AreaCache {
    fn build(key: AreaKey) -> Self {
        // Clip slightly outside the element so edges created by clipping stay out of sight.
        let margin = key.border.map_or(0., f64::from) + 2.;
        let clip = Rect {
            min: (-margin, -margin),
            max: (key.view.width + margin, key.view.height + margin),
        };
        let mut fills = Vec::new();
        let mut borders = Vec::new();
        for area in key.areas.iter() {
            let rings = prepare_area(area, &key.view, &clip, key.grid);
            if rings.is_empty() {
                continue;
            }
            let even_odd = PathStyle::Fill(FillOptions::even_odd());
            if let Some(path) = ring_path(PathBuilder::fill().with_style(even_odd), &rings) {
                fills.push(path);
            }
            if let Some(width) = key.border
                && let Some(path) = ring_path(stroke(width, None), &rings)
            {
                borders.push(path);
            }
        }
        Self {
            key,
            fills,
            borders,
        }
    }
}

/// A prepared path moved from local pixels to window coordinates.
fn translated(path: &Path<Pixels>, offset: Point<Pixels>) -> Path<Pixels> {
    let mut path = path.clone();
    path.bounds.origin += offset;
    for vertex in &mut path.vertices {
        vertex.xy_position += offset;
    }
    path
}

struct ShapedLabel {
    text: ShapedLine,
    halo: Option<ShapedLine>,
    origin: Point<Pixels>,
    line_height: Pixels,
}

/// Layout decided in prepaint: the projection, area paths in window coordinates and shaped labels.
pub struct MapLayout {
    view: Option<View>,
    area_fills: Vec<Path<Pixels>>,
    area_borders: Vec<Path<Pixels>>,
    labels: Vec<ShapedLabel>,
}

impl IntoElement for GeoMap {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for GeoMap {
    type RequestLayoutState = ();
    type PrepaintState = MapLayout;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let view = View::fit(
            &self.fit_points(),
            self.padding,
            f64::from(bounds.size.width),
            f64::from(bounds.size.height),
        );
        let mut area_fills = Vec::new();
        let mut area_borders = Vec::new();
        if let (Some(view), Some(style), Some(id)) = (view, &self.areas, id) {
            let key = AreaKey {
                areas: style.areas.clone(),
                view,
                grid: f64::from(window.scale_factor()),
                border: style.border.map(|(width, _)| width),
            };
            window.with_element_state::<AreaCache, _>(id, |cache, _| {
                let cache = match cache {
                    Some(cache) if cache.key.matches(&key) => cache,
                    _ => AreaCache::build(key),
                };
                area_fills = cache
                    .fills
                    .iter()
                    .map(|path| translated(path, bounds.origin))
                    .collect();
                area_borders = cache
                    .borders
                    .iter()
                    .map(|path| translated(path, bounds.origin))
                    .collect();
                ((), cache)
            });
        }
        let labels = match view {
            Some(view) => self
                .labels
                .iter()
                .map(|label| shape_label(label, &view, bounds.origin, window))
                .collect(),
            None => Vec::new(),
        };
        MapLayout {
            view,
            area_fills,
            area_borders,
            labels,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(color) = self.background {
            window.paint_quad(fill(bounds, color));
        }
        let Some(view) = layout.view else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(style) = &self.areas {
                for path in layout.area_fills.drain(..) {
                    window.paint_path(path, style.fill);
                }
                if let Some((_, color)) = style.border {
                    for path in layout.area_borders.drain(..) {
                        window.paint_path(path, color);
                    }
                }
            }
            for route in &self.routes {
                if route.points.len() < 2 {
                    continue;
                }
                let mut path = stroke(route.width, route.dash);
                path.move_to(view.point(bounds.origin, route.points[0]));
                for location in &route.points[1..] {
                    path.line_to(view.point(bounds.origin, *location));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, route.color);
                }
            }
            for marker in &self.markers {
                let center = view.point(bounds.origin, marker.point);
                let radius = marker.radius;
                let mut quad = fill(
                    Bounds::new(
                        point(center.x - radius, center.y - radius),
                        size(radius * 2., radius * 2.),
                    ),
                    marker.fill,
                )
                .corner_radii(Corners::all(radius));
                if let Some((width, color)) = marker.border {
                    quad = quad.border_widths(width).border_color(color);
                }
                window.paint_quad(quad);
            }
            for label in &layout.labels {
                if let Some(halo) = &label.halo {
                    for (dx, dy) in HALO_OFFSETS {
                        let origin = point(label.origin.x + px(dx), label.origin.y + px(dy));
                        let _ = halo.paint(
                            origin,
                            label.line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }
                let _ = label.text.paint(
                    label.origin,
                    label.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        });
    }
}

/// Offsets at which the halo copy of a label is drawn under the text.
const HALO_OFFSETS: [(f32, f32); 8] = [
    (-1., -1.),
    (0., -1.),
    (1., -1.),
    (-1., 0.),
    (1., 0.),
    (-1., 1.),
    (0., 1.),
    (1., 1.),
];

fn shape_label(label: &Label, view: &View, origin: Point<Pixels>, window: &Window) -> ShapedLabel {
    let style = window.text_style();
    let mut font = style.font();
    font.weight = label.weight;
    let font_size = label
        .size
        .unwrap_or_else(|| style.font_size.to_pixels(window.rem_size()));
    let line_height = font_size * 1.25;
    let shape = |color: Hsla| {
        let run = TextRun {
            len: label.text.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window
            .text_system()
            .shape_line(label.text.clone(), font_size, &[run], None)
    };
    let text = shape(label.color);
    let halo = label.halo.map(shape);
    let anchor = view.point(origin, label.point) + label.offset;
    let x = match label.align {
        LabelAlign::Start => anchor.x,
        LabelAlign::Center => anchor.x - text.width() / 2.,
        LabelAlign::End => anchor.x - text.width(),
    };
    ShapedLabel {
        text,
        halo,
        origin: point(x, anchor.y - line_height / 2.),
        line_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(min: (f64, f64), max: (f64, f64)) -> Rect {
        Rect { min, max }
    }

    #[test]
    fn fits_points_with_equal_scale_and_centers_them() {
        // 0.01° of longitude at the equator against 0.005° of latitude, in a square.
        let view = View::fit(&[[0., 0.], [0.005, 0.01]], 0., 200., 200.).unwrap();
        let (x0, y0) = view.project([0., 0.]);
        let (x1, y1) = view.project([0.005, 0.01]);
        assert!(
            (x1 - x0 - 200.).abs() < 1e-6,
            "the wider side fills the width"
        );
        assert!(
            (y0 - y1 - 100.).abs() < 1e-3,
            "the same scale applies to latitude"
        );
        assert!(((y0 + y1) / 2. - 100.).abs() < 1e-6, "centered vertically");
        // Longitude shrinks with latitude.
        let north = View::fit(&[[60., 0.], [60., 0.01]], 0., 200., 200.).unwrap();
        assert!((north.cos_lat - 0.5).abs() < 1e-9);
        assert!(View::fit(&[], 0.1, 200., 200.).is_none());
    }

    #[test]
    fn clips_rings_to_the_rectangle() {
        let square = [(-5., -5.), (5., -5.), (5., 5.), (-5., 5.)];
        let clipped = clip_ring(&square, &rect((0., 0.), (10., 10.)));
        let bounds = bounding_rect(&clipped).unwrap();
        assert_eq!(bounds, rect((0., 0.), (5., 5.)));
        assert!(clip_ring(&square, &rect((20., 20.), (30., 30.))).is_empty());
        // A ring around the whole rectangle becomes the rectangle.
        let around = clip_ring(&square, &rect((-1., -1.), (1., 1.)));
        assert_eq!(bounding_rect(&around).unwrap(), rect((-1., -1.), (1., 1.)));
    }

    #[test]
    fn snaps_to_the_device_grid_and_drops_collapsed_rings() {
        let ring = [(0.1, 0.1), (0.2, 0.2), (3.4, 0.), (3.6, 2.9), (0., 3.)];
        // On a 1x display, the first two points collapse into one.
        assert_eq!(
            snap_ring(&ring, 1.),
            vec![(0., 0.), (3., 0.), (4., 3.), (0., 3.)]
        );
        // On a 2x display, half pixels survive.
        assert_eq!(
            snap_ring(&[(0.26, 0.), (1., 0.), (1., 1.)], 2.)[0],
            (0.5, 0.)
        );
        assert!(snap_ring(&[(0., 0.), (0.2, 0.1), (0.1, 0.3)], 1.).is_empty());
        // The closing point equal to the first is removed.
        assert_eq!(
            snap_ring(&[(0., 0.), (5., 0.), (5., 5.), (0., 0.)], 1.).len(),
            3
        );
    }

    #[test]
    fn reuses_prepared_geometry_only_for_the_same_inputs() {
        let areas: Arc<[Area]> = vec![Area::default()].into();
        let view = View::fit(&[[35.5, 129.3], [35.6, 129.4]], 0.1, 400., 300.).unwrap();
        let key = |areas: &Arc<[Area]>, view: View| AreaKey {
            areas: areas.clone(),
            view,
            grid: 1.5,
            border: Some(px(1.)),
        };
        // The same routes in the same size give the same view on every frame.
        let again = View::fit(&[[35.5, 129.3], [35.6, 129.4]], 0.1, 400., 300.).unwrap();
        assert!(key(&areas, view).matches(&key(&areas, again)));
        let resized = View::fit(&[[35.5, 129.3], [35.6, 129.4]], 0.1, 401., 300.).unwrap();
        assert!(!key(&areas, view).matches(&key(&areas, resized)));
        let other: Arc<[Area]> = vec![Area::default()].into();
        assert!(!key(&areas, view).matches(&key(&other, view)));
    }

    #[test]
    fn skips_areas_outside_the_view() {
        let view = View::fit(&[[35.5, 129.3], [35.6, 129.4]], 0., 400., 400.).unwrap();
        let clip = rect((0., 0.), (400., 400.));
        let inside = Area {
            rings: vec![vec![[35.52, 129.32], [35.52, 129.36], [35.56, 129.36]]],
        };
        let outside = Area {
            rings: vec![vec![[36.0, 130.0], [36.0, 130.1], [36.1, 130.1]]],
        };
        assert_eq!(prepare_area(&inside, &view, &clip, 1.).len(), 1);
        assert!(prepare_area(&outside, &view, &clip, 1.).is_empty());
    }
}
