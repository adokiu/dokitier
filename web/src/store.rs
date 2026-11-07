use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::metrics::MetricSample;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ApplyStats {
    pub applied: usize,
    pub conflicts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkConfig {
    pub a_peer_id: u32,
    pub b_peer_id: u32,
    pub bandwidth_mbps: Option<f64>,
    pub latency_weight: Option<f64>,
    pub loss_weight: Option<f64>,
    pub bandwidth_weight: Option<f64>,
    pub disabled: bool,
    pub note: String,
    pub hlc: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticRoute {
    pub id: i64,
    pub dest_cidr: String,
    pub next_hop_peer_id: u32,
    pub metric: i64,
    pub enabled: bool,
    pub note: String,
    pub hlc: String,
}

pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    pub key: String,
    pub namespace: String,
    pub value: String,
    pub hlc: String,
    pub origin: String,
    #[serde(default)]
    pub deleted: bool,
    pub updated_at: i64,
}

fn parse_link_key(key: &str) -> Option<(u32, u32)> {
    let rest = key.strip_prefix("link:")?;
    let (a, b) = rest.split_once('|')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn hlc_greater(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> (i64, u64, &str) {
        let mut parts = v.splitn(3, ':');
        let ms = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let logical = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let origin = parts.next().unwrap_or("");
        (ms, logical, origin)
    }
    parse(a) > parse(b)
}

impl Store {
    pub async fn open(path: &str) -> anyhow::Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to open sqlite {path}"))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        let stmts = [
            "PRAGMA journal_mode=WAL",
            r#"CREATE TABLE IF NOT EXISTS sync_meta (
                key        TEXT PRIMARY KEY,
                namespace  TEXT NOT NULL,
                value      TEXT NOT NULL,
                hlc        TEXT NOT NULL,
                origin     TEXT NOT NULL,
                deleted    INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            )"#,
            "CREATE INDEX IF NOT EXISTS idx_sync_ns ON sync_meta(namespace)",
            r#"CREATE TABLE IF NOT EXISTS nodes (
                peer_id       INTEGER PRIMARY KEY,
                instance_id   TEXT UNIQUE,
                hostname      TEXT,
                ipv4          TEXT,
                ipv6          TEXT,
                version       TEXT,
                nat_type      TEXT,
                public_server INTEGER DEFAULT 0,
                first_seen    INTEGER,
                last_seen     INTEGER,
                meta_json     TEXT
            )"#,
            r#"CREATE TABLE IF NOT EXISTS links (
                a_peer_id       INTEGER NOT NULL,
                b_peer_id       INTEGER NOT NULL,
                bandwidth_mbps  REAL,
                latency_weight  REAL,
                loss_weight     REAL,
                bandwidth_weight REAL,
                disabled        INTEGER NOT NULL DEFAULT 0,
                note            TEXT NOT NULL DEFAULT '',
                hlc             TEXT NOT NULL,
                updated_at      INTEGER NOT NULL,
                PRIMARY KEY (a_peer_id, b_peer_id)
            )"#,
            r#"CREATE TABLE IF NOT EXISTS static_routes (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                dest_cidr        TEXT NOT NULL,
                next_hop_peer_id INTEGER NOT NULL,
                metric           INTEGER NOT NULL DEFAULT 100,
                enabled          INTEGER NOT NULL DEFAULT 1,
                note             TEXT NOT NULL DEFAULT '',
                hlc              TEXT NOT NULL,
                UNIQUE(dest_cidr, next_hop_peer_id)
            )"#,
            r#"CREATE TABLE IF NOT EXISTS global_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                hlc   TEXT NOT NULL
            )"#,
            r#"CREATE TABLE IF NOT EXISTS link_metrics_latest (
                a_peer_id       INTEGER NOT NULL,
                b_peer_id       INTEGER NOT NULL,
                tunnel_types    TEXT,
                latency_ms      REAL,
                loss_pct        REAL,
                rx_bps          REAL,
                tx_bps          REAL,
                bandwidth_mbps  REAL,
                source_peer_id  INTEGER,
                updated_at      INTEGER NOT NULL,
                PRIMARY KEY (a_peer_id, b_peer_id)
            )"#,
            r#"CREATE TABLE IF NOT EXISTS metrics_history (
                ts              INTEGER NOT NULL,
                a_peer_id       INTEGER NOT NULL,
                b_peer_id       INTEGER NOT NULL,
                latency_ms      REAL,
                loss_pct        REAL,
                rx_bps          REAL,
                tx_bps          REAL,
                bandwidth_mbps  REAL
            )"#,
            "CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics_history(ts)",
            "CREATE INDEX IF NOT EXISTS idx_metrics_link ON metrics_history(a_peer_id, b_peer_id, ts)",
        ];
        for stmt in stmts {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        Ok(())
    }

    pub async fn list_links(&self) -> anyhow::Result<Vec<LinkConfig>> {
        let rows = sqlx::query(
            "SELECT a_peer_id, b_peer_id, bandwidth_mbps, latency_weight, loss_weight,
                    bandwidth_weight, disabled, note, hlc, updated_at
             FROM links ORDER BY a_peer_id, b_peer_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| LinkConfig {
                a_peer_id: row.get::<i64, _>(0) as u32,
                b_peer_id: row.get::<i64, _>(1) as u32,
                bandwidth_mbps: row.get(2),
                latency_weight: row.get(3),
                loss_weight: row.get(4),
                bandwidth_weight: row.get(5),
                disabled: row.get::<i64, _>(6) != 0,
                note: row.get(7),
                hlc: row.get(8),
                updated_at: row.get(9),
            })
            .collect())
    }

    pub async fn upsert_link(&self, link: &LinkConfig) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO links
               (a_peer_id, b_peer_id, bandwidth_mbps, latency_weight, loss_weight,
                bandwidth_weight, disabled, note, hlc, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(a_peer_id, b_peer_id) DO UPDATE SET
                 bandwidth_mbps = excluded.bandwidth_mbps,
                 latency_weight = excluded.latency_weight,
                 loss_weight = excluded.loss_weight,
                 bandwidth_weight = excluded.bandwidth_weight,
                 disabled = excluded.disabled,
                 note = excluded.note,
                 hlc = excluded.hlc,
                 updated_at = excluded.updated_at"#,
        )
        .bind(link.a_peer_id as i64)
        .bind(link.b_peer_id as i64)
        .bind(link.bandwidth_mbps)
        .bind(link.latency_weight)
        .bind(link.loss_weight)
        .bind(link.bandwidth_weight)
        .bind(link.disabled as i64)
        .bind(&link.note)
        .bind(&link.hlc)
        .bind(link.updated_at)
        .execute(&self.pool)
        .await?;

        let record = SyncRecord {
            key: format!("link:{}|{}", link.a_peer_id, link.b_peer_id),
            namespace: "link".to_string(),
            value: serde_json::to_string(link)?,
            hlc: link.hlc.clone(),
            origin: link.hlc.rsplit(':').next().unwrap_or("local").to_string(),
            deleted: false,
            updated_at: link.updated_at,
        };
        self.record_sync(&record).await?;
        Ok(())
    }

    pub async fn list_static_routes(&self) -> anyhow::Result<Vec<StaticRoute>> {
        let rows = sqlx::query(
            "SELECT id, dest_cidr, next_hop_peer_id, metric, enabled, note, hlc
             FROM static_routes ORDER BY metric, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| StaticRoute {
                id: row.get(0),
                dest_cidr: row.get(1),
                next_hop_peer_id: row.get::<i64, _>(2) as u32,
                metric: row.get(3),
                enabled: row.get::<i64, _>(4) != 0,
                note: row.get(5),
                hlc: row.get(6),
            })
            .collect())
    }

    pub async fn insert_static_route(&self, route: &StaticRoute) -> anyhow::Result<i64> {
        let res = sqlx::query(
            r#"INSERT INTO static_routes
               (dest_cidr, next_hop_peer_id, metric, enabled, note, hlc)
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT(dest_cidr, next_hop_peer_id) DO UPDATE SET
                 metric = excluded.metric,
                 enabled = excluded.enabled,
                 note = excluded.note,
                 hlc = excluded.hlc"#,
        )
        .bind(&route.dest_cidr)
        .bind(route.next_hop_peer_id as i64)
        .bind(route.metric)
        .bind(route.enabled as i64)
        .bind(&route.note)
        .bind(&route.hlc)
        .execute(&self.pool)
        .await?;
        let record = SyncRecord {
            key: format!(
                "static_route:{}|{}",
                route.dest_cidr, route.next_hop_peer_id
            ),
            namespace: "static_route".to_string(),
            value: serde_json::to_string(route)?,
            hlc: route.hlc.clone(),
            origin: route.hlc.rsplit(':').next().unwrap_or("local").to_string(),
            deleted: false,
            updated_at: chrono::Utc::now().timestamp(),
        };
        self.record_sync(&record).await?;
        Ok(res.last_insert_rowid())
    }

    pub async fn delete_static_route(&self, id: i64) -> anyhow::Result<bool> {
        let existing: Option<(String, i64)> =
            sqlx::query_as("SELECT dest_cidr, next_hop_peer_id FROM static_routes WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let res = sqlx::query("DELETE FROM static_routes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if let Some((dest_cidr, next_hop_peer_id)) = existing {
            let now = chrono::Utc::now();
            let record = SyncRecord {
                key: format!("static_route:{dest_cidr}|{next_hop_peer_id}"),
                namespace: "static_route".to_string(),
                value: String::new(),
                hlc: format!("{}:0:web", now.timestamp_millis()),
                origin: "web".to_string(),
                deleted: true,
                updated_at: now.timestamp(),
            };
            self.record_sync(&record).await?;
        }
        Ok(res.rows_affected() > 0)
    }

    pub async fn get_settings(&self) -> anyhow::Result<HashMap<String, String>> {
        let rows = sqlx::query("SELECT key, value FROM global_settings")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
            .collect())
    }

    pub async fn set_setting(&self, key: &str, value: &str, hlc: &str) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO global_settings (key, value, hlc) VALUES (?, ?, ?)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value, hlc = excluded.hlc"#,
        )
        .bind(key)
        .bind(value)
        .bind(hlc)
        .execute(&self.pool)
        .await?;
        let record = SyncRecord {
            key: format!("setting:{key}"),
            namespace: "settings".to_string(),
            value: value.to_string(),
            hlc: hlc.to_string(),
            origin: hlc.rsplit(':').next().unwrap_or("local").to_string(),
            deleted: false,
            updated_at: chrono::Utc::now().timestamp(),
        };
        self.record_sync(&record).await?;
        Ok(())
    }

    pub async fn insert_metrics(&self, samples: &[MetricSample]) -> anyhow::Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for s in samples {
            sqlx::query(
                r#"INSERT INTO metrics_history
                   (ts, a_peer_id, b_peer_id, latency_ms, loss_pct, rx_bps, tx_bps, bandwidth_mbps)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(s.ts)
            .bind(s.a_peer_id as i64)
            .bind(s.b_peer_id as i64)
            .bind(s.latency_ms)
            .bind(s.loss_pct)
            .bind(s.rx_bps)
            .bind(s.tx_bps)
            .bind(s.bandwidth_mbps)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                r#"INSERT INTO link_metrics_latest
                   (a_peer_id, b_peer_id, latency_ms, loss_pct, rx_bps, tx_bps, bandwidth_mbps, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(a_peer_id, b_peer_id) DO UPDATE SET
                     latency_ms = excluded.latency_ms,
                     loss_pct = excluded.loss_pct,
                     rx_bps = excluded.rx_bps,
                     tx_bps = excluded.tx_bps,
                     bandwidth_mbps = excluded.bandwidth_mbps,
                     updated_at = excluded.updated_at"#,
            )
            .bind(s.a_peer_id as i64)
            .bind(s.b_peer_id as i64)
            .bind(s.latency_ms)
            .bind(s.loss_pct)
            .bind(s.rx_bps)
            .bind(s.tx_bps)
            .bind(s.bandwidth_mbps)
            .bind(s.ts)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// History for charts. `step_secs` buckets multiple rows together.
    pub async fn query_metrics(
        &self,
        a_peer_id: u32,
        b_peer_id: u32,
        since_ts: i64,
        step_secs: i64,
    ) -> anyhow::Result<Vec<MetricSample>> {
        let rows = sqlx::query(
            r#"SELECT (ts / ?) * ? AS bucket,
                      AVG(latency_ms), AVG(loss_pct), AVG(rx_bps), AVG(tx_bps), AVG(bandwidth_mbps)
               FROM metrics_history
               WHERE a_peer_id = ? AND b_peer_id = ? AND ts >= ?
               GROUP BY bucket ORDER BY bucket"#,
        )
        .bind(step_secs)
        .bind(step_secs)
        .bind(a_peer_id as i64)
        .bind(b_peer_id as i64)
        .bind(since_ts)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| MetricSample {
                ts: row.get(0),
                a_peer_id,
                b_peer_id,
                latency_ms: row.get(1),
                loss_pct: row.get(2),
                rx_bps: row.get(3),
                tx_bps: row.get(4),
                bandwidth_mbps: row.get(5),
            })
            .collect())
    }

    pub async fn prune_metrics(&self, before_ts: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM metrics_history WHERE ts < ?")
            .bind(before_ts)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Write a record into the sync log (called after every local mutation).
    pub async fn record_sync(&self, record: &SyncRecord) -> anyhow::Result<()> {
        sqlx::query(
            r#"INSERT INTO sync_meta (key, namespace, value, hlc, origin, deleted, updated_at)
               VALUES (?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 hlc = excluded.hlc,
                 origin = excluded.origin,
                 deleted = excluded.deleted,
                 updated_at = excluded.updated_at
               WHERE excluded.hlc > sync_meta.hlc"#,
        )
        .bind(&record.key)
        .bind(&record.namespace)
        .bind(&record.value)
        .bind(&record.hlc)
        .bind(&record.origin)
        .bind(record.deleted as i64)
        .bind(record.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Merge remote records with last-write-wins by HLC.
    pub async fn apply_sync_records(&self, records: &[SyncRecord]) -> anyhow::Result<usize> {
        Ok(self.apply_sync_records_with_stats(records).await?.applied)
    }

    pub async fn apply_sync_records_with_stats(
        &self,
        records: &[SyncRecord],
    ) -> anyhow::Result<ApplyStats> {
        let mut stats = ApplyStats::default();
        for record in records {
            let existing: Option<(String, String, bool)> = sqlx::query_as(
                "SELECT hlc, value, deleted FROM sync_meta WHERE key = ?",
            )
            .bind(&record.key)
            .fetch_optional(&self.pool)
            .await?;
            let should_apply = existing
                .as_ref()
                .map(|(hlc, _, _)| hlc_greater(&record.hlc, hlc))
                .unwrap_or(true);
            if !should_apply {
                // Same key with a different value and a non-newer HLC means the
                // two nodes edited concurrently; LWW keeps the local value.
                if let Some((_, value, deleted)) = existing
                    && !deleted
                    && !record.deleted
                    && value != record.value
                {
                    stats.conflicts += 1;
                }
                continue;
            }
            self.record_sync(record).await?;
            self.materialize_record(record).await?;
            stats.applied += 1;
        }
        Ok(stats)
    }

    async fn materialize_record(&self, record: &SyncRecord) -> anyhow::Result<()> {
        if record.deleted {
            match record.namespace.as_str() {
                "link" => {
                    if let Some((a, b)) = parse_link_key(&record.key) {
                        sqlx::query("DELETE FROM links WHERE a_peer_id = ? AND b_peer_id = ?")
                            .bind(a as i64)
                            .bind(b as i64)
                            .execute(&self.pool)
                            .await?;
                    }
                }
                "static_route" => {
                    // Tombstones carry an empty value; the key encodes the row.
                    let Some(rest) = record.key.strip_prefix("static_route:") else {
                        return Ok(());
                    };
                    if let Some((dest_cidr, next_hop)) = rest.split_once('|') {
                        sqlx::query("DELETE FROM static_routes WHERE dest_cidr = ? AND next_hop_peer_id = ?")
                            .bind(dest_cidr)
                            .bind(next_hop.parse::<i64>().unwrap_or(0))
                            .execute(&self.pool)
                            .await?;
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        match record.namespace.as_str() {
            "link" => {
                let link: LinkConfig = serde_json::from_str(&record.value)?;
                sqlx::query(
                    r#"INSERT INTO links
                       (a_peer_id, b_peer_id, bandwidth_mbps, latency_weight, loss_weight,
                        bandwidth_weight, disabled, note, hlc, updated_at)
                       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                       ON CONFLICT(a_peer_id, b_peer_id) DO UPDATE SET
                         bandwidth_mbps = excluded.bandwidth_mbps,
                         latency_weight = excluded.latency_weight,
                         loss_weight = excluded.loss_weight,
                         bandwidth_weight = excluded.bandwidth_weight,
                         disabled = excluded.disabled,
                         note = excluded.note,
                         hlc = excluded.hlc,
                         updated_at = excluded.updated_at"#,
                )
                .bind(link.a_peer_id as i64)
                .bind(link.b_peer_id as i64)
                .bind(link.bandwidth_mbps)
                .bind(link.latency_weight)
                .bind(link.loss_weight)
                .bind(link.bandwidth_weight)
                .bind(link.disabled as i64)
                .bind(&link.note)
                .bind(&link.hlc)
                .bind(link.updated_at)
                .execute(&self.pool)
                .await?;
            }
            "settings" => {
                let key = record.key.strip_prefix("setting:").unwrap_or(&record.key);
                sqlx::query(
                    r#"INSERT INTO global_settings (key, value, hlc) VALUES (?, ?, ?)
                       ON CONFLICT(key) DO UPDATE SET value = excluded.value, hlc = excluded.hlc"#,
                )
                .bind(key)
                .bind(&record.value)
                .bind(&record.hlc)
                .execute(&self.pool)
                .await?;
            }
            "static_route" => {
                let route: StaticRoute = serde_json::from_str(&record.value)?;
                sqlx::query(
                    r#"INSERT INTO static_routes
                       (dest_cidr, next_hop_peer_id, metric, enabled, note, hlc)
                       VALUES (?, ?, ?, ?, ?, ?)
                       ON CONFLICT(dest_cidr, next_hop_peer_id) DO UPDATE SET
                         metric = excluded.metric,
                         enabled = excluded.enabled,
                         note = excluded.note,
                         hlc = excluded.hlc"#,
                )
                .bind(&route.dest_cidr)
                .bind(route.next_hop_peer_id as i64)
                .bind(route.metric)
                .bind(route.enabled as i64)
                .bind(&route.note)
                .bind(&route.hlc)
                .execute(&self.pool)
                .await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn list_sync_records(&self) -> anyhow::Result<Vec<SyncRecord>> {
        let rows = sqlx::query(
            "SELECT key, namespace, value, hlc, origin, deleted, updated_at FROM sync_meta ORDER BY namespace, key",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| SyncRecord {
                key: row.get(0),
                namespace: row.get(1),
                value: row.get(2),
                hlc: row.get(3),
                origin: row.get(4),
                deleted: row.get::<i64, _>(5) != 0,
                updated_at: row.get(6),
            })
            .collect())
    }

    pub async fn sync_summary(&self) -> anyhow::Result<serde_json::Value> {
        let row = sqlx::query(
            "SELECT COUNT(*), COALESCE(MAX(updated_at), 0) FROM sync_meta WHERE deleted = 0",
        )
        .fetch_one(&self.pool)
        .await?;
        let by_ns = sqlx::query(
            "SELECT namespace, COUNT(*) FROM sync_meta WHERE deleted = 0 GROUP BY namespace",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut namespaces = serde_json::Map::new();
        for row_ns in by_ns {
            namespaces.insert(
                row_ns.get::<String, _>(0),
                serde_json::json!(row_ns.get::<i64, _>(1)),
            );
        }
        Ok(serde_json::json!({
            "records": row.get::<i64, _>(0),
            "last_updated": row.get::<i64, _>(1),
            "namespaces": namespaces,
        }))
    }

    pub async fn sync_state(&self) -> anyhow::Result<serde_json::Value> {
        let rows = sqlx::query(
            "SELECT key, namespace, hlc, origin, deleted, updated_at FROM sync_meta ORDER BY namespace, key",
        )
        .fetch_all(&self.pool)
        .await?;
        let items: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "key": row.get::<String, _>(0),
                    "namespace": row.get::<String, _>(1),
                    "hlc": row.get::<String, _>(2),
                    "origin": row.get::<String, _>(3),
                    "deleted": row.get::<i64, _>(4) != 0,
                    "updated_at": row.get::<i64, _>(5),
                })
            })
            .collect();
        Ok(serde_json::json!({ "keys": items }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("dokitier-store-{nanos}.db"))
            .to_string_lossy()
            .to_string()
    }

    #[tokio::test]
    async fn remote_link_records_materialize_and_lww() {
        let store = Store::open(&temp_db()).await.unwrap();
        let link = LinkConfig {
            a_peer_id: 1,
            b_peer_id: 2,
            bandwidth_mbps: Some(50.0),
            latency_weight: Some(2.0),
            loss_weight: Some(3.0),
            bandwidth_weight: Some(4.0),
            disabled: false,
            note: "remote".into(),
            hlc: "200:0:peer-a".into(),
            updated_at: 2,
        };
        let record = SyncRecord {
            key: "link:1|2".into(),
            namespace: "link".into(),
            value: serde_json::to_string(&link).unwrap(),
            hlc: link.hlc.clone(),
            origin: "peer-a".into(),
            deleted: false,
            updated_at: 2,
        };
        assert_eq!(store.apply_sync_records(&[record.clone()]).await.unwrap(), 1);

        let links = store.list_links().await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].bandwidth_mbps, Some(50.0));

        // Older HLC must be ignored.
        let mut older = record.clone();
        older.hlc = "100:0:peer-b".into();
        assert_eq!(store.apply_sync_records(&[older]).await.unwrap(), 0);
        assert_eq!(store.list_links().await.unwrap()[0].bandwidth_mbps, Some(50.0));

        // Tombstone removes the link.
        let mut tombstone = record.clone();
        tombstone.deleted = true;
        tombstone.hlc = "300:0:peer-c".into();
        tombstone.value = serde_json::to_string(&link).unwrap();
        assert_eq!(store.apply_sync_records(&[tombstone]).await.unwrap(), 1);
        assert!(store.list_links().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn settings_and_static_routes_materialize() {
        let store = Store::open(&temp_db()).await.unwrap();
        let setting = SyncRecord {
            key: "setting:routing.latency_weight".into(),
            namespace: "settings".into(),
            value: "9".into(),
            hlc: "10:0:a".into(),
            origin: "a".into(),
            deleted: false,
            updated_at: 1,
        };
        let route = StaticRoute {
            id: 0,
            dest_cidr: "10.9.0.0/16".into(),
            next_hop_peer_id: 7,
            metric: 10,
            enabled: true,
            note: String::new(),
            hlc: "11:0:a".into(),
        };
        let route_record = SyncRecord {
            key: "static_route:10.9.0.0/16|7".into(),
            namespace: "static_route".into(),
            value: serde_json::to_string(&route).unwrap(),
            hlc: route.hlc.clone(),
            origin: "a".into(),
            deleted: false,
            updated_at: 1,
        };
        store
            .apply_sync_records(&[setting, route_record])
            .await
            .unwrap();
        assert_eq!(
            store.get_settings().await.unwrap().get("routing.latency_weight"),
            Some(&"9".to_string())
        );
        assert_eq!(store.list_static_routes().await.unwrap().len(), 1);
    }
}

#[cfg(test)]
mod metric_tests {
    use super::*;
    use crate::metrics::MetricSample;

    #[tokio::test]
    async fn history_buckets_and_prunes() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("dokitier-metrics-{nanos}.db"))
            .to_string_lossy()
            .to_string();
        let store = Store::open(&path).await.unwrap();
        let samples: Vec<MetricSample> = [100i64, 105, 200]
            .into_iter()
            .map(|ts| MetricSample {
                ts,
                a_peer_id: 1,
                b_peer_id: 2,
                latency_ms: Some(10.0),
                loss_pct: Some(0.5),
                rx_bps: Some(1000.0),
                tx_bps: Some(500.0),
                bandwidth_mbps: None,
            })
            .collect();
        store.insert_metrics(&samples).await.unwrap();
        let buckets = store.query_metrics(1, 2, 0, 100).await.unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].latency_ms, Some(10.0));
        let _ = store.prune_metrics(150).await.unwrap();
        let remaining = store.query_metrics(1, 2, 0, 100).await.unwrap();
        assert_eq!(remaining.len(), 1);
    }
}

#[cfg(test)]
mod conflict_tests {
    use super::*;

    #[tokio::test]
    async fn older_different_value_counts_as_conflict() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("dokitier-conflict-{nanos}.db"))
            .to_string_lossy()
            .to_string();
        let store = Store::open(&path).await.unwrap();

        let make = |hlc: &str, value: &str| SyncRecord {
            key: "setting:routing.latency_weight".into(),
            namespace: "settings".into(),
            value: value.into(),
            hlc: hlc.into(),
            origin: "peer".into(),
            deleted: false,
            updated_at: 1,
        };
        store
            .apply_sync_records(&[make("200:0:a", "5")])
            .await
            .unwrap();
        let stats = store
            .apply_sync_records_with_stats(&[make("100:0:b", "9")])
            .await
            .unwrap();
        assert_eq!(stats.applied, 0);
        assert_eq!(stats.conflicts, 1);
        assert_eq!(
            store
                .get_settings()
                .await
                .unwrap()
                .get("routing.latency_weight"),
            Some(&"5".to_string())
        );
    }
}

#[cfg(test)]
mod static_route_sync_tests {
    use super::*;

    #[tokio::test]
    async fn insert_and_delete_record_sync_events() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("dokitier-static-sync-{nanos}.db"))
            .to_string_lossy()
            .to_string();
        let store = Store::open(&path).await.unwrap();
        let route = StaticRoute {
            id: 0,
            dest_cidr: "10.44.0.0/16".into(),
            next_hop_peer_id: 9,
            metric: 5,
            enabled: true,
            note: "n".into(),
            hlc: "100:0:web".into(),
        };
        let id = store.insert_static_route(&route).await.unwrap();
        let records = store.list_sync_records().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].namespace, "static_route");
        assert!(!records[0].deleted);

        assert!(store.delete_static_route(id).await.unwrap());
        let records = store.list_sync_records().await.unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].deleted);
        assert!(records[0].hlc > "100:0:web".to_string());

        // Remote tombstone (empty value) must delete the row via the key.
        let tombstone = SyncRecord {
            key: "static_route:10.44.0.0/16|9".into(),
            namespace: "static_route".into(),
            value: String::new(),
            hlc: "200:0:peer".into(),
            origin: "peer".into(),
            deleted: true,
            updated_at: 2,
        };
        store.apply_sync_records(&[tombstone]).await.unwrap();
        assert!(store.list_static_routes().await.unwrap().is_empty());
    }
}
