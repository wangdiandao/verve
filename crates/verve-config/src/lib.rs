//! Dual-domain configuration schema, forward migrations, SQLite metadata, and atomic persistence.

mod atomic;
mod error;
mod local;
mod manager;
mod schema;
mod state;
mod syncable;

pub use atomic::{atomic_write_bytes, atomic_write_str};
pub use error::ConfigError;
pub use local::LocalMachineConfig;
pub use manager::ConfigManager;
pub use schema::CURRENT_SCHEMA_VERSION;
pub use state::{ProfileUsage, StateStore};
pub use syncable::{
    DEFAULT_DELAY_TEST_URL, ProfileEntry, ProfileSource, SyncableConfig, TunConfig,
    normalize_locale,
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn syncable_and_local_configs_round_trip() {
        let syncable = SyncableConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            proxy_mode: "Global".into(),
            mixed_port: 17890,
            allow_lan: true,
            ipv6: true,
            log_level: "warn".into(),
            system_proxy_enabled: true,
            system_proxy_bypass: vec!["localhost".into(), "127.0.0.1/8".into()],
            tun_enabled: true,
            tun: TunConfig {
                stack: "GVisor".into(),
                auto_route: true,
                strict_route: false,
                dns_hijack: vec![" any:53 ".into(), "".into()],
                mtu: 1500,
                route_exclude_address: vec![" 192.168.0.0/16 ".into(), "   ".into()],
            },
            active_profile_id: Some("prof-remote".into()),
            profiles: vec![
                ProfileEntry {
                    id: "prof-local".into(),
                    name: "Local Profile".into(),
                    source: ProfileSource::Local,
                    file_name: "local.yaml".into(),
                    merge_file_name: Some("local.merge.yaml".into()),
                    script_file_name: None,
                },
                ProfileEntry {
                    id: "prof-remote".into(),
                    name: "Work Sub".into(),
                    source: ProfileSource::Remote {
                        url: "https://example.com/sub.yaml".into(),
                        update_interval_mins: Some(120),
                    },
                    file_name: "work.yaml".into(),
                    merge_file_name: None,
                    script_file_name: Some("work.js".into()),
                },
            ],
            global_merge_enabled: true,
            global_script_enabled: false,
            quick_rules: vec!["DOMAIN-SUFFIX,example.com,Proxy".into()],
            theme_mode: "Dark".into(),
            theme_seed_color: Some(0xFF1A_6585),
            locale: "en-US".into(),
            default_delay_test_url: "  https://cp.cloudflare.com/generate_204  ".into(),
        };

        let encoded = syncable
            .to_toml_string()
            .expect("syncable TOML serialization must succeed");
        let decoded = SyncableConfig::from_toml_str(&encoded)
            .expect("syncable TOML deserialization must succeed");

        assert_eq!(decoded.proxy_mode, "global");
        assert_eq!(decoded.log_level, "warning");
        assert_eq!(decoded.theme_mode, "dark");
        assert_eq!(decoded.theme_seed_color, Some(0xFF1A_6585));
        assert_eq!(
            decoded.default_delay_test_url,
            "https://cp.cloudflare.com/generate_204"
        );
        assert_eq!(decoded.tun.stack, "gvisor");
        assert_eq!(decoded.tun.dns_hijack, vec!["any:53"]);
        assert_eq!(decoded.tun.route_exclude_address, vec!["192.168.0.0/16"]);
        assert_eq!(decoded.mixed_port, 17890);
        assert_eq!(decoded.profiles, syncable.profiles);
        assert_eq!(decoded.quick_rules, syncable.quick_rules);

        let local = LocalMachineConfig {
            schema_version: CURRENT_SCHEMA_VERSION,
            mihomo_binary_path: Some(PathBuf::from("/usr/local/bin/mihomo")),
            service_mode_enabled: true,
            tun_interface_name: Some("verve-tun0".into()),
            auto_launch: true,
            window_width: Some(1280),
            window_height: Some(800),
        };

        let local_encoded = local
            .to_toml_string()
            .expect("local TOML serialization must succeed");
        let local_decoded = LocalMachineConfig::from_toml_str(&local_encoded)
            .expect("local TOML deserialization must succeed");
        assert_eq!(local_decoded, local);
    }

    #[test]
    fn schema_migration_v0_to_v1_and_future_version_rejection() {
        // Missing schema_version is treated as v0 and migrated deterministically to v1.
        let v0_missing = r#"
            proxy_mode = "DIRECT"
            mixed_port = 8899
        "#;
        let migrated =
            SyncableConfig::from_toml_str(v0_missing).expect("v0 config must migrate to v1");
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(migrated.proxy_mode, "direct");
        assert_eq!(migrated.mixed_port, 8899);
        assert_eq!(migrated.log_level, "info");
        assert_eq!(migrated.theme_mode, "system");
        assert_eq!(migrated.locale, "zh-CN");

        // Explicit schema_version = 0 also migrates cleanly.
        let v0_local = r#"
            schema_version = 0
            service_mode_enabled = true
            tun_interface_name = "  utun9  "
        "#;
        let migrated_local =
            LocalMachineConfig::from_toml_str(v0_local).expect("v0 local config must migrate");
        assert_eq!(migrated_local.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(migrated_local.service_mode_enabled);
        assert_eq!(migrated_local.tun_interface_name.as_deref(), Some("utun9"));

        // Future schema_version > CURRENT_SCHEMA_VERSION is rejected.
        let future_sync = format!("schema_version = {}\n", CURRENT_SCHEMA_VERSION + 1);
        let err = SyncableConfig::from_toml_str(&future_sync)
            .expect_err("future syncable schema must fail");
        assert!(matches!(
            err,
            ConfigError::UnsupportedSchemaVersion {
                found,
                supported: CURRENT_SCHEMA_VERSION
            } if found == CURRENT_SCHEMA_VERSION + 1
        ));

        let future_local = format!("schema_version = {}\n", CURRENT_SCHEMA_VERSION + 5);
        let err_local = LocalMachineConfig::from_toml_str(&future_local)
            .expect_err("future local schema must fail");
        assert!(matches!(
            err_local,
            ConfigError::UnsupportedSchemaVersion {
                found,
                supported: CURRENT_SCHEMA_VERSION
            } if found == CURRENT_SCHEMA_VERSION + 5
        ));
    }

    #[test]
    fn validation_rejects_invalid_enum_fields_and_unsafe_paths() {
        let bad_mode = "proxy_mode = \"invalid\"\n";
        assert!(matches!(
            SyncableConfig::from_toml_str(bad_mode),
            Err(ConfigError::InvalidField {
                field: "proxy_mode",
                ..
            })
        ));

        let mut cfg = SyncableConfig::default();
        cfg.profiles.push(ProfileEntry {
            id: "p1".into(),
            name: "Bad Path".into(),
            source: ProfileSource::Local,
            file_name: "../escape.yaml".into(),
            merge_file_name: None,
            script_file_name: None,
        });
        assert!(matches!(
            cfg.to_toml_string(),
            Err(ConfigError::InvalidField {
                field: "profiles.file_name",
                ..
            })
        ));
    }

    #[test]
    fn locale_normalization_and_validation() {
        for raw in ["zh", "zh-cn", "zh_cn", " ZH-CN ", "Zh_Cn"] {
            assert_eq!(normalize_locale(raw).unwrap(), "zh-CN");
        }
        for raw in ["en", "en-us", "en_us", " EN-US ", "En_Us"] {
            assert_eq!(normalize_locale(raw).unwrap(), "en-US");
        }

        for invalid in ["", "   ", "fr-FR", "zh-TW", "de"] {
            assert!(matches!(
                normalize_locale(invalid),
                Err(ConfigError::InvalidField {
                    field: "locale",
                    ..
                })
            ));
        }

        let cfg = SyncableConfig::from_toml_str("locale = \"en_us\"\n").unwrap();
        assert_eq!(cfg.locale, "en-US");

        assert!(matches!(
            SyncableConfig::from_toml_str("locale = \"ja-JP\"\n"),
            Err(ConfigError::InvalidField {
                field: "locale",
                ..
            })
        ));
    }

    #[test]
    fn atomic_write_replaces_existing_file_without_leftover_temps() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let target = dir.path().join("nested").join("settings.toml");

        atomic_write_str(&target, "first = 1\n").expect("initial atomic write must succeed");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first = 1\n");

        atomic_write_str(&target, "second = 2\n").expect("replacement atomic write must succeed");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second = 2\n");

        let entries: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0], "settings.toml");
    }

    #[test]
    fn state_store_enables_wal_and_persists_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let db_path = dir.path().join("state.db");

        {
            let store = StateStore::open(&db_path).expect("state store open must succeed");
            assert_eq!(store.journal_mode().unwrap(), "wal");

            store
                .upsert_profile_usage(&ProfileUsage {
                    profile_id: "p1".into(),
                    upload_bytes: 1_024,
                    download_bytes: 2_048,
                    total_bytes: Some(10_485_760),
                    expire_unix_secs: Some(1_800_000_000),
                    updated_at_unix_secs: 1_700_000_000,
                })
                .unwrap();
            store
                .upsert_profile_usage(&ProfileUsage {
                    profile_id: "p2".into(),
                    upload_bytes: 512,
                    download_bytes: 256,
                    total_bytes: None,
                    expire_unix_secs: None,
                    updated_at_unix_secs: 1_700_000_010,
                })
                .unwrap();

            store
                .set_proxy_selection("p1", "Proxy", "tokyo-01")
                .unwrap();
            store
                .set_proxy_selection("p1", "Proxy", "osaka-02")
                .unwrap();
            store
                .set_proxy_selection("p1", "Streaming", "sg-01")
                .unwrap();
            store.set_proxy_selection("p2", "Proxy", "us-01").unwrap();

            store.set_delay_ms("osaka-02", 55, 1_700_000_020).unwrap();
            store.set_delay_ms("osaka-02", 42, 1_700_000_030).unwrap();
            store.set_delay_ms("sg-01", 88, 1_700_000_035).unwrap();
        }

        // Reopen database and confirm persistence across connections.
        let reopened = StateStore::open(&db_path).expect("state store reopen must succeed");
        assert_eq!(reopened.journal_mode().unwrap(), "wal");

        let p1_usage = reopened
            .get_profile_usage("p1")
            .unwrap()
            .expect("p1 usage must persist");
        assert_eq!(p1_usage.upload_bytes, 1_024);
        assert_eq!(p1_usage.download_bytes, 2_048);
        assert_eq!(p1_usage.total_bytes, Some(10_485_760));
        assert_eq!(reopened.list_profile_usages().unwrap().len(), 2);

        let p1_selections = reopened.get_proxy_selections("p1").unwrap();
        assert_eq!(
            p1_selections.get("Proxy").map(String::as_str),
            Some("osaka-02")
        );
        assert_eq!(
            p1_selections.get("Streaming").map(String::as_str),
            Some("sg-01")
        );

        let delays = reopened.get_delay_cache().unwrap();
        assert_eq!(delays.get("osaka-02").copied(), Some(42));
        assert_eq!(delays.get("sg-01").copied(), Some(88));

        reopened.remove_profile_metadata("p1").unwrap();
        assert!(reopened.get_profile_usage("p1").unwrap().is_none());
        assert!(reopened.get_proxy_selections("p1").unwrap().is_empty());
        assert!(reopened.get_profile_usage("p2").unwrap().is_some());
    }

    #[test]
    fn config_manager_coordinates_layout_and_files() {
        let dir = tempfile::tempdir().expect("tempdir creation must succeed");
        let manager = ConfigManager::open(dir.path()).expect("config manager open must succeed");

        assert!(manager.profiles_dir().is_dir());
        assert!(manager.overrides_dir().is_dir());
        assert!(manager.runtime_dir().is_dir());

        // Missing files load as defaults.
        assert_eq!(manager.load_syncable().unwrap(), SyncableConfig::default());
        assert_eq!(manager.load_local().unwrap(), LocalMachineConfig::default());

        let syncable = SyncableConfig {
            mixed_port: 9090,
            ..Default::default()
        };
        manager.save_syncable(&syncable).unwrap();
        assert_eq!(manager.load_syncable().unwrap().mixed_port, 9090);

        let local = LocalMachineConfig {
            auto_launch: true,
            ..Default::default()
        };
        manager.save_local(&local).unwrap();
        assert!(manager.load_local().unwrap().auto_launch);

        manager
            .write_profile_yaml("sub.yaml", "mixed-port: 7890\n")
            .unwrap();
        assert_eq!(
            manager.read_profile_yaml("sub.yaml").unwrap(),
            "mixed-port: 7890\n"
        );

        manager
            .write_override_file("global.merge.yaml", "allow-lan: true\n")
            .unwrap();
        assert_eq!(
            manager.read_override_file("global.merge.yaml").unwrap(),
            "allow-lan: true\n"
        );

        let store = manager.open_state_store().unwrap();
        assert_eq!(store.journal_mode().unwrap(), "wal");
    }

    #[test]
    fn tun_config_validation_and_backwards_compatible_deserialization() {
        // Older settings.toml omitting `tun`, `theme_seed_color`, and `default_delay_test_url`
        // must deserialize cleanly using defaults.
        let legacy_toml = r#"
            schema_version = 1
            proxy_mode = "rule"
            mixed_port = 7897
            theme_mode = "system"
            locale = "zh-CN"
        "#;
        let legacy = SyncableConfig::from_toml_str(legacy_toml).expect("legacy settings must load");
        assert_eq!(legacy.tun, TunConfig::safe_defaults());
        assert_eq!(legacy.theme_seed_color, None);
        assert_eq!(legacy.default_delay_test_url, DEFAULT_DELAY_TEST_URL);

        // Invalid stack is rejected.
        let mut bad_stack = TunConfig::safe_defaults();
        bad_stack.stack = "wireguard".into();
        assert!(matches!(
            bad_stack.normalize_and_validate(),
            Err(ConfigError::InvalidField {
                field: "tun.stack",
                ..
            })
        ));

        // MTU below 1280 is rejected.
        let mut bad_mtu = TunConfig::safe_defaults();
        bad_mtu.mtu = 1279;
        assert!(matches!(
            bad_mtu.normalize_and_validate(),
            Err(ConfigError::InvalidField {
                field: "tun.mtu",
                ..
            })
        ));

        // mixed_port == 0 is rejected.
        let bad_port = SyncableConfig {
            mixed_port: 0,
            ..Default::default()
        };
        assert!(matches!(
            bad_port.to_toml_string(),
            Err(ConfigError::InvalidField {
                field: "mixed_port",
                ..
            })
        ));

        // Empty default_delay_test_url is rejected.
        let bad_url = SyncableConfig {
            default_delay_test_url: "   ".into(),
            ..Default::default()
        };
        assert!(matches!(
            bad_url.to_toml_string(),
            Err(ConfigError::InvalidField {
                field: "default_delay_test_url",
                ..
            })
        ));
    }

    #[test]
    fn remove_profile_files_deletes_profile_and_overrides_and_ignores_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ConfigManager::open(dir.path()).expect("open manager");

        manager
            .write_profile_yaml("p1.yaml", "mixed-port: 7897\n")
            .unwrap();
        manager
            .write_override_file("p1.merge.yaml", "allow-lan: true\n")
            .unwrap();
        manager
            .write_override_file("p1.script.js", "function main(c) { return c; }\n")
            .unwrap();

        let entry = ProfileEntry {
            id: "p1".into(),
            name: "Profile 1".into(),
            source: ProfileSource::Local,
            file_name: "p1.yaml".into(),
            merge_file_name: Some("p1.merge.yaml".into()),
            script_file_name: Some("p1.script.js".into()),
        };

        assert!(manager.profile_path("p1.yaml").unwrap().exists());
        assert!(manager.override_path("p1.merge.yaml").unwrap().exists());
        assert!(manager.override_path("p1.script.js").unwrap().exists());

        manager.remove_profile_files(&entry).unwrap();

        assert!(!manager.profile_path("p1.yaml").unwrap().exists());
        assert!(!manager.override_path("p1.merge.yaml").unwrap().exists());
        assert!(!manager.override_path("p1.script.js").unwrap().exists());

        // Second removal ignores ErrorKind::NotFound cleanly.
        manager.remove_profile_files(&entry).unwrap();
    }
}
