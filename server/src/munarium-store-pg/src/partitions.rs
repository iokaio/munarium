// SPDX-License-Identifier: Apache-2.0
//! ledger_events partition maintenance (2026-08-17). Migration 0002 ships
//! `ledger_events` range-partitioned on `tenant_seq` with one bounded
//! partition (p0: [0, 10M)) and a DEFAULT partition, and promised
//! maintenance "as an mmctl command" that never existed. The honest
//! mechanics changed the shape of the fix: because a DEFAULT partition
//! exists, overflow never fails an insert — but once rows land in the
//! default for a range, `CREATE TABLE ... PARTITION OF ... FOR VALUES`
//! over that range fails with an overlap error and recovery is a manual
//! row-movement procedure (docs/ops/clustering.md). So maintenance must
//! run BEFORE overflow: this task creates the next 10M-wide partition when
//! the high-water mark comes within `HEADROOM` of the current top bound.
//!
//! Cluster-safe by construction: the DDL runs under
//! `pg_advisory_xact_lock(hashtext('munarium:partition-ddl'))` (the same
//! runtime-DDL idiom as collection partitions in munarium-retrieval-pg), so N
//! instances' concurrent sweeps serialize and `IF NOT EXISTS` makes the
//! loser a no-op. The server spawns one sweep at startup and one per day.

use crate::storage_err;
use munarium_core::{KernelError, Result};
use sqlx::{PgPool, Row};

pub const PARTITION_WIDTH: i64 = 10_000_000;
/// Create the next partition when max(tenant_seq) is within this many rows
/// of the top bounded partition's upper bound.
pub const HEADROOM: i64 = 2_000_000;

/// One maintenance sweep. Returns the name of the partition it created, or
/// None when there was nothing to do.
pub async fn ensure_ledger_partitions(pool: &PgPool) -> Result<Option<String>> {
    let mut tx = pool.begin().await.map_err(storage_err)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('munarium:partition-ddl'))")
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;

    // Bounded partitions of ledger_events, by catalog: name + bound expr,
    // e.g. "FOR VALUES FROM ('0') TO ('10000000')"; the default partition
    // renders as "DEFAULT".
    let rows = sqlx::query(
        "SELECT c.relname AS name, pg_get_expr(c.relpartbound, c.oid) AS bound
           FROM pg_inherits i
           JOIN pg_class c ON c.oid = i.inhrelid
           JOIN pg_class p ON p.oid = i.inhparent
          WHERE p.relname = 'ledger_events'",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(storage_err)?;

    let mut top_upper: i64 = 0;
    for row in &rows {
        let bound: String = row.get("bound");
        if let Some(upper) = parse_upper_bound(&bound) {
            top_upper = top_upper.max(upper);
        }
    }
    if top_upper == 0 {
        return Err(KernelError::Storage(
            "ledger_events has no bounded partitions — migration 0002 did not apply?".into(),
        ));
    }

    let (max_seq,): (Option<i64>,) = sqlx::query_as("SELECT max(tenant_seq) FROM ledger_events")
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_err)?;
    let max_seq = max_seq.unwrap_or(0);
    if max_seq <= top_upper - HEADROOM {
        tx.commit().await.map_err(storage_err)?;
        return Ok(None);
    }

    let name = format!("ledger_events_p{}", top_upper / PARTITION_WIDTH);
    let ddl = format!(
        "CREATE TABLE IF NOT EXISTS {name} PARTITION OF ledger_events \
         FOR VALUES FROM ({top_upper}) TO ({})",
        top_upper + PARTITION_WIDTH
    );
    if let Err(e) = sqlx::query(&ddl).execute(&mut *tx).await {
        // The overlap case: rows already sat in ledger_events_default for
        // this range (the sweep started too late, or a pre-sweep binary ran
        // long). This is the one manual procedure — say so loudly.
        return Err(KernelError::Storage(format!(
            "creating {name} failed ({e}); if this is a partition-overlap error, rows have \
             already overflowed into ledger_events_default and need the manual row-movement \
             procedure in docs/ops/clustering.md"
        )));
    }
    tx.commit().await.map_err(storage_err)?;
    Ok(Some(name))
}

fn parse_upper_bound(bound: &str) -> Option<i64> {
    // "FOR VALUES FROM ('0') TO ('10000000')" -> 10000000; "DEFAULT" -> None
    let to = bound.split(" TO (").nth(1)?;
    to.trim_end_matches(')')
        .trim()
        .trim_matches('\'')
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_parsing_handles_catalog_forms() {
        assert_eq!(
            parse_upper_bound("FOR VALUES FROM ('0') TO ('10000000')"),
            Some(10_000_000)
        );
        assert_eq!(
            parse_upper_bound("FOR VALUES FROM (10000000) TO (20000000)"),
            Some(20_000_000)
        );
        assert_eq!(parse_upper_bound("DEFAULT"), None);
    }
}
