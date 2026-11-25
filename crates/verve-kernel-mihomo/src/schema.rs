//! Private Mihomo REST / WebSocket wire schemas and conversions into `verve-kernel` domain types.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize};
use verve_kernel::{
    ConnectionInfo, KernelError, LogEntry, LogLevel, MemorySample, ProxyGroup, ProxyGroupKind,
    ProxyMode, ProxyNode, ProxyTopology, RuleEntry, RuleProviderInfo, RuleSnapshot, TelemetryEvent,
    TelemetryKind, TrafficSample,
};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct VersionResponse {
    pub(crate) version: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorMessageResponse {
    #[serde(default)]
    pub(crate) message: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReloadConfigRequest<'a> {
    pub(crate) path: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct PatchModeRequest {
    pub(crate) mode: ProxyMode,
}

#[derive(Debug, Serialize)]
pub(crate) struct SelectProxyRequest<'a> {
    pub(crate) name: &'a str,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DelayResponse {
    pub(crate) delay: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProxiesResponse {
    #[serde(default)]
    proxies: BTreeMap<String, RawProxyEntry>,
}

#[derive(Debug, Deserialize)]
struct RawProxyEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    udp: bool,
    #[serde(default = "default_true")]
    alive: bool,
    #[serde(default)]
    now: Option<String>,
    #[serde(default)]
    all: Option<Vec<String>>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    history: Vec<RawDelayHistory>,
}

#[derive(Debug, Deserialize)]
struct RawDelayHistory {
    #[serde(default)]
    delay: u64,
}

impl ProxiesResponse {
    pub(crate) fn into_topology(self) -> ProxyTopology {
        let mut groups = Vec::new();
        let mut nodes = BTreeMap::new();

        for (key, raw) in self.proxies {
            let name = raw
                .name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| key.clone());

            if let Some(all) = raw.all {
                groups.push(ProxyGroup {
                    name,
                    kind: map_group_kind(&raw.kind),
                    now: raw.now.filter(|s| !s.is_empty()),
                    all,
                    hidden: raw.hidden,
                });
            } else {
                let latest_delay_ms = raw
                    .history
                    .iter()
                    .rev()
                    .find(|entry| entry.delay > 0)
                    .map(|entry| u16::try_from(entry.delay).unwrap_or(u16::MAX));

                nodes.insert(
                    name.clone(),
                    ProxyNode {
                        name,
                        protocol: raw.kind,
                        udp: raw.udp,
                        latest_delay_ms,
                        alive: raw.alive,
                    },
                );
            }
        }

        groups.sort_by(|a, b| group_sort_key(&a.name).cmp(&group_sort_key(&b.name)));

        ProxyTopology { groups, nodes }
    }
}

fn group_sort_key(name: &str) -> (u8, &str) {
    if name == "Proxy" {
        (0, name)
    } else if name == "GLOBAL" {
        (2, name)
    } else {
        (1, name)
    }
}

fn map_group_kind(raw: &str) -> ProxyGroupKind {
    match raw.to_ascii_lowercase().as_str() {
        "selector" | "select" => ProxyGroupKind::Select,
        "urltest" | "url-test" => ProxyGroupKind::UrlTest,
        "fallback" => ProxyGroupKind::Fallback,
        "loadbalance" | "load-balance" => ProxyGroupKind::LoadBalance,
        "relay" => ProxyGroupKind::Relay,
        "direct" => ProxyGroupKind::Direct,
        "reject" => ProxyGroupKind::Reject,
        _ => ProxyGroupKind::Unknown(raw.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RulesResponse {
    #[serde(default)]
    rules: Vec<RawRuleEntry>,
}

#[derive(Debug, Deserialize)]
struct RawRuleEntry {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    payload: String,
    #[serde(default)]
    proxy: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RuleProvidersResponse {
    #[serde(default)]
    providers: BTreeMap<String, RawRuleProvider>,
}

#[derive(Debug, Deserialize)]
struct RawRuleProvider {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    behavior: String,
    #[serde(rename = "vehicleType", default)]
    vehicle_type: String,
    #[serde(rename = "ruleCount", default)]
    rule_count: usize,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
}

pub(crate) fn into_rule_snapshot(
    rules_resp: RulesResponse,
    providers_resp: Option<RuleProvidersResponse>,
) -> RuleSnapshot {
    let rules = rules_resp
        .rules
        .into_iter()
        .map(|entry| RuleEntry {
            kind: entry.kind,
            payload: entry.payload,
            proxy: entry.proxy,
        })
        .collect();

    let providers = providers_resp
        .map(|resp| {
            resp.providers
                .into_iter()
                .map(|(key, provider)| RuleProviderInfo {
                    name: provider.name.filter(|s| !s.is_empty()).unwrap_or(key),
                    behavior: provider.behavior,
                    vehicle_type: provider.vehicle_type,
                    rule_count: provider.rule_count,
                    updated_at: provider.updated_at.filter(|s| !s.is_empty()),
                })
                .collect()
        })
        .unwrap_or_default();

    RuleSnapshot { rules, providers }
}

#[derive(Debug, Deserialize)]
struct TrafficFrame {
    #[serde(default)]
    up: u64,
    #[serde(default)]
    down: u64,
}

#[derive(Debug, Deserialize)]
struct MemoryFrame {
    #[serde(default)]
    inuse: u64,
}

#[derive(Debug, Deserialize)]
struct ConnectionsFrame {
    #[serde(rename = "uploadTotal", default)]
    upload_total: u64,
    #[serde(rename = "downloadTotal", default)]
    download_total: u64,
    #[serde(default)]
    connections: Option<Vec<RawConnection>>,
}

#[derive(Debug, Deserialize)]
struct RawConnection {
    #[serde(default)]
    id: String,
    #[serde(default)]
    metadata: RawConnectionMetadata,
    #[serde(default)]
    upload: u64,
    #[serde(default)]
    download: u64,
    #[serde(default)]
    start: String,
    #[serde(default)]
    chains: Vec<String>,
    #[serde(default)]
    rule: String,
    #[serde(rename = "rulePayload", default)]
    rule_payload: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawConnectionMetadata {
    #[serde(default)]
    host: String,
    #[serde(rename = "destinationIP", default)]
    destination_ip: Option<String>,
    #[serde(
        rename = "destinationPort",
        default,
        deserialize_with = "deserialize_opt_port"
    )]
    destination_port: Option<u16>,
    #[serde(rename = "remoteDestination", default)]
    remote_destination: Option<String>,
    #[serde(default)]
    network: String,
    #[serde(rename = "type", default)]
    conn_type: String,
}

#[derive(Debug, Deserialize)]
struct LogFrame {
    #[serde(rename = "type", default)]
    level: String,
    #[serde(default)]
    payload: String,
    #[serde(default)]
    time: Option<String>,
}

pub(crate) fn parse_telemetry_frame(
    kind: TelemetryKind,
    payload: &str,
) -> Result<TelemetryEvent, KernelError> {
    match kind {
        TelemetryKind::Traffic => {
            let frame: TrafficFrame = serde_json::from_str(payload).map_err(|err| {
                KernelError::Protocol(format!("invalid Mihomo traffic frame: {err}"))
            })?;
            Ok(TelemetryEvent::Traffic(TrafficSample {
                up_bps: frame.up,
                down_bps: frame.down,
            }))
        }
        TelemetryKind::Memory => {
            let frame: MemoryFrame = serde_json::from_str(payload).map_err(|err| {
                KernelError::Protocol(format!("invalid Mihomo memory frame: {err}"))
            })?;
            Ok(TelemetryEvent::Memory(MemorySample {
                inuse_bytes: frame.inuse,
            }))
        }
        TelemetryKind::Connections => {
            let frame: ConnectionsFrame = serde_json::from_str(payload).map_err(|err| {
                KernelError::Protocol(format!("invalid Mihomo connections frame: {err}"))
            })?;
            let connections = frame
                .connections
                .unwrap_or_default()
                .into_iter()
                .map(|conn| {
                    let destination_ip = conn.metadata.destination_ip.filter(|s| !s.is_empty());
                    let remote_dest = conn.metadata.remote_destination.filter(|s| !s.is_empty());
                    let host = if !conn.metadata.host.is_empty() {
                        conn.metadata.host
                    } else {
                        destination_ip.clone().or(remote_dest).unwrap_or_default()
                    };
                    let start_unix_ms = parse_rfc3339_to_unix_ms(&conn.start).unwrap_or(0);

                    ConnectionInfo {
                        id: conn.id,
                        host,
                        destination_ip,
                        destination_port: conn.metadata.destination_port,
                        network: conn.metadata.network,
                        conn_type: conn.metadata.conn_type,
                        rule: conn.rule,
                        rule_payload: conn.rule_payload,
                        chains: conn.chains,
                        upload_bytes: conn.upload,
                        download_bytes: conn.download,
                        start_unix_ms,
                    }
                })
                .collect();

            Ok(TelemetryEvent::Connections {
                upload_total: frame.upload_total,
                download_total: frame.download_total,
                connections,
            })
        }
        TelemetryKind::Logs => {
            let frame: LogFrame = serde_json::from_str(payload)
                .map_err(|err| KernelError::Protocol(format!("invalid Mihomo log frame: {err}")))?;
            let timestamp_unix_ms = frame
                .time
                .as_deref()
                .and_then(parse_rfc3339_to_unix_ms)
                .unwrap_or_else(now_unix_ms);

            Ok(TelemetryEvent::Log(LogEntry {
                level: map_log_level(&frame.level),
                payload: frame.payload,
                timestamp_unix_ms,
            }))
        }
    }
}

fn map_log_level(raw: &str) -> LogLevel {
    match raw.to_ascii_lowercase().as_str() {
        "debug" => LogLevel::Debug,
        "info" => LogLevel::Info,
        "warn" | "warning" => LogLevel::Warning,
        "error" | "err" | "fatal" | "panic" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn now_unix_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn deserialize_opt_port<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PortValue {
        Num(u16),
        Str(String),
    }

    let opt = Option::<PortValue>::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(PortValue::Num(port)) => Ok((port > 0).then_some(port)),
        Some(PortValue::Str(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            match trimmed.parse::<u16>() {
                Ok(port) if port > 0 => Ok(Some(port)),
                Ok(_) => Ok(None),
                Err(_) => Ok(None),
            }
        }
    }
}

/// Parses an RFC 3339 timestamp string into Unix epoch milliseconds.
fn parse_rfc3339_to_unix_ms(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.len() < 20 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let minute: i64 = s[14..16].parse().ok()?;
    let second: i64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let mut idx = 19;
    let mut millis: i64 = 0;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac = &s[frac_start..idx];
        let mut factor = 100;
        for ch in frac.bytes().take(3) {
            millis += i64::from(ch - b'0') * factor;
            factor /= 10;
        }
    }

    let tz_str = &s[idx..];
    let offset_secs: i64 = if tz_str.eq_ignore_ascii_case("z") {
        0
    } else if tz_str.len() == 6
        && (tz_str.starts_with('+') || tz_str.starts_with('-'))
        && tz_str.as_bytes()[3] == b':'
    {
        let sign = if tz_str.starts_with('+') { 1 } else { -1 };
        let off_h: i64 = tz_str[1..3].parse().ok()?;
        let off_m: i64 = tz_str[4..6].parse().ok()?;
        sign * (off_h * 3600 + off_m * 60)
    } else {
        return None;
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs;
    let total_ms = secs.checked_mul(1_000)?.checked_add(millis)?;
    u64::try_from(total_ms).ok()
}

/// Howard Hinnant's civil-from-days algorithm returning days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_proxies_groups_nodes_and_sorts_deterministically() {
        let json = r#"{
            "proxies": {
                "GLOBAL": {
                    "name": "GLOBAL",
                    "type": "Selector",
                    "now": "Proxy",
                    "all": ["Proxy", "DIRECT"],
                    "hidden": false
                },
                "Auto": {
                    "name": "Auto",
                    "type": "URLTest",
                    "now": "香港 01",
                    "all": ["香港 01", "tokyo-01"],
                    "hidden": true
                },
                "Proxy": {
                    "name": "Proxy",
                    "type": "Select",
                    "now": "香港 01",
                    "all": ["Auto", "香港 01", "DIRECT"]
                },
                "香港 01": {
                    "name": "香港 01",
                    "type": "VLESS",
                    "udp": true,
                    "history": [
                        {"delay": 88},
                        {"delay": 0}
                    ]
                },
                "tokyo-01": {
                    "name": "tokyo-01",
                    "type": "Trojan",
                    "udp": false,
                    "alive": false,
                    "history": [
                        {"delay": 55},
                        {"delay": 42}
                    ]
                }
            }
        }"#;

        let resp: ProxiesResponse = serde_json::from_str(json).expect("valid proxies JSON");
        let topology = resp.into_topology();

        let group_names: Vec<&str> = topology.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(group_names, vec!["Proxy", "Auto", "GLOBAL"]);
        assert_eq!(topology.groups[0].kind, ProxyGroupKind::Select);
        assert_eq!(topology.groups[1].kind, ProxyGroupKind::UrlTest);
        assert!(topology.groups[1].hidden);

        let hk = &topology.nodes["香港 01"];
        assert_eq!(hk.protocol, "VLESS");
        assert!(hk.udp);
        assert!(hk.alive);
        assert_eq!(hk.latest_delay_ms, Some(88));

        let tokyo = &topology.nodes["tokyo-01"];
        assert!(!tokyo.alive);
        assert_eq!(tokyo.latest_delay_ms, Some(42));
    }

    #[test]
    fn maps_group_kinds_case_insensitively() {
        assert_eq!(map_group_kind("selector"), ProxyGroupKind::Select);
        assert_eq!(map_group_kind("Select"), ProxyGroupKind::Select);
        assert_eq!(map_group_kind("URLTest"), ProxyGroupKind::UrlTest);
        assert_eq!(map_group_kind("Fallback"), ProxyGroupKind::Fallback);
        assert_eq!(map_group_kind("LoadBalance"), ProxyGroupKind::LoadBalance);
        assert_eq!(map_group_kind("Relay"), ProxyGroupKind::Relay);
        assert_eq!(map_group_kind("Direct"), ProxyGroupKind::Direct);
        assert_eq!(map_group_kind("Reject"), ProxyGroupKind::Reject);
        assert_eq!(
            map_group_kind("CustomStrategy"),
            ProxyGroupKind::Unknown("CustomStrategy".into())
        );
    }

    #[test]
    fn parses_rfc3339_timestamps_and_connections_frame() {
        assert_eq!(
            parse_rfc3339_to_unix_ms("2023-11-14T22:13:20.250Z"),
            Some(1_700_000_000_250)
        );
        assert_eq!(
            parse_rfc3339_to_unix_ms("2023-11-15T06:13:20.250+08:00"),
            Some(1_700_000_000_250)
        );

        let frame = r#"{
            "uploadTotal": 100,
            "downloadTotal": 200,
            "connections": [{
                "id": "c-1",
                "metadata": {
                    "network": "tcp",
                    "type": "HTTP",
                    "destinationIP": "1.1.1.1",
                    "destinationPort": "443",
                    "host": "example.com"
                },
                "upload": 10,
                "download": 20,
                "start": "2023-11-14T22:13:20.000Z",
                "chains": ["Proxy", "hk-01"],
                "rule": "MATCH",
                "rulePayload": ""
            }]
        }"#;

        let event = parse_telemetry_frame(TelemetryKind::Connections, frame)
            .expect("connections frame parses");
        match event {
            TelemetryEvent::Connections {
                upload_total,
                download_total,
                connections,
            } => {
                assert_eq!(upload_total, 100);
                assert_eq!(download_total, 200);
                assert_eq!(connections.len(), 1);
                assert_eq!(connections[0].destination_port, Some(443));
                assert_eq!(connections[0].start_unix_ms, 1_700_000_000_000);
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }
}
