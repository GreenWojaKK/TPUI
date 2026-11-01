//! Example that draws the route map blocks of analysis runs with the map module.
//!
//! ```text
//! cargo run -p viewer-ui --example route_map -- <runs-directory> <map-context.json>
//! ```
//!
//! Each run file holds `candidates`; those of kind `map` carry route `lines` (`[latitude, longitude]` points),
//! `points` with an optional `kind` (`terminus`, `mention`, `waypoint`) and a `legend`. The map context is a
//! list of areas whose `polygons` are rings of `[longitude, latitude]` points, drawn as the background.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Bounds, ClickEvent, Context, Hsla, IntoElement, Render, SharedString, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use serde_json::Value;
use viewer_ui::map::{Area, GeoMap, Label, LabelAlign, LatLon, Marker, Route};
use viewer_ui::theme;
use viewer_ui::widgets::small_button;

struct MapBlock {
    case: String,
    title: String,
    payload: Value,
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
}

fn load_blocks(runs: &Path) -> Result<Vec<MapBlock>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(runs)
        .map_err(|err| format!("{}: {err}", runs.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut blocks = Vec::new();
    for file in files {
        let run = read_json(&file)?;
        let case = run["caseId"].as_str().unwrap_or_default().to_string();
        for candidate in run["candidates"].as_array().into_iter().flatten() {
            if candidate["kind"] == "map" {
                blocks.push(MapBlock {
                    case: case.clone(),
                    title: candidate["title"].as_str().unwrap_or_default().to_string(),
                    payload: candidate["payload"].clone(),
                });
            }
        }
    }
    Ok(blocks)
}

fn load_areas(path: &Path) -> Result<Arc<[Area]>, String> {
    let root = read_json(path)?;
    let areas = root
        .as_array()
        .ok_or("map context is not a list")?
        .iter()
        .map(|area| Area {
            rings: area["polygons"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|polygon| polygon.as_array().into_iter().flatten())
                .map(|ring| {
                    ring.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| Some([v[1].as_f64()?, v[0].as_f64()?]))
                        .collect()
                })
                .collect(),
        })
        .collect();
    Ok(areas)
}

fn lat_lon(value: &Value) -> Option<LatLon> {
    Some([value[0].as_f64()?, value[1].as_f64()?])
}

fn color(value: &Value) -> Option<Hsla> {
    let hex = value.as_str()?.strip_prefix('#')?;
    Some(rgb(u32::from_str_radix(hex, 16).ok()?).into())
}

fn faded(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

/// Drawing order of a line mode: context lines first, proposals last.
fn layer(mode: &str) -> u8 {
    match mode {
        "context" => 0,
        "context-route" => 1,
        "proposal" => 3,
        _ => 2,
    }
}

struct RouteMapView {
    blocks: Vec<MapBlock>,
    areas: Arc<[Area]>,
    index: usize,
}

impl RouteMapView {
    fn map(&self, block: &MapBlock) -> GeoMap {
        let p = &block.payload;
        let legend = p["legend"].as_array().cloned().unwrap_or_default();
        let lines = p["lines"].as_array().cloned().unwrap_or_default();
        let points = p["points"].as_array().cloned().unwrap_or_default();

        let mut map = GeoMap::new("route-map")
            .padding(0.14)
            .background(theme::pick(0x23272c, 0xf0f6fa))
            .areas(self.areas.clone(), theme::pick(0x2b3036, 0xe8f0f5))
            .area_border(px(1.), theme::pick(0x3a4048, 0xd2e0eb));

        let mut ordered: Vec<&Value> = lines.iter().collect();
        ordered.sort_by_key(|line| layer(line["mode"].as_str().unwrap_or_default()));
        for line in ordered {
            let mode = line["mode"].as_str().unwrap_or_default();
            let dash = line["dash"].as_bool().unwrap_or(false);
            let route_color = color(&line["color"])
                .or_else(|| {
                    legend
                        .iter()
                        .find(|item| item["label"] == line["label"])
                        .and_then(|item| color(&item["color"]))
                })
                .unwrap_or_else(|| match mode {
                    "context" => rgb(0xc3d3df).into(),
                    "walk" => rgb(0x10a171).into(),
                    _ => rgb(0x126af4).into(),
                });
            let alpha = if mode == "context-route" {
                0.45
            } else if dash && mode != "proposal" {
                0.8
            } else {
                1.
            };
            let width = match mode {
                "context" => px(1.2),
                "context-route" => px(1.8),
                "proposal" => px(2.4),
                _ if dash => px(2.4),
                _ => px(3.5),
            };
            let points = line["points"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(lat_lon);
            let mut route = Route::new(points.collect(), faded(route_color, alpha)).width(width);
            if mode == "walk" {
                route = route.dashed(px(6.), px(3.5));
            } else if dash {
                route = route.dashed(px(7.5), px(5.));
            }
            map = map.route(route);
        }

        // Labels go to the outer side of each marker, judged against the center of all points.
        let all: Vec<LatLon> = lines
            .iter()
            .flat_map(|line| {
                line["points"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(lat_lon)
            })
            .chain(points.iter().filter_map(|m| lat_lon(&m["point"])))
            .collect();
        let (lon_min, lon_max) = all
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                (lo.min(p[1]), hi.max(p[1]))
            });
        let center = (lon_min + lon_max) / 2.;
        let white: Hsla = rgb(0xffffff).into();
        // Label outlines match the map background so labels read the same in both themes.
        let halo: Hsla = theme::pick(0x23272c, 0xffffff).into();
        for marker in &points {
            let Some(at) = lat_lon(&marker["point"]) else {
                continue;
            };
            let label = marker["label"].as_str().unwrap_or_default().to_string();
            match marker["kind"].as_str() {
                Some("waypoint") => {
                    let fill = color(&marker["color"]).unwrap_or_else(|| rgb(0x7048e8).into());
                    map = map
                        .marker(Marker::new(at, px(8.), fill).border(px(1.5), white))
                        .label(
                            Label::new(at, label, white)
                                .align(LabelAlign::Center)
                                .bold()
                                .size(px(10.)),
                        );
                }
                kind => {
                    let mention = kind == Some("mention");
                    let (fill, border, text): (Hsla, Hsla, Hsla) = if mention {
                        (
                            rgb(0xf34b6e).into(),
                            halo,
                            theme::pick(0xff8fa6, 0xb0243f).into(),
                        )
                    } else {
                        (
                            halo,
                            theme::pick(0xdfe1e5, 0x173d6b).into(),
                            theme::pick(0xdfe1e5, 0x173d6b).into(),
                        )
                    };
                    let radius = if mention { px(4.5) } else { px(4.) };
                    let left = at[1] < center;
                    map = map
                        .marker(Marker::new(at, radius, fill).border(px(1.6), border))
                        .label(
                            Label::new(at, label, text)
                                .align(if left {
                                    LabelAlign::Start
                                } else {
                                    LabelAlign::End
                                })
                                .offset(if left { px(8.) } else { px(-8.) }, px(-6.))
                                .halo(halo)
                                .bold()
                                .size(px(12.)),
                        );
                }
            }
        }
        map
    }

    fn legend(&self, block: &MapBlock) -> impl IntoElement {
        let p = &block.payload;
        let items = p["legend"].as_array().cloned().unwrap_or_default();
        let kinds: Vec<&str> = p["points"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["kind"].as_str())
            .collect();
        let mut caption = Vec::new();
        if kinds.contains(&"terminus") {
            caption.push("○ terminus");
        }
        if kinds.contains(&"mention") {
            caption.push("● stop named in the complaint");
        }
        caption.push("bus lines follow stop order");
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .text_sm()
            .children(items.iter().map(|item| {
                let swatch = color(&item["color"]).unwrap_or_else(|| theme::text().into());
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().w(px(18.)).h(px(4.)).rounded_sm().bg(swatch))
                    .child(item["label"].as_str().unwrap_or_default().to_string())
            }))
            .child(
                div()
                    .text_color(theme::text_muted())
                    .child(caption.join(" · ")),
            )
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.blocks.len().saturating_sub(1);
        self.index = self.index.saturating_add_signed(delta).min(last);
        cx.notify();
    }
}

impl Render for RouteMapView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let block = &self.blocks[self.index];
        let title: SharedString = format!("Case {} · {}", block.case, block.title).into();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(theme::background())
            .text_color(theme::text())
            .font_family(theme::UI_FONT)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        small_button("previous", "← Previous")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.step(-1, cx))),
                    )
                    .child(format!("{} / {}", self.index + 1, self.blocks.len()))
                    .child(
                        small_button("next", "Next →")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.step(1, cx))),
                    )
                    .child(div().text_lg().child(title))
                    .child(div().flex_1())
                    .child(
                        small_button(
                            "theme",
                            match theme::mode() {
                                theme::Mode::Dark => "Light Mode",
                                theme::Mode::Light => "Dark Mode",
                            },
                        )
                        .on_click(cx.listener(
                            |_, _: &ClickEvent, window, cx| {
                                theme::set_mode(theme::mode().toggled());
                                window.refresh();
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(200.))
                    .rounded_md()
                    .overflow_hidden()
                    .child(self.map(block)),
            )
            .child(self.legend(block))
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(runs), Some(context)) = (args.next(), args.next()) else {
        eprintln!("usage: route_map <runs-directory> <map-context.json>");
        std::process::exit(2);
    };
    let (blocks, areas) = match (load_blocks(&runs), load_areas(&context)) {
        (Ok(blocks), Ok(areas)) if !blocks.is_empty() => (blocks, areas),
        (Ok(_), Ok(_)) => {
            eprintln!("no map blocks in {}", runs.display());
            std::process::exit(1);
        }
        (Err(err), _) | (_, Err(err)) => {
            eprintln!("cannot read data: {err}");
            std::process::exit(1);
        }
    };
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Route maps".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                theme::set_mode(theme::Mode::from_appearance(window.appearance()));
                cx.new(|_| RouteMapView {
                    blocks,
                    areas,
                    index: 0,
                })
            },
        )
        .expect("cannot open the window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
