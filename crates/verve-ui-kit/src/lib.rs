//! Material 3 design system thin wrapper for Verve (`0.0.2`).
//!
//! Provides Material 3 theme setup, semantic status/latency tokens, formatting helpers,
//! stateless UI primitives, and GPU Canvas traffic visualizations with zero domain state.

#![warn(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod chart;
pub mod format;
pub mod primitives;
pub mod theme;

pub use chart::{
    ChartWindow, MiniSparkline, NormalizedTrafficSeries, TrafficChart, TrafficDataPoint,
    normalize_series,
};
pub use format::{format_bytes, format_duration_hms, format_throughput_bps};
pub use primitives::{EmptyState, HealthIndicator, LatencyBadge, MetricTile, SectionCard};
pub use theme::{
    AppearanceMode, DEFAULT_SEED_ARGB, KernelHealthState, LatencyBand, ThemeTokens, apply_theme,
    build_material_theme, init_ui_kit, is_dark_theme, role_color, theme_tokens,
};

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext, Context, IntoElement, ParentElement, Render, Styled, TestAppContext, Window,
        div, px,
    };

    use super::*;

    struct UiKitShowcase {
        samples: Vec<TrafficDataPoint>,
    }

    impl Render for UiKitShowcase {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .gap(px(12.0))
                .p(px(16.0))
                .child(
                    SectionCard::new()
                        .title("Overview")
                        .subtitle("Material 3 primitives & Canvas telemetry")
                        .header_trailing(HealthIndicator::new(KernelHealthState::Running))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(8.0))
                                .child(
                                    MetricTile::new("Download", format_throughput_bps(12_800))
                                        .value_role(gpui_material::theme::ColorRole::Primary),
                                )
                                .child(
                                    MetricTile::new("Upload", format_throughput_bps(4_096))
                                        .sublabel(format_duration_hms(252)),
                                )
                                .child(HealthIndicator::new(KernelHealthState::Starting))
                                .child(
                                    HealthIndicator::new(KernelHealthState::Fault).label("Error"),
                                )
                                .child(LatencyBadge::new(Some(42)))
                                .child(LatencyBadge::new(Some(240)))
                                .child(LatencyBadge::new(Some(650)))
                                .child(LatencyBadge::new(None)),
                        )
                        .child(TrafficChart::new(self.samples.clone()))
                        .child(
                            TrafficChart::new(self.samples.clone())
                                .window(ChartWindow::FiveMinutes)
                                .height(px(96.0))
                                .show_peak_label(false),
                        )
                        .child(MiniSparkline::new(self.samples.clone())),
                )
                .child(
                    EmptyState::new("No active rules", "Add a rule to inspect traffic.")
                        .action(HealthIndicator::new(KernelHealthState::Stopped)),
                )
        }
    }

    #[gpui::test]
    fn renders_all_primitives_and_toggles_light_to_dark_theme(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let light = build_material_theme(AppearanceMode::Light, Some(DEFAULT_SEED_ARGB), false);
            init_ui_kit(light, cx);
            cx.set_reduce_motion(true);
        });

        let window = cx.add_window(|_window, _cx| UiKitShowcase {
            samples: vec![
                TrafficDataPoint {
                    up_bps: 1_024,
                    down_bps: 8_192,
                },
                TrafficDataPoint {
                    up_bps: 2_048,
                    down_bps: 24_576,
                },
                TrafficDataPoint {
                    up_bps: 4_096,
                    down_bps: 16_384,
                },
            ],
        });

        let light_surface = cx
            .update_window(window.into(), |_, window, cx| {
                let light_tokens = theme_tokens(cx);
                assert!(!light_tokens.is_dark);
                let _ = window.draw(cx);
                light_tokens.surface
            })
            .expect("light mode render pass must succeed");

        cx.update(|cx| {
            let dark = build_material_theme(AppearanceMode::Dark, Some(DEFAULT_SEED_ARGB), true);
            apply_theme(dark, cx);
        });
        cx.run_until_parked();

        let dark_surface = cx
            .update_window(window.into(), |_, window, cx| {
                let dark_tokens = theme_tokens(cx);
                assert!(dark_tokens.is_dark);
                let _ = window.draw(cx);
                dark_tokens.surface
            })
            .expect("dark mode render pass must succeed");

        assert_ne!(light_surface, dark_surface);
        assert_eq!(LatencyBadge::new(Some(42)).formatted_text(), "42 ms");
        assert_eq!(LatencyBadge::new(None).formatted_text(), "--");
    }
}
