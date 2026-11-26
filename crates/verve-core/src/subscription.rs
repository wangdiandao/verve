//! Remote subscription fetching, response header parsing, and YAML validation.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use percent_encoding::percent_decode_str;

use crate::error::CoreError;

const SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(20);
const SUBSCRIPTION_USER_AGENT: &str = "Verve/0.0.3 (clash.meta)";
const MAX_PROFILE_ID_LEN: usize = 32;

/// Parsed traffic usage and expiration metadata from the `subscription-userinfo` response header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscriptionUserInfo {
    /// Uploaded bytes reported by the subscription provider.
    pub upload_bytes: u64,
    /// Downloaded bytes reported by the subscription provider.
    pub download_bytes: u64,
    /// Total quota allowance in bytes, if finite and reported.
    pub total_bytes: Option<u64>,
    /// Subscription expiration Unix timestamp in seconds, if finite and reported.
    pub expire_unix_secs: Option<i64>,
}

/// Result of fetching and validating a remote YAML subscription profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedSubscription {
    /// Validated raw YAML profile document.
    pub raw_yaml: String,
    /// Optional suggested display name extracted from HTTP headers or URL.
    pub suggested_name: Option<String>,
    /// Optional subscription traffic/expiration metadata extracted from `subscription-userinfo`.
    pub usage: Option<SubscriptionUserInfo>,
}

/// Async abstraction for downloading and validating remote subscription profiles.
pub trait SubscriptionFetcher: Send + Sync {
    /// Fetches a remote subscription document from `url`, parses metadata headers, and validates YAML.
    fn fetch<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FetchedSubscription, CoreError>> + Send + 'a>>;
}

/// Production [`SubscriptionFetcher`] backed by `reqwest` over `rustls-tls`.
#[derive(Debug, Clone)]
pub struct ReqwestSubscriptionFetcher {
    client: reqwest::Client,
}

impl Default for ReqwestSubscriptionFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestSubscriptionFetcher {
    /// Creates a new [`ReqwestSubscriptionFetcher`] with a 20s timeout and Mihomo-compatible User-Agent.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(SUBSCRIPTION_TIMEOUT)
            .user_agent(SUBSCRIPTION_USER_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

impl SubscriptionFetcher for ReqwestSubscriptionFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<FetchedSubscription, CoreError>> + Send + 'a>> {
        Box::pin(async move {
            let trimmed = url.trim();
            if !is_supported_subscription_url(trimmed) {
                return Err(CoreError::InvalidInput(
                    "subscription URL must start with http:// or https://".into(),
                ));
            }

            let response =
                self.client.get(trimmed).send().await.map_err(|err| {
                    CoreError::Subscription(format!("HTTP request failed: {err}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                return Err(CoreError::Subscription(format!(
                    "subscription server returned HTTP {status}"
                )));
            }

            let headers = response.headers();
            let userinfo_header = header_to_str(headers, "subscription-userinfo");
            let profile_title = header_to_str(headers, "profile-title");
            let content_disposition = header_to_str(headers, "content-disposition");

            let usage = userinfo_header.and_then(parse_subscription_userinfo);
            let suggested_name = infer_profile_name(trimmed, profile_title, content_disposition);

            let raw_yaml = response
                .text()
                .await
                .map_err(|err| CoreError::Subscription(format!("failed to read body: {err}")))?;

            validate_profile_yaml(&raw_yaml)?;

            Ok(FetchedSubscription {
                raw_yaml,
                suggested_name,
                usage,
            })
        })
    }
}

fn header_to_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|val| val.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn is_supported_subscription_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") && url.len() > "http://".len())
        || (lower.starts_with("https://") && url.len() > "https://".len())
}

/// Parses a Clash/Mihomo `subscription-userinfo` HTTP header value.
///
/// Expects semicolon-separated `key=value` tokens such as
/// `"upload=123; download=456; total=1073741824; expire=1800000000"`.
pub fn parse_subscription_userinfo(header: &str) -> Option<SubscriptionUserInfo> {
    let mut upload_bytes = 0u64;
    let mut download_bytes = 0u64;
    let mut total_bytes: Option<u64> = None;
    let mut expire_unix_secs: Option<i64> = None;
    let mut matched_any = false;

    for part in header.split(';') {
        let Some((raw_key, raw_val)) = part.split_once('=') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let val = raw_val.trim();
        match key.as_str() {
            "upload" => {
                if let Ok(parsed) = val.parse::<u64>() {
                    upload_bytes = parsed;
                    matched_any = true;
                }
            }
            "download" => {
                if let Ok(parsed) = val.parse::<u64>() {
                    download_bytes = parsed;
                    matched_any = true;
                }
            }
            "total" => {
                if let Ok(parsed) = val.parse::<u64>() {
                    total_bytes = (parsed > 0).then_some(parsed);
                    matched_any = true;
                }
            }
            "expire" => {
                if let Ok(parsed) = val.parse::<i64>() {
                    expire_unix_secs = (parsed > 0).then_some(parsed);
                    matched_any = true;
                }
            }
            _ => {}
        }
    }

    matched_any.then_some(SubscriptionUserInfo {
        upload_bytes,
        download_bytes,
        total_bytes,
        expire_unix_secs,
    })
}

/// Infers a human-readable profile name from `profile-title`, `content-disposition`, or `url`.
pub fn infer_profile_name(
    url: &str,
    profile_title: Option<&str>,
    content_disposition: Option<&str>,
) -> Option<String> {
    if let Some(title) = profile_title.and_then(decode_profile_title) {
        return Some(title);
    }

    if let Some(filename) = content_disposition.and_then(parse_content_disposition_filename) {
        let cleaned = strip_yaml_extension(&filename);
        if !cleaned.is_empty() {
            return Some(cleaned.to_owned());
        }
    }

    infer_name_from_url(url)
}

fn decode_profile_title(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(encoded) = trimmed
        .strip_prefix("base64:")
        .or_else(|| trimmed.strip_prefix("BASE64:"))
    {
        if let Some(decoded) = decode_base64_utf8(encoded.trim()) {
            let clean = decoded.trim();
            if !clean.is_empty() {
                return Some(clean.to_owned());
            }
        }
        return None;
    }

    Some(trimmed.to_owned())
}

fn decode_base64_utf8(input: &str) -> Option<String> {
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);
    let mut accum: u32 = 0;
    let mut bits: u8 = 0;

    for byte in input.bytes() {
        if byte.is_ascii_whitespace() || byte == b'=' {
            continue;
        }
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        accum = (accum << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push(((accum >> bits) & 0xFF) as u8);
        }
    }

    String::from_utf8(buf).ok()
}

fn parse_content_disposition_filename(header: &str) -> Option<String> {
    let mut fallback: Option<String> = None;

    for part in header.split(';').skip(1) {
        let Some((raw_key, raw_val)) = part.split_once('=') else {
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let val = raw_val.trim();

        if key == "filename*" {
            let unquoted = val.trim_matches('"');
            let encoded = unquoted.split_once("''").map_or(unquoted, |(_, rhs)| rhs);
            let decoded = percent_decode_str(encoded)
                .decode_utf8_lossy()
                .trim()
                .to_owned();
            if !decoded.is_empty() {
                return Some(decoded);
            }
        } else if key == "filename" {
            let unquoted = val.trim_matches('"').trim();
            if !unquoted.is_empty() {
                let decoded = percent_decode_str(unquoted)
                    .decode_utf8_lossy()
                    .trim()
                    .to_owned();
                if !decoded.is_empty() {
                    fallback = Some(decoded);
                }
            }
        }
    }

    fallback
}

fn infer_name_from_url(url: &str) -> Option<String> {
    let without_scheme = url
        .trim()
        .split_once("://")
        .map_or(url.trim(), |(_, rest)| rest);
    let without_query = without_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(without_scheme);

    let mut segments = without_query.split('/');
    let host_port = segments.next()?.trim();

    let last_segment = segments.rfind(|seg| !seg.trim().is_empty()).map(|seg| {
        percent_decode_str(seg)
            .decode_utf8_lossy()
            .trim()
            .to_owned()
    });

    if let Some(seg) = last_segment {
        let cleaned = strip_yaml_extension(&seg);
        if !cleaned.is_empty() {
            return Some(cleaned.to_owned());
        }
    }

    let host = host_port
        .rsplit_once('@')
        .map_or(host_port, |(_, h)| h)
        .split(':')
        .next()
        .unwrap_or(host_port)
        .trim();
    (!host.is_empty()).then(|| host.to_owned())
}

fn strip_yaml_extension(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.len() >= 5 && trimmed[trimmed.len() - 5..].eq_ignore_ascii_case(".yaml") {
        trimmed[..trimmed.len() - 5].trim()
    } else if trimmed.len() >= 4 && trimmed[trimmed.len() - 4..].eq_ignore_ascii_case(".yml") {
        trimmed[..trimmed.len() - 4].trim()
    } else {
        trimmed
    }
}

/// Validates that `raw_yaml` is non-empty and parses as a YAML mapping document.
pub fn validate_profile_yaml(raw_yaml: &str) -> Result<(), CoreError> {
    if raw_yaml.trim().is_empty() {
        return Err(CoreError::Subscription(
            "subscription response body is empty".into(),
        ));
    }

    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(raw_yaml).map_err(|err| {
        CoreError::Subscription(format!("invalid YAML subscription document: {err}"))
    })?;

    if !parsed.is_mapping() {
        return Err(CoreError::Subscription(
            "subscription YAML root must be a mapping".into(),
        ));
    }

    Ok(())
}

/// Generates a filesystem-safe, unique lowercase profile identifier derived from `name_or_url`.
pub fn generate_profile_id(name_or_url: &str, existing_ids: &[&str]) -> String {
    let mut slug = String::new();
    let mut prev_dash = true;

    for ch in name_or_url.chars() {
        if slug.len() >= MAX_PROFILE_ID_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }

    let base = slug.trim_matches('-');
    let base = if base.is_empty() { "profile" } else { base };

    if !existing_ids.contains(&base) {
        return base.to_owned();
    }

    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !existing_ids.contains(&candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_subscription_userinfo_header() {
        let parsed = parse_subscription_userinfo(
            "upload=1048576; download=2097152; total=10737418240; expire=1800000000",
        )
        .expect("valid userinfo must parse");
        assert_eq!(parsed.upload_bytes, 1_048_576);
        assert_eq!(parsed.download_bytes, 2_097_152);
        assert_eq!(parsed.total_bytes, Some(10_737_418_240));
        assert_eq!(parsed.expire_unix_secs, Some(1_800_000_000));

        let unlimited = parse_subscription_userinfo("upload=10; download=20; total=0; expire=0")
            .expect("zero total/expire must parse as None");
        assert_eq!(unlimited.upload_bytes, 10);
        assert_eq!(unlimited.download_bytes, 20);
        assert_eq!(unlimited.total_bytes, None);
        assert_eq!(unlimited.expire_unix_secs, None);

        assert_eq!(parse_subscription_userinfo("unrelated=abc"), None);
    }

    #[test]
    fn infers_profile_name_with_header_and_url_fallbacks() {
        assert_eq!(
            infer_profile_name(
                "https://sub.example.com/get",
                Some("My Provider"),
                Some("attachment; filename=\"ignored.yaml\"")
            ),
            Some("My Provider".into())
        );

        assert_eq!(
            infer_profile_name(
                "https://sub.example.com/get",
                Some("base64:VmVydmUgU3Vi"),
                None
            ),
            Some("Verve Sub".into())
        );

        assert_eq!(
            infer_profile_name(
                "https://sub.example.com/get",
                None,
                Some("attachment; filename*=UTF-8''Work%20Nodes.yaml")
            ),
            Some("Work Nodes".into())
        );

        assert_eq!(
            infer_profile_name(
                "https://sub.example.com/api/v1/hk-fast.yml?token=secret",
                None,
                None
            ),
            Some("hk-fast".into())
        );

        assert_eq!(
            infer_profile_name("https://sub.example.com/?token=123", None, None),
            Some("sub.example.com".into())
        );
    }

    #[test]
    fn validates_profile_yaml_mapping() {
        assert!(validate_profile_yaml("mixed-port: 7897\nrules:\n  - MATCH,DIRECT\n").is_ok());
        assert!(validate_profile_yaml("   \n").is_err());
        assert!(validate_profile_yaml("- just\n- a\n- sequence\n").is_err());
        assert!(validate_profile_yaml("proxies:\n  - [unclosed\n").is_err());
    }

    #[test]
    fn generates_safe_unique_profile_ids() {
        assert_eq!(
            generate_profile_id("Work VPN (Tokyo)", &[]),
            "work-vpn-tokyo"
        );
        assert_eq!(
            generate_profile_id("Work VPN (Tokyo)", &["work-vpn-tokyo"]),
            "work-vpn-tokyo-2"
        );
        assert_eq!(
            generate_profile_id("Work VPN (Tokyo)", &["work-vpn-tokyo", "work-vpn-tokyo-2"]),
            "work-vpn-tokyo-3"
        );
        assert_eq!(generate_profile_id("中文订阅", &[]), "profile");
        assert_eq!(generate_profile_id("中文订阅", &["profile"]), "profile-2");
    }
}
