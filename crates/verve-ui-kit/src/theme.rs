//! Material 3 theme initialization, appearance modes, and semantic status/latency tokens.

use gpui::{App, Hsla};
use gpui_material::MaterialTheme;
use gpui_material::foundation::color as argb_to_hsla;
use gpui_material::theme::{Appearance, Argb, ColorRole, ColorScheme};
use serde::{Deserialize, Serialize};

/// Default Material 3 Expressive deep-teal seed color (`0xFF006A6A`).
pub const DEFAULT_SEED_ARGB: u32 = 0xFF00_6A6A;

/// User-facing appearance preference matching `verve-config`'s `"light" | "dark" | "system"` contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
    /// Always use the Light Material 3 palette.
    Light,
    /// Always use the Dark Material 3 palette.
    Dark,
    /// Follow the host operating system's appearance setting.
    #[default]
    System,
}

impl AppearanceMode {
    /// Parses a persisted configuration string (`"light"`, `"dark"`, or `"system"`, case-insensitive).
    pub fn from_config_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    /// Returns the canonical lowercase configuration string (`"light"`, `"dark"`, or `"system"`).
    pub const fn as_config_str(&self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    /// Resolves this preference to a concrete Material [`Appearance`] given the host `system_dark` state.
    pub const fn resolve_appearance(&self, system_dark: bool) -> Appearance {
        match self {
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
            Self::System => {
                if system_dark {
                    Appearance::Dark
                } else {
                    Appearance::Light
                }
            }
        }
    }
}

/// Builds a [`MaterialTheme`] for the requested [`AppearanceMode`], optional seed ARGB color,
/// and current host `system_dark` flag.
///
/// When `seed_argb` is `None`, the Material 3 baseline scheme is returned.
pub fn build_material_theme(
    mode: AppearanceMode,
    seed_argb: Option<u32>,
    system_dark: bool,
) -> MaterialTheme {
    let appearance = mode.resolve_appearance(system_dark);
    match seed_argb {
        Some(argb) => MaterialTheme::from_seed(Argb::from_u32(argb), appearance, 0.0),
        None => MaterialTheme::baseline(appearance),
    }
}

/// Initializes `gpui-material` and `gpui-base` with the initial [`MaterialTheme`].
///
/// Must be called once at application startup before opening windows.
pub fn init_ui_kit(theme: MaterialTheme, cx: &mut App) {
    gpui_material::init(theme, cx);
}

/// Replaces the active application-wide [`MaterialTheme`].
pub fn apply_theme(theme: MaterialTheme, cx: &mut App) {
    gpui_material::set_material_theme(theme, cx);
}

/// Converts a single [`ColorRole`] from `theme` into a GPUI [`Hsla`] color.
pub fn role_color(theme: &MaterialTheme, role: ColorRole) -> Hsla {
    argb_to_hsla(theme.colors.color(role))
}

/// Returns `true` when `theme` is configured for a Dark appearance.
pub const fn is_dark_theme(theme: &MaterialTheme) -> bool {
    matches!(theme.colors.appearance(), Appearance::Dark)
}

/// Resolved Material 3 semantic colors extracted from the active [`MaterialTheme`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeTokens {
    /// Whether the active theme uses a Dark color scheme.
    pub is_dark: bool,
    /// Primary background surface (`surface`).
    pub surface: Hsla,
    /// Lowest-emphasis surface container (`surface_container_lowest`).
    pub surface_container_lowest: Hsla,
    /// Low-emphasis card surface (`surface_container_low`).
    pub surface_container_low: Hsla,
    /// Default surface container (`surface_container`).
    pub surface_container: Hsla,
    /// High-emphasis surface container (`surface_container_high`).
    pub surface_container_high: Hsla,
    /// Primary text and icon foreground (`on_surface`).
    pub on_surface: Hsla,
    /// Muted secondary text and icon foreground (`on_surface_variant`).
    pub on_surface_variant: Hsla,
    /// Primary brand accent (`primary`).
    pub primary: Hsla,
    /// Foreground paired with `primary` (`on_primary`).
    pub on_primary: Hsla,
    /// Tonal primary container (`primary_container`).
    pub primary_container: Hsla,
    /// Foreground paired with `primary_container` (`on_primary_container`).
    pub on_primary_container: Hsla,
    /// Secondary brand accent (`secondary`).
    pub secondary: Hsla,
    /// Tonal secondary container (`secondary_container`).
    pub secondary_container: Hsla,
    /// Foreground paired with `secondary_container` (`on_secondary_container`).
    pub on_secondary_container: Hsla,
    /// Tertiary brand accent (`tertiary`).
    pub tertiary: Hsla,
    /// Tonal tertiary container (`tertiary_container`).
    pub tertiary_container: Hsla,
    /// Foreground paired with `tertiary_container` (`on_tertiary_container`).
    pub on_tertiary_container: Hsla,
    /// Prominent border outline (`outline`).
    pub outline: Hsla,
    /// Subtle divider and card outline (`outline_variant`).
    pub outline_variant: Hsla,
    /// Error accent (`error`).
    pub error: Hsla,
    /// Error container fill (`error_container`).
    pub error_container: Hsla,
    /// Foreground paired with `error_container` (`on_error_container`).
    pub on_error_container: Hsla,
    /// Success accent (`success`).
    pub success: Hsla,
    /// Success container fill (`success_container`).
    pub success_container: Hsla,
    /// Foreground paired with `success_container` (`on_success_container`).
    pub on_success_container: Hsla,
    /// Warning accent (`warning`).
    pub warning: Hsla,
    /// Warning container fill (`warning_container`).
    pub warning_container: Hsla,
    /// Foreground paired with `warning_container` (`on_warning_container`).
    pub on_warning_container: Hsla,
}

impl ThemeTokens {
    /// Extracts resolved [`Hsla`] semantic tokens from `theme`.
    pub fn from_theme(theme: &MaterialTheme) -> Self {
        Self {
            is_dark: is_dark_theme(theme),
            surface: role_color(theme, ColorRole::Surface),
            surface_container_lowest: role_color(theme, ColorRole::SurfaceContainerLowest),
            surface_container_low: role_color(theme, ColorRole::SurfaceContainerLow),
            surface_container: role_color(theme, ColorRole::SurfaceContainer),
            surface_container_high: role_color(theme, ColorRole::SurfaceContainerHigh),
            on_surface: role_color(theme, ColorRole::OnSurface),
            on_surface_variant: role_color(theme, ColorRole::OnSurfaceVariant),
            primary: role_color(theme, ColorRole::Primary),
            on_primary: role_color(theme, ColorRole::OnPrimary),
            primary_container: role_color(theme, ColorRole::PrimaryContainer),
            on_primary_container: role_color(theme, ColorRole::OnPrimaryContainer),
            secondary: role_color(theme, ColorRole::Secondary),
            secondary_container: role_color(theme, ColorRole::SecondaryContainer),
            on_secondary_container: role_color(theme, ColorRole::OnSecondaryContainer),
            tertiary: role_color(theme, ColorRole::Tertiary),
            tertiary_container: role_color(theme, ColorRole::TertiaryContainer),
            on_tertiary_container: role_color(theme, ColorRole::OnTertiaryContainer),
            outline: role_color(theme, ColorRole::Outline),
            outline_variant: role_color(theme, ColorRole::OutlineVariant),
            error: role_color(theme, ColorRole::Error),
            error_container: role_color(theme, ColorRole::ErrorContainer),
            on_error_container: role_color(theme, ColorRole::OnErrorContainer),
            success: role_color(theme, ColorRole::Success),
            success_container: role_color(theme, ColorRole::SuccessContainer),
            on_success_container: role_color(theme, ColorRole::OnSuccessContainer),
            warning: role_color(theme, ColorRole::Warning),
            warning_container: role_color(theme, ColorRole::WarningContainer),
            on_warning_container: role_color(theme, ColorRole::OnWarningContainer),
        }
    }
}

/// Reads the current application [`MaterialTheme`] and extracts [`ThemeTokens`].
pub fn theme_tokens(cx: &App) -> ThemeTokens {
    ThemeTokens::from_theme(gpui_material::material_theme(cx))
}

/// High-level proxy kernel runtime state rendered by status indicators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KernelHealthState {
    /// Kernel process is running and healthy.
    Running,
    /// Kernel process is starting or reloading configuration.
    Starting,
    /// Kernel process is stopped.
    Stopped,
    /// Kernel process encountered an unrecoverable error or exited unexpectedly.
    Fault,
}

impl KernelHealthState {
    /// Returns a concise default label for this health state.
    pub const fn default_label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Starting => "Starting",
            Self::Stopped => "Stopped",
            Self::Fault => "Fault",
        }
    }

    /// Returns the semantic indicator dot color for Light or Dark appearance.
    pub fn color(&self, is_dark: bool) -> Hsla {
        let scheme = ColorScheme::baseline(if is_dark {
            Appearance::Dark
        } else {
            Appearance::Light
        });
        let role = match self {
            Self::Running => ColorRole::Success,
            Self::Starting => ColorRole::Warning,
            Self::Stopped => ColorRole::OnSurfaceVariant,
            Self::Fault => ColorRole::Error,
        };
        argb_to_hsla(scheme.color(role))
    }

    /// Resolves `(dot_color, pill_background, pill_foreground)` from the active [`ThemeTokens`].
    pub const fn pill_colors(&self, tokens: &ThemeTokens) -> (Hsla, Hsla, Hsla) {
        match self {
            Self::Running => (
                tokens.success,
                tokens.success_container,
                tokens.on_success_container,
            ),
            Self::Starting => (
                tokens.warning,
                tokens.warning_container,
                tokens.on_warning_container,
            ),
            Self::Stopped => (
                tokens.on_surface_variant,
                tokens.surface_container_high,
                tokens.on_surface_variant,
            ),
            Self::Fault => (
                tokens.error,
                tokens.error_container,
                tokens.on_error_container,
            ),
        }
    }
}

/// Latency band classification for proxy delay badges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LatencyBand {
    /// Fast response (`<= 150 ms`).
    Good,
    /// Moderate response (`151..=400 ms`).
    Fair,
    /// High latency or degraded path (`> 400 ms`).
    Poor,
    /// Untested, timed out, or unreachable (`None`).
    Unavailable,
}

impl LatencyBand {
    /// Classifies an optional measured delay in milliseconds into a [`LatencyBand`].
    pub const fn from_delay_ms(delay_ms: Option<u16>) -> Self {
        match delay_ms {
            None => Self::Unavailable,
            Some(ms) if ms <= 150 => Self::Good,
            Some(ms) if ms <= 400 => Self::Fair,
            Some(_) => Self::Poor,
        }
    }

    /// Returns the semantic accent color for this latency band in Light or Dark mode.
    pub fn color(&self, is_dark: bool) -> Hsla {
        let scheme = ColorScheme::baseline(if is_dark {
            Appearance::Dark
        } else {
            Appearance::Light
        });
        let role = match self {
            Self::Good => ColorRole::Success,
            Self::Fair => ColorRole::Warning,
            Self::Poor => ColorRole::Error,
            Self::Unavailable => ColorRole::OnSurfaceVariant,
        };
        argb_to_hsla(scheme.color(role))
    }

    /// Resolves `(badge_background, badge_foreground)` from the active [`ThemeTokens`].
    pub const fn badge_colors(&self, tokens: &ThemeTokens) -> (Hsla, Hsla) {
        match self {
            Self::Good => (tokens.success_container, tokens.on_success_container),
            Self::Fair => (tokens.warning_container, tokens.on_warning_container),
            Self::Poor => (tokens.error_container, tokens.on_error_container),
            Self::Unavailable => (tokens.surface_container_high, tokens.on_surface_variant),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_mode_parses_and_formats_config_strings() {
        assert_eq!(AppearanceMode::default(), AppearanceMode::System);
        assert_eq!(
            AppearanceMode::from_config_str("light"),
            Some(AppearanceMode::Light)
        );
        assert_eq!(
            AppearanceMode::from_config_str(" DARK "),
            Some(AppearanceMode::Dark)
        );
        assert_eq!(
            AppearanceMode::from_config_str("System"),
            Some(AppearanceMode::System)
        );
        assert_eq!(AppearanceMode::from_config_str("sepia"), None);
        assert_eq!(AppearanceMode::Light.as_config_str(), "light");
        assert_eq!(AppearanceMode::Dark.as_config_str(), "dark");
        assert_eq!(AppearanceMode::System.as_config_str(), "system");
    }

    #[test]
    fn build_material_theme_resolves_appearance_and_seed() {
        let light = build_material_theme(AppearanceMode::System, Some(DEFAULT_SEED_ARGB), false);
        assert!(!is_dark_theme(&light));

        let dark = build_material_theme(AppearanceMode::System, Some(DEFAULT_SEED_ARGB), true);
        assert!(is_dark_theme(&dark));

        let baseline_dark = build_material_theme(AppearanceMode::Dark, None, false);
        assert!(is_dark_theme(&baseline_dark));
        assert_ne!(
            dark.colors.color(ColorRole::Primary),
            baseline_dark.colors.color(ColorRole::Primary)
        );
    }

    #[test]
    fn latency_band_classifies_thresholds_and_colors() {
        assert_eq!(LatencyBand::from_delay_ms(None), LatencyBand::Unavailable);
        assert_eq!(LatencyBand::from_delay_ms(Some(0)), LatencyBand::Good);
        assert_eq!(LatencyBand::from_delay_ms(Some(150)), LatencyBand::Good);
        assert_eq!(LatencyBand::from_delay_ms(Some(151)), LatencyBand::Fair);
        assert_eq!(LatencyBand::from_delay_ms(Some(400)), LatencyBand::Fair);
        assert_eq!(LatencyBand::from_delay_ms(Some(401)), LatencyBand::Poor);

        assert_ne!(
            LatencyBand::Good.color(false),
            LatencyBand::Poor.color(false)
        );
        assert_ne!(
            LatencyBand::Good.color(false),
            LatencyBand::Good.color(true)
        );
    }

    #[test]
    fn kernel_health_state_exposes_distinct_colors() {
        assert_ne!(
            KernelHealthState::Running.color(false),
            KernelHealthState::Fault.color(false)
        );
        assert_ne!(
            KernelHealthState::Starting.color(true),
            KernelHealthState::Stopped.color(true)
        );
    }
}
