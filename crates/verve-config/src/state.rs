use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Traffic usage and expiration metadata recorded for a subscription profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileUsage {
    /// Profile identifier matching [`crate::ProfileEntry::id`].
    pub profile_id: String,
    /// Uploaded bytes reported by subscription metadata.
    pub upload_bytes: u64,
    /// Downloaded bytes reported by subscription metadata.
    pub download_bytes: u64,
    /// Total quota in bytes, if provided by the subscription.
    pub total_bytes: Option<u64>,
    /// Subscription expiration timestamp in Unix epoch seconds, if provided.
    pub expire_unix_secs: Option<i64>,
    /// Timestamp when this usage record was last refreshed (Unix epoch seconds).
    pub updated_at_unix_secs: i64,
}

/// SQLite WAL-backed store for high-frequency runtime metadata (`state.db`).
#[derive(Debug)]
pub struct StateStore {
    conn: rusqlite::Connection,
}

impl StateStore {
    /// Opens (or creates) `state.db` at `path` with WAL journaling enabled and
    /// initializes the metadata schema idempotently.
    pub fn open(path: &Path) -> Result<Self, ConfigError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let conn = rusqlite::Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;

        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Returns the active SQLite `journal_mode` pragma value (lowercased).
    pub fn journal_mode(&self) -> Result<String, ConfigError> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode.to_ascii_lowercase())
    }

    fn init_schema(&self) -> Result<(), ConfigError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS profile_usage (
                profile_id TEXT PRIMARY KEY,
                upload_bytes INTEGER NOT NULL,
                download_bytes INTEGER NOT NULL,
                total_bytes INTEGER,
                expire_unix_secs INTEGER,
                updated_at_unix_secs INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS proxy_selections (
                profile_id TEXT NOT NULL,
                group_name TEXT NOT NULL,
                node_name TEXT NOT NULL,
                PRIMARY KEY (profile_id, group_name)
            );

            CREATE TABLE IF NOT EXISTS delay_cache (
                node_name TEXT PRIMARY KEY,
                delay_ms INTEGER NOT NULL,
                tested_at_unix_secs INTEGER NOT NULL
            );",
        )?;
        Ok(())
    }

    /// Inserts or updates subscription traffic and expiration metadata for `usage.profile_id`.
    pub fn upsert_profile_usage(&self, usage: &ProfileUsage) -> Result<(), ConfigError> {
        let upload = u64_to_sql_i64("upload_bytes", usage.upload_bytes)?;
        let download = u64_to_sql_i64("download_bytes", usage.download_bytes)?;
        let total = usage
            .total_bytes
            .map(|value| u64_to_sql_i64("total_bytes", value))
            .transpose()?;

        self.conn.execute(
            "INSERT INTO profile_usage (
                profile_id,
                upload_bytes,
                download_bytes,
                total_bytes,
                expire_unix_secs,
                updated_at_unix_secs
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(profile_id) DO UPDATE SET
                upload_bytes = excluded.upload_bytes,
                download_bytes = excluded.download_bytes,
                total_bytes = excluded.total_bytes,
                expire_unix_secs = excluded.expire_unix_secs,
                updated_at_unix_secs = excluded.updated_at_unix_secs",
            params![
                usage.profile_id,
                upload,
                download,
                total,
                usage.expire_unix_secs,
                usage.updated_at_unix_secs
            ],
        )?;
        Ok(())
    }

    /// Fetches traffic and expiration metadata for `profile_id`, if present.
    pub fn get_profile_usage(&self, profile_id: &str) -> Result<Option<ProfileUsage>, ConfigError> {
        let row = self
            .conn
            .query_row(
                "SELECT profile_id, upload_bytes, download_bytes, total_bytes, expire_unix_secs, updated_at_unix_secs
                 FROM profile_usage
                 WHERE profile_id = ?1",
                params![profile_id],
                row_to_profile_usage,
            )
            .optional()?;
        Ok(row)
    }

    /// Lists all persisted profile usage records ordered by `profile_id`.
    pub fn list_profile_usages(&self) -> Result<Vec<ProfileUsage>, ConfigError> {
        let mut stmt = self.conn.prepare(
            "SELECT profile_id, upload_bytes, download_bytes, total_bytes, expire_unix_secs, updated_at_unix_secs
             FROM profile_usage
             ORDER BY profile_id ASC",
        )?;
        let rows = stmt.query_map([], row_to_profile_usage)?;
        let mut usages = Vec::new();
        for row in rows {
            usages.push(row?);
        }
        Ok(usages)
    }

    /// Deletes usage and proxy selection metadata associated with `profile_id`.
    pub fn remove_profile_metadata(&self, profile_id: &str) -> Result<(), ConfigError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM profile_usage WHERE profile_id = ?1",
            params![profile_id],
        )?;
        tx.execute(
            "DELETE FROM proxy_selections WHERE profile_id = ?1",
            params![profile_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Persists the user's selected outbound node for `(profile_id, group_name)`.
    pub fn set_proxy_selection(
        &self,
        profile_id: &str,
        group_name: &str,
        node_name: &str,
    ) -> Result<(), ConfigError> {
        self.conn.execute(
            "INSERT INTO proxy_selections (profile_id, group_name, node_name)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(profile_id, group_name) DO UPDATE SET
                node_name = excluded.node_name",
            params![profile_id, group_name, node_name],
        )?;
        Ok(())
    }

    /// Returns all remembered `(group_name -> node_name)` selections for `profile_id`.
    pub fn get_proxy_selections(
        &self,
        profile_id: &str,
    ) -> Result<BTreeMap<String, String>, ConfigError> {
        let mut stmt = self.conn.prepare(
            "SELECT group_name, node_name
             FROM proxy_selections
             WHERE profile_id = ?1
             ORDER BY group_name ASC",
        )?;
        let rows = stmt.query_map(params![profile_id], |row| {
            let group: String = row.get(0)?;
            let node: String = row.get(1)?;
            Ok((group, node))
        })?;

        let mut map = BTreeMap::new();
        for row in rows {
            let (group, node) = row?;
            map.insert(group, node);
        }
        Ok(map)
    }

    /// Inserts or updates the latest measured round-trip delay (ms) for `node_name`.
    pub fn set_delay_ms(
        &self,
        node_name: &str,
        delay_ms: u16,
        tested_at_unix_secs: i64,
    ) -> Result<(), ConfigError> {
        self.conn.execute(
            "INSERT INTO delay_cache (node_name, delay_ms, tested_at_unix_secs)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(node_name) DO UPDATE SET
                delay_ms = excluded.delay_ms,
                tested_at_unix_secs = excluded.tested_at_unix_secs",
            params![node_name, i64::from(delay_ms), tested_at_unix_secs],
        )?;
        Ok(())
    }

    /// Returns all cached `(node_name -> delay_ms)` measurements.
    pub fn get_delay_cache(&self) -> Result<BTreeMap<String, u16>, ConfigError> {
        let mut stmt = self.conn.prepare(
            "SELECT node_name, delay_ms
             FROM delay_cache
             ORDER BY node_name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let node: String = row.get(0)?;
            let raw_delay: i64 = row.get(1)?;
            let delay = u16::try_from(raw_delay).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Integer,
                    Box::new(err),
                )
            })?;
            Ok((node, delay))
        })?;

        let mut map = BTreeMap::new();
        for row in rows {
            let (node, delay) = row?;
            map.insert(node, delay);
        }
        Ok(map)
    }
}

fn row_to_profile_usage(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProfileUsage> {
    let profile_id: String = row.get(0)?;
    let upload_raw: i64 = row.get(1)?;
    let download_raw: i64 = row.get(2)?;
    let total_raw: Option<i64> = row.get(3)?;
    let expire_unix_secs: Option<i64> = row.get(4)?;
    let updated_at_unix_secs: i64 = row.get(5)?;

    let upload_bytes = sql_i64_to_u64(1, upload_raw)?;
    let download_bytes = sql_i64_to_u64(2, download_raw)?;
    let total_bytes = total_raw
        .map(|value| sql_i64_to_u64(3, value))
        .transpose()?;

    Ok(ProfileUsage {
        profile_id,
        upload_bytes,
        download_bytes,
        total_bytes,
        expire_unix_secs,
        updated_at_unix_secs,
    })
}

fn u64_to_sql_i64(field: &'static str, value: u64) -> Result<i64, ConfigError> {
    i64::try_from(value).map_err(|_| ConfigError::InvalidField {
        field,
        value: value.to_string(),
        reason: "value exceeds SQLite signed 64-bit integer range".into(),
    })
}

fn sql_i64_to_u64(column: usize, value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(err),
        )
    })
}
