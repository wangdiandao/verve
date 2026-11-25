//! Declarative 5-route navigation registry for [`crate::shell::AppShell`].

use gpui_material::Icon;
use gpui_material_symbols::Symbol;

/// Top-level route identifiers corresponding to the 5 vertical feature slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RouteId {
    /// Network takeover, active profile summary, and live traffic overview.
    #[default]
    Overview,
    /// Proxy groups, node selection, and latency checks.
    Proxies,
    /// Local and remote YAML profile management and activation.
    Profiles,
    /// Scoped Connections / Rules / Logs troubleshooting center.
    Activity,
    /// Network, service, and appearance configuration.
    Settings,
}

/// Static metadata entry describing a navigation route in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDescriptor {
    /// Strongly typed route identifier.
    pub route: RouteId,
    /// Stable string identifier used by Material navigation primitives.
    pub id: &'static str,
    /// Visible English route label.
    pub label: &'static str,
}

const ROUTES: [RouteDescriptor; 5] = [
    RouteDescriptor {
        route: RouteId::Overview,
        id: "overview",
        label: "Overview",
    },
    RouteDescriptor {
        route: RouteId::Proxies,
        id: "proxies",
        label: "Proxies",
    },
    RouteDescriptor {
        route: RouteId::Profiles,
        id: "profiles",
        label: "Profiles",
    },
    RouteDescriptor {
        route: RouteId::Activity,
        id: "activity",
        label: "Activity",
    },
    RouteDescriptor {
        route: RouteId::Settings,
        id: "settings",
        label: "Settings",
    },
];

/// Returns the ordered slice of all 5 registered application routes.
pub const fn route_registry() -> &'static [RouteDescriptor] {
    &ROUTES
}

/// Converts a Material Symbol into a theme-tinted [`gpui_material::Icon`].
pub(crate) fn symbol_icon(symbol: Symbol) -> Icon {
    let svg = symbol
        .icon()
        .to_svg()
        .expect("embedded Material Symbol must render SVG");
    Icon::svg_data(svg.into_bytes())
}

impl RouteId {
    /// All 5 routes in navigation order.
    pub const ALL: [Self; 5] = [
        Self::Overview,
        Self::Proxies,
        Self::Profiles,
        Self::Activity,
        Self::Settings,
    ];

    /// Stable string key for this route.
    pub const fn id_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Proxies => "proxies",
            Self::Profiles => "profiles",
            Self::Activity => "activity",
            Self::Settings => "settings",
        }
    }

    /// Parses a [`RouteId`] from its stable string key.
    pub fn from_id_str(id: &str) -> Option<Self> {
        match id {
            "overview" => Some(Self::Overview),
            "proxies" => Some(Self::Proxies),
            "profiles" => Some(Self::Profiles),
            "activity" => Some(Self::Activity),
            "settings" => Some(Self::Settings),
            _ => None,
        }
    }

    /// Human-readable navigation label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Proxies => "Proxies",
            Self::Profiles => "Profiles",
            Self::Activity => "Activity",
            Self::Settings => "Settings",
        }
    }

    /// Material Symbol icon for this navigation destination.
    pub fn icon(self) -> Icon {
        let symbol = match self {
            Self::Overview => Symbol::DASHBOARD,
            Self::Proxies => Symbol::PUBLIC,
            Self::Profiles => Symbol::DESCRIPTION,
            Self::Activity => Symbol::MONITORING,
            Self::Settings => Symbol::SETTINGS,
        };
        symbol_icon(symbol)
    }
}
