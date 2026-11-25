//! GPU-accelerated Canvas traffic chart and compact sidebar sparkline.

use gpui::{
    App, Bounds, Hsla, IntoElement, ParentElement, PathBuilder, Pixels, Point, RenderOnce, Styled,
    Window, canvas, div, point, px,
};
use serde::{Deserialize, Serialize};

use crate::format::format_throughput_bps;
use crate::theme::theme_tokens;

const MIN_SCALE_PEAK_BPS: u64 = 1024;
const CHART_VERTICAL_PADDING_PX: f32 = 4.0;

/// Single throughput sample in bytes per second for upload and download streams.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficDataPoint {
    /// Upload throughput in bytes per second.
    pub up_bps: u64,
    /// Download throughput in bytes per second.
    pub down_bps: u64,
}

/// Time window presets supported by the traffic chart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChartWindow {
    /// 60 one-second samples (1 minute).
    #[default]
    OneMinute,
    /// 300 one-second samples (5 minutes).
    FiveMinutes,
}

impl ChartWindow {
    /// Returns the sample capacity for this window.
    pub const fn sample_count(&self) -> usize {
        match self {
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
        }
    }

    /// Returns a compact UI label for this window (`"1m"` or `"5m"`).
    pub const fn label(&self) -> &'static str {
        match self {
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
        }
    }
}

/// Normalized, right-aligned upload and download series ready for canvas path rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedTrafficSeries {
    /// Peak throughput in bytes per second across the visible window (floored at `1024`).
    pub peak_bps: u64,
    /// Number of real samples present in the visible window (`<= capacity`).
    pub sample_count: usize,
    /// Right-aligned normalized upload points in `0.0..=1.0`.
    pub up: Vec<f32>,
    /// Right-aligned normalized download points in `0.0..=1.0`.
    pub down: Vec<f32>,
}

/// Normalizes `points` into `[0.0..=1.0]` coordinates across a fixed-width `capacity` window.
///
/// - When `points.len() > capacity`, only the trailing `capacity` samples are retained.
/// - When `points.len() < capacity`, samples are right-aligned (leading slots are `0.0`) so live
///   telemetry enters from the right edge like an oscilloscope.
/// - `peak_bps` is floored at `1024` B/s to avoid division by zero when traffic is idle.
pub fn normalize_series(points: &[TrafficDataPoint], capacity: usize) -> NormalizedTrafficSeries {
    if capacity == 0 {
        return NormalizedTrafficSeries {
            peak_bps: MIN_SCALE_PEAK_BPS,
            sample_count: 0,
            up: Vec::new(),
            down: Vec::new(),
        };
    }

    let start = points.len().saturating_sub(capacity);
    let visible = &points[start..];

    let max_bps = visible.iter().fold(0_u64, |acc, point| {
        acc.max(point.up_bps).max(point.down_bps)
    });
    let peak_bps = max_bps.max(MIN_SCALE_PEAK_BPS);
    let peak_f64 = peak_bps as f64;

    let pad = capacity.saturating_sub(visible.len());
    let mut up = vec![0.0_f32; pad];
    let mut down = vec![0.0_f32; pad];
    up.reserve(visible.len());
    down.reserve(visible.len());

    for point in visible {
        let norm_up = ((point.up_bps as f64) / peak_f64).clamp(0.0, 1.0) as f32;
        let norm_down = ((point.down_bps as f64) / peak_f64).clamp(0.0, 1.0) as f32;
        up.push(norm_up);
        down.push(norm_down);
    }

    NormalizedTrafficSeries {
        peak_bps,
        sample_count: visible.len(),
        up,
        down,
    }
}

/// Full-size Material 3 GPU Canvas traffic chart with download/upload curves and optional peak caption.
#[derive(IntoElement)]
pub struct TrafficChart {
    points: Vec<TrafficDataPoint>,
    window: ChartWindow,
    height: Pixels,
    show_peak_label: bool,
}

impl TrafficChart {
    /// Creates a traffic chart from a slice or vector of throughput samples.
    pub fn new(points: impl Into<Vec<TrafficDataPoint>>) -> Self {
        Self {
            points: points.into(),
            window: ChartWindow::OneMinute,
            height: px(140.0),
            show_peak_label: true,
        }
    }

    /// Sets the sample window capacity ([`ChartWindow::OneMinute`] or [`ChartWindow::FiveMinutes`]).
    pub fn window(mut self, window: ChartWindow) -> Self {
        self.window = window;
        self
    }

    /// Overrides the chart canvas height (defaults to `140px`).
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// Toggles the top-right peak scale caption (enabled by default).
    pub fn show_peak_label(mut self, show: bool) -> Self {
        self.show_peak_label = show;
        self
    }
}

impl RenderOnce for TrafficChart {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let series = normalize_series(&self.points, self.window.sample_count());
        let peak_caption = format_throughput_bps(series.peak_bps);

        let down_color = tokens.primary;
        let up_color = tokens.tertiary;
        let grid_color = tokens.outline_variant.opacity(0.45);

        let chart_canvas = canvas(
            |_, _, _| (),
            move |bounds, _, window, _cx| {
                paint_traffic_canvas(
                    bounds,
                    &series,
                    TrafficCanvasStyle {
                        down_color,
                        up_color,
                        grid_color: Some(grid_color),
                        stroke_width: px(2.0),
                        fill_alpha: 0.14,
                    },
                    window,
                );
            },
        )
        .size_full();

        let mut root = div()
            .relative()
            .w_full()
            .h(self.height)
            .rounded(px(12.0))
            .bg(tokens.surface_container_lowest)
            .border_1()
            .border_color(tokens.outline_variant)
            .overflow_hidden()
            .child(chart_canvas);

        if self.show_peak_label {
            root = root.child(
                div()
                    .absolute()
                    .top(px(6.0))
                    .right(px(8.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(tokens.surface_container.opacity(0.85))
                    .text_xs()
                    .text_color(tokens.on_surface_variant)
                    .child(peak_caption),
            );
        }

        root
    }
}

/// Borderless `40px`-tall mini sparkline for `AppShell`'s sidebar footer telemetry pod.
#[derive(IntoElement)]
pub struct MiniSparkline {
    points: Vec<TrafficDataPoint>,
    window: ChartWindow,
    height: Pixels,
}

impl MiniSparkline {
    /// Creates a compact sparkline from a slice or vector of throughput samples.
    pub fn new(points: impl Into<Vec<TrafficDataPoint>>) -> Self {
        Self {
            points: points.into(),
            window: ChartWindow::OneMinute,
            height: px(40.0),
        }
    }

    /// Sets the sample window capacity (defaults to [`ChartWindow::OneMinute`]).
    pub fn window(mut self, window: ChartWindow) -> Self {
        self.window = window;
        self
    }

    /// Overrides the sparkline height (defaults to `40px`).
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }
}

impl RenderOnce for MiniSparkline {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = theme_tokens(cx);
        let series = normalize_series(&self.points, self.window.sample_count());

        let down_color = tokens.primary;
        let up_color = tokens.tertiary;

        div().w_full().h(self.height).child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _cx| {
                    paint_traffic_canvas(
                        bounds,
                        &series,
                        TrafficCanvasStyle {
                            down_color,
                            up_color,
                            grid_color: None,
                            stroke_width: px(1.5),
                            fill_alpha: 0.12,
                        },
                        window,
                    );
                },
            )
            .size_full(),
        )
    }
}

struct TrafficCanvasStyle {
    down_color: Hsla,
    up_color: Hsla,
    grid_color: Option<Hsla>,
    stroke_width: Pixels,
    fill_alpha: f32,
}

fn paint_traffic_canvas(
    bounds: Bounds<Pixels>,
    series: &NormalizedTrafficSeries,
    style: TrafficCanvasStyle,
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if !width.is_finite() || !height.is_finite() || width <= 1.0 || height <= 1.0 {
        return;
    }

    let pad_y = CHART_VERTICAL_PADDING_PX.min(height * 0.25);
    let plot_height = (height - pad_y * 2.0).max(1.0);
    let left = f32::from(bounds.origin.x);
    let bottom = f32::from(bounds.origin.y) + height - pad_y;

    if let Some(grid_color) = style.grid_color {
        for fraction in [0.25_f32, 0.5, 0.75] {
            let y = bottom - plot_height * fraction;
            let mut grid = PathBuilder::stroke(px(1.0));
            grid.move_to(point(px(left), px(y)));
            grid.line_to(point(px(left + width), px(y)));
            if let Ok(path) = grid.build() {
                window.paint_path(path, grid_color);
            }
        }
    }

    if series.down.is_empty() || series.up.is_empty() {
        return;
    }

    let down_points = series_points(&series.down, left, width, bottom, plot_height);
    let up_points = series_points(&series.up, left, width, bottom, plot_height);

    paint_area_and_stroke(
        &down_points,
        bottom,
        style.down_color,
        style.fill_alpha,
        style.stroke_width,
        window,
    );
    paint_area_and_stroke(
        &up_points,
        bottom,
        style.up_color,
        style.fill_alpha * 0.7,
        style.stroke_width,
        window,
    );
}

fn series_points(
    values: &[f32],
    left: f32,
    width: f32,
    bottom: f32,
    plot_height: f32,
) -> Vec<Point<Pixels>> {
    if values.len() == 1 {
        let y = bottom - values[0].clamp(0.0, 1.0) * plot_height;
        return vec![point(px(left), px(y)), point(px(left + width), px(y))];
    }

    let denom = (values.len() - 1) as f32;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let x = left + width * (index as f32 / denom);
            let y = bottom - value.clamp(0.0, 1.0) * plot_height;
            point(px(x), px(y))
        })
        .collect()
}

fn paint_area_and_stroke(
    points: &[Point<Pixels>],
    baseline_y: f32,
    color: Hsla,
    fill_alpha: f32,
    stroke_width: Pixels,
    window: &mut Window,
) {
    if points.len() < 2 {
        return;
    }

    if fill_alpha > 0.0 {
        let mut fill = PathBuilder::fill();
        fill.move_to(point(points[0].x, px(baseline_y)));
        for &pt in points {
            fill.line_to(pt);
        }
        fill.line_to(point(points[points.len() - 1].x, px(baseline_y)));
        fill.close();
        if let Ok(path) = fill.build() {
            window.paint_path(path, color.opacity(fill_alpha));
        }
    }

    let mut stroke = PathBuilder::stroke(stroke_width);
    stroke.move_to(points[0]);
    for &pt in &points[1..] {
        stroke.line_to(pt);
    }
    if let Ok(path) = stroke.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_window_metadata_matches_spec() {
        assert_eq!(ChartWindow::default(), ChartWindow::OneMinute);
        assert_eq!(ChartWindow::OneMinute.sample_count(), 60);
        assert_eq!(ChartWindow::OneMinute.label(), "1m");
        assert_eq!(ChartWindow::FiveMinutes.sample_count(), 300);
        assert_eq!(ChartWindow::FiveMinutes.label(), "5m");
    }

    #[test]
    fn normalize_series_handles_empty_and_zero_traffic() {
        let empty = normalize_series(&[], 60);
        assert_eq!(empty.peak_bps, 1024);
        assert_eq!(empty.sample_count, 0);
        assert_eq!(empty.up.len(), 60);
        assert_eq!(empty.down.len(), 60);
        assert!(empty.up.iter().all(|&v| v == 0.0));
        assert!(empty.down.iter().all(|&v| v == 0.0));

        let idle = vec![TrafficDataPoint::default(); 5];
        let normalized_idle = normalize_series(&idle, 10);
        assert_eq!(normalized_idle.peak_bps, 1024);
        assert_eq!(normalized_idle.sample_count, 5);
        assert_eq!(normalized_idle.up, vec![0.0; 10]);
        assert_eq!(normalized_idle.down, vec![0.0; 10]);

        let zero_cap = normalize_series(&idle, 0);
        assert_eq!(zero_cap.peak_bps, 1024);
        assert!(zero_cap.up.is_empty());
        assert!(zero_cap.down.is_empty());
    }

    #[test]
    fn normalize_series_right_aligns_partial_windows() {
        let samples = vec![
            TrafficDataPoint {
                up_bps: 512,
                down_bps: 1024,
            },
            TrafficDataPoint {
                up_bps: 2048,
                down_bps: 1024,
            },
        ];

        let normalized = normalize_series(&samples, 5);
        assert_eq!(normalized.peak_bps, 2048);
        assert_eq!(normalized.sample_count, 2);
        assert_eq!(normalized.up, vec![0.0, 0.0, 0.0, 0.25, 1.0]);
        assert_eq!(normalized.down, vec![0.0, 0.0, 0.0, 0.5, 0.5]);
    }

    #[test]
    fn normalize_series_truncates_older_samples_and_scales_high_throughput() {
        let samples = vec![
            TrafficDataPoint {
                up_bps: 10_000_000_000,
                down_bps: 10_000_000_000,
            },
            TrafficDataPoint {
                up_bps: 2_500_000,
                down_bps: 5_000_000,
            },
            TrafficDataPoint {
                up_bps: 1_250_000,
                down_bps: 10_000_000,
            },
        ];

        // Only the last 2 samples remain in view, so the 10 GB/s outlier is excluded.
        let normalized = normalize_series(&samples, 2);
        assert_eq!(normalized.peak_bps, 10_000_000);
        assert_eq!(normalized.sample_count, 2);
        assert!((normalized.up[0] - 0.25).abs() < 1e-6);
        assert!((normalized.up[1] - 0.125).abs() < 1e-6);
        assert!((normalized.down[0] - 0.5).abs() < 1e-6);
        assert!((normalized.down[1] - 1.0).abs() < 1e-6);
    }
}
