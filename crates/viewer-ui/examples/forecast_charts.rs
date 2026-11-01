//! 확률예측 explorer 데이터(`explorer-data.json`)를 차트 모듈로 그리는 예제.
//!
//! ```text
//! cargo run -p viewer-ui --example forecast_charts -- <explorer-data.json> [YYYY-MM-DD]
//! ```
//!
//! 날짜별 24시간 예측구간, 계절별 편향 선 그래프, 분위수 신뢰도, 계절 × 시간대 편향 히트맵을
//! 그린다. 값은 파일에 있는 그대로 쓰고 다시 계산하지 않는다.

use std::path::PathBuf;

use gpui::{
    App, Bounds, ClickEvent, Context, Hsla, IntoElement, Render, SharedString, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, prelude::*, px, relative, rgb, size,
};
use gpui_platform::application;
use serde_json::Value;
use viewer_ui::chart::{Axis, Band, Line, LineChart, Rule};
use viewer_ui::heatmap::{DivergingScale, Heatmap};
use viewer_ui::theme;
use viewer_ui::widgets::small_button;

/// 분위수 7개(0.05·0.1·0.25·0.5·0.75·0.9·0.95) 중 띠의 아래·위와 투명도.
const BANDS: [(usize, usize, f32); 3] = [(0, 6, 0.10), (1, 5, 0.14), (2, 4, 0.20)];
const MEDIAN: usize = 3;

struct Day {
    date: String,
    group: String,
    capacity_mw: f64,
    actual: Vec<Option<f64>>,
    /// 전문가 순서대로 `[시각][분위수]`.
    quantiles: Vec<Vec<Vec<f64>>>,
}

struct ForecastData {
    /// (ID, 표시 이름).
    experts: Vec<(String, String)>,
    seasons: Vec<String>,
    dates: Vec<String>,
    days: Vec<Day>,
    tables: Value,
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn load(path: &PathBuf) -> Result<ForecastData, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let root: Value = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let experts: Vec<(String, String)> = root["experts"]
        .as_object()
        .ok_or("experts가 없습니다")?
        .iter()
        .map(|(id, meta)| (id.clone(), meta["label"].as_str().unwrap_or(id).to_string()))
        .collect();
    let seasons = root["seasons"]
        .as_array()
        .ok_or("seasons가 없습니다")?
        .iter()
        .filter_map(|season| season.as_str().map(str::to_string))
        .collect();
    let mut dates: Vec<String> = Vec::new();
    let days = root["rows"]
        .as_array()
        .ok_or("rows가 없습니다")?
        .iter()
        .map(|row| {
            let date = row["date"].as_str().unwrap_or_default().to_string();
            if dates.last() != Some(&date) {
                dates.push(date.clone());
            }
            Day {
                date,
                group: row["group"].as_str().unwrap_or_default().to_string(),
                capacity_mw: number(&row["capacity_mw"]).unwrap_or_default(),
                actual: row["actual"]
                    .as_array()
                    .map(|values| values.iter().map(number).collect())
                    .unwrap_or_default(),
                quantiles: experts
                    .iter()
                    .map(|(id, _)| {
                        row[id.as_str()]
                            .as_array()
                            .map(|hours| {
                                hours
                                    .iter()
                                    .map(|q| {
                                        q.as_array()
                                            .map(|q| q.iter().filter_map(number).collect())
                                            .unwrap_or_default()
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(ForecastData {
        experts,
        seasons,
        dates,
        days,
        tables: root["tables"].clone(),
    })
}

struct ForecastCharts {
    data: ForecastData,
    date: usize,
    /// 마우스가 올라간 차트와 시각 순번.
    hover: Option<(usize, usize)>,
}

fn expert_color(ix: usize) -> Hsla {
    match ix % 2 {
        0 => theme::accent().into(),
        _ => rgb(0xffa475).into(),
    }
}

fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

fn hours() -> Vec<f64> {
    (1..=24).map(f64::from).collect()
}

impl ForecastCharts {
    fn set_hover(&mut self, panel: usize, hour: Option<usize>, cx: &mut Context<Self>) {
        let next = match hour {
            Some(hour) => Some((panel, hour)),
            None if self.hover.is_some_and(|(current, _)| current == panel) => None,
            None => self.hover,
        };
        if next != self.hover {
            self.hover = next;
            cx.notify();
        }
    }

    fn move_date(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.data.dates.len().saturating_sub(1) as isize;
        self.date = (self.date as isize + delta).clamp(0, last) as usize;
        self.hover = None;
        cx.notify();
    }

    fn selected_days(&self) -> Vec<&Day> {
        let date = &self.data.dates[self.date];
        self.data
            .days
            .iter()
            .filter(|day| &day.date == date)
            .collect()
    }

    /// 날짜·그룹·전문가 하나의 24시간 예측구간 차트.
    fn fan_chart(
        &self,
        panel: usize,
        day: &Day,
        expert: usize,
        y_max: f64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let color = expert_color(expert);
        let q = &day.quantiles[expert];
        let column = |ix: usize| -> Vec<Option<f64>> {
            q.iter().map(|hour| hour.get(ix).copied()).collect()
        };
        let mut chart = LineChart::new(
            Axis::new(1., 24.)
                .ticks([1., 6., 12., 18., 24.].map(|h| (h, format!("{h}").into())))
                .title("종료 시각 (KST)"),
            Axis::new(0., y_max).nice_ticks(4).title("MWh"),
        );
        for (lower, upper, alpha) in BANDS {
            chart = chart.layer(Band::new(
                hours(),
                column(lower),
                column(upper),
                with_alpha(color, alpha),
            ));
        }
        let hovered = self
            .hover
            .filter(|(p, _)| *p == panel)
            .map(|(_, hour)| hour);
        let view = cx.entity().downgrade();
        chart
            .layer(Line::new(hours(), column(MEDIAN), color))
            .layer(
                Line::new(hours(), day.actual.clone(), theme::text().into())
                    .width(px(1.6))
                    .dashed(px(4.), px(3.)),
            )
            .on_hover(hours(), hovered, move |hour, _window, cx| {
                view.update(cx, |this, cx| this.set_hover(panel, hour, cx))
                    .ok();
            })
    }

    fn hover_text(&self) -> SharedString {
        let Some((panel, hour)) = self.hover else {
            return "차트에 마우스를 올리면 그 시각의 값을 표시합니다.".into();
        };
        let days = self.selected_days();
        let experts = self.data.experts.len();
        let Some(day) = days.get(panel / experts) else {
            return SharedString::default();
        };
        let fmt = |value: Option<f64>| value.map_or("—".into(), |v| format!("{v:.3}"));
        let predictions: Vec<String> = self
            .data
            .experts
            .iter()
            .enumerate()
            .map(|(ix, (_, label))| {
                let q = &day.quantiles[ix][hour];
                format!(
                    "{label} 중앙값 {}, 80% [{}, {}]",
                    fmt(q.get(MEDIAN).copied()),
                    fmt(q.get(1).copied()),
                    fmt(q.get(5).copied())
                )
            })
            .collect();
        format!(
            "{} {} {}시 · 실제 {} · {} MWh",
            day.date,
            day.group,
            hour + 1,
            fmt(day.actual.get(hour).copied().flatten()),
            predictions.join(" · ")
        )
        .into()
    }

    /// 계절별 중앙값 편향(용량 대비 %). 전문가 × 그룹마다 선 하나.
    fn seasonal_chart(&self) -> impl IntoElement + use<> {
        let seasons = &self.data.seasons;
        let rows = self.data.tables["seasonal_summary"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut lines = Vec::new();
        let mut bound: f64 = 0.;
        for (ix, (id, _)) in self.data.experts.iter().enumerate() {
            for (g, group) in ["G1", "G2"].into_iter().enumerate() {
                let values: Vec<Option<f64>> = seasons
                    .iter()
                    .map(|season| {
                        rows.iter()
                            .find(|row| {
                                row["expert"] == id.as_str()
                                    && row["group"] == group
                                    && row["season"] == season.as_str()
                            })
                            .and_then(|row| number(&row["median_bias_norm"]))
                            .map(|v| v * 100.)
                    })
                    .collect();
                bound = values.iter().flatten().fold(bound, |b, v| b.max(v.abs()));
                let line = Line::new(
                    (0..seasons.len()).map(|s| s as f64).collect(),
                    values,
                    expert_color(ix),
                )
                .markers();
                lines.push(if g == 1 {
                    line.dashed(px(5.), px(3.))
                } else {
                    line
                });
            }
        }
        let bound = (bound * 1.15).max(0.1);
        let mut chart = LineChart::new(
            Axis::new(-0.4, seasons.len() as f64 - 0.6).ticks(
                seasons
                    .iter()
                    .enumerate()
                    .map(|(ix, season)| (ix as f64, season.clone().into())),
            ),
            Axis::new(-bound, bound)
                .nice_ticks(4)
                .title("중앙값 편향 / 용량 (%)"),
        )
        .layer(Rule::new(
            (-1., 0.),
            (seasons.len() as f64, 0.),
            theme::text_muted().into(),
        ));
        for line in lines {
            chart = chart.layer(line);
        }
        chart
    }

    /// 한 그룹·계절의 분위수 신뢰도. 띠는 P(Y<q)와 P(Y≤q) 사이다.
    fn reliability_chart(&self, group: &str, season: &str) -> impl IntoElement + use<> {
        let rows = self.data.tables["reliability"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut chart = LineChart::new(
            Axis::new(0., 1.)
                .nice_ticks(4)
                .title(format!("분위수 수준 · {group} · {season}")),
            Axis::new(0., 1.).nice_ticks(4).title("P(Y<q) ~ P(Y≤q)"),
        )
        .layer(Rule::new((0., 0.), (1., 1.), theme::text_muted().into()).dashed(px(2.), px(3.)));
        for (ix, (id, _)) in self.data.experts.iter().enumerate() {
            let points: Vec<&Value> = rows
                .iter()
                .filter(|row| {
                    row["expert"] == id.as_str() && row["group"] == group && row["season"] == season
                })
                .collect();
            let alpha: Vec<f64> = points
                .iter()
                .filter_map(|row| number(&row["alpha"]))
                .collect();
            let strict = points.iter().map(|row| number(&row["strict"])).collect();
            let inclusive: Vec<Option<f64>> =
                points.iter().map(|row| number(&row["inclusive"])).collect();
            let color = expert_color(ix);
            chart = chart
                .layer(Band::new(
                    alpha.clone(),
                    strict,
                    inclusive.clone(),
                    with_alpha(color, 0.2),
                ))
                .layer(Line::new(alpha, inclusive, color).width(px(1.5)));
        }
        chart
    }

    /// 첫 전문가·한 그룹의 계절 × 시간대 중앙값 편향(용량 대비 %).
    fn hourly_heatmap(&self, group: &str) -> impl IntoElement + use<> {
        let rows = self.data.tables["hourly_bias"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let expert = self.data.experts[0].0.clone();
        let bound = rows
            .iter()
            .filter_map(|row| number(&row["median_bias_norm"]))
            .fold(0.0001_f64, |b, v| b.max(v.abs()))
            * 100.;
        let values = self
            .data
            .seasons
            .iter()
            .map(|season| {
                (1..=24)
                    .map(|hour| {
                        rows.iter()
                            .find(|row| {
                                row["expert"] == expert.as_str()
                                    && row["group"] == group
                                    && row["season"] == season.as_str()
                                    && row["hour"] == hour
                            })
                            .and_then(|row| number(&row["median_bias_norm"]))
                            .map(|v| v * 100.)
                    })
                    .collect()
            })
            .collect();
        Heatmap::new(
            self.data.seasons.iter().map(|s| s.clone().into()).collect(),
            (1..=24)
                .map(|h| SharedString::from(h.to_string()))
                .collect(),
            values,
            DivergingScale {
                bound,
                negative: rgb(0x2166ac),
                middle: rgb(0x3a3d42),
                positive: rgb(0xb2182b),
            },
        )
        .column_ticks(vec![0, 5, 11, 17, 23])
        .format(|v| format!("{v:+.1}%"))
    }
}

fn panel(title: impl Into<SharedString>, height: f32, chart: impl IntoElement) -> gpui::Div {
    div()
        .w(relative(0.5))
        .p_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child(title.into()),
        )
        .child(div().h(px(height)).w_full().child(chart))
}

impl Render for ForecastCharts {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let days = self.selected_days();
        let y_max = days
            .iter()
            .flat_map(|day| {
                day.actual
                    .iter()
                    .flatten()
                    .copied()
                    .chain(day.quantiles.iter().flatten().flatten().copied())
            })
            .fold(0.1_f64, f64::max)
            * 1.07;
        let mut fans = Vec::new();
        for (d, day) in days.iter().enumerate() {
            for (e, (_, label)) in self.data.experts.iter().enumerate() {
                let panel_ix = d * self.data.experts.len() + e;
                let title = format!("{} · {} · {:.2} MW", day.group, label, day.capacity_mw);
                fans.push(panel(
                    title,
                    260.,
                    self.fan_chart(panel_ix, day, e, y_max, cx),
                ));
            }
        }
        let summer = self
            .data
            .seasons
            .iter()
            .find(|s| s.as_str() == "summer")
            .or(self.data.seasons.first())
            .cloned()
            .unwrap_or_default();
        div()
            .id("forecast-charts")
            .size_full()
            .overflow_y_scroll()
            .bg(theme::background())
            .text_color(theme::text_muted())
            .font_family(theme::UI_FONT)
            .text_xs()
            .p_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .child(small_button("previous", "← 이전").on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.move_date(-1, cx)),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text())
                            .child(self.data.dates[self.date].clone()),
                    )
                    .child(
                        small_button("next", "다음 →").on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.move_date(1, cx)),
                        ),
                    )
                    .child(div().min_w_0().truncate().child(self.hover_text())),
            )
            .child(div().flex().flex_wrap().children(fans))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .child(panel(
                        "계절별 중앙값 편향 (실선 G1 · 점선 G2)",
                        240.,
                        self.seasonal_chart(),
                    ))
                    .child(panel(
                        "분위수 신뢰도",
                        240.,
                        self.reliability_chart("G1", &summer),
                    ))
                    .child(panel(
                        format!("계절 × 시간대 편향 · {} · G1", self.data.experts[0].1),
                        200.,
                        self.hourly_heatmap("G1"),
                    ))
                    .child(panel(
                        format!("계절 × 시간대 편향 · {} · G2", self.data.experts[0].1),
                        200.,
                        self.hourly_heatmap("G2"),
                    )),
            )
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("사용법: forecast_charts <explorer-data.json> [YYYY-MM-DD]");
        std::process::exit(2);
    };
    let data = match load(&path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("데이터를 읽을 수 없습니다: {err}");
            std::process::exit(1);
        }
    };
    let date = args
        .next()
        .and_then(|date| data.dates.iter().position(|d| *d == date))
        .unwrap_or(0);

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1400.), px(1000.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("확률예측 차트".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| ForecastCharts {
                    data,
                    date,
                    hover: None,
                })
            },
        )
        .expect("창을 열 수 없습니다");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
