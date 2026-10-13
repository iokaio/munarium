// SPDX-License-Identifier: Apache-2.0
//! Pinned retained producer records and original custody, never execution authority.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Experimental operational read profile; existing artifact/event candidates are unchanged.
pub const CHECKPOINT_PROFILE: &str = "stage2-recovery-checkpoint-v1";
const PAGE_BYTES: usize = 1024 * 1024;

fn encoded(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| invalid())
}

impl ActionLedger<'_> {
    /// Export all retained action artifacts/events at one cutoff in bounded pages.
    /// Current read authorization is required on every page; receipts and clocks are original.
    /// Completion means pagination of declared coverage, never journal/reopening completeness.
    pub async fn checkpoint(
        &self,
        admission: &ActionAdmission,
        cutoff: Option<u64>,
        after: u64,
        limit: u16,
    ) -> Result<Value> {
        self.authorize(admission, false)?;
        if !(1..=64).contains(&limit) || after > MAX {
            return Err(invalid());
        }
        let (pin, artifacts, events) = self.snapshot_at(cutoff).await?;
        if after > pin {
            return Err(invalid());
        }
        let mut records = Vec::with_capacity(artifacts.len() + events.len());
        for artifact in artifacts {
            validate_action_record(&artifact.value)?;
            let kind = text(&artifact.value["type"])?;
            let receipt = json!({"record_type":kind,"ledger_position":artifact.seq,
                "digest":action_digest(kind,&artifact.value)?});
            records.push(json!({"kind":"artifact","position":artifact.seq,
                "received_at":artifact.received_at,"record":artifact.value,"receipt":receipt}));
        }
        // These are retained producer tails, not current-source authorization assertions.
        let mut heads: BTreeMap<(String, u64), Value> = BTreeMap::new();
        for event in events {
            validate_action_record(&event.value)?;
            let id = text(&event.value["stream"]["id"])?.to_owned();
            let generation = number(&event.value["source_generation"])?;
            let sequence = number(&event.value["sequence"])?;
            let key = (id, generation);
            if heads
                .get(&key)
                .is_none_or(|old| old["sequence"].as_u64().is_none_or(|n| sequence > n))
            {
                heads.insert(
                    key,
                    json!({"stream":event.value["stream"],"source_generation":generation,
                    "producer":event.value["producer"],"sequence":sequence,
                    "event_digest":action_digest("accountability-event",&event.value)?}),
                );
            }
            let receipt = self.acknowledgement(&event.value, event.seq, event.received_at)?;
            records.push(json!({"kind":"event","position":event.seq,
                "received_at":event.received_at,"record":event.value,"receipt":receipt}));
        }
        records.sort_by_key(|entry| entry["position"].as_u64());
        if records
            .windows(2)
            .any(|pair| pair[0]["position"] == pair[1]["position"])
        {
            return Err(invalid());
        }
        let descriptor = json!({"schema_version":1,"type":"recovery-checkpoint",
            "profile":CHECKPOINT_PROFILE,"coverage":"action-artifacts-and-events",
            "scope":self.policy.scope.value(),"ledger":{"scope":self.policy.scope.value(),"kind":"ledger","id":self.version},
            "ledger_position":pin,"total_records":records.len(),"source_heads":heads.into_values().collect::<Vec<_>>(),
            "digest_profile":"sha256-delimited-canonical-v1","execution_enabled":false});
        // Delimit canonical descriptor/records individually; the complete export may exceed
        // the single-record candidate limit. JSON contains no literal NUL separators.
        let mut hash = Sha256::new();
        hash.update(b"munarium:stage2:recovery-checkpoint:v1\0");
        hash.update(encoded(&descriptor)?);
        for record in &records {
            hash.update([0]);
            hash.update(encoded(record)?);
        }
        let mut page = descriptor;
        page["checkpoint_digest"] = json!(format!("sha256:{:x}", hash.finalize()));
        page["after"] = json!(after);
        page["records"] = json!([]);
        page["complete"] = json!(false);
        page["next_after"] = Value::Null;
        let remaining: Vec<_> = records
            .iter()
            .filter(|record| record["position"].as_u64().is_some_and(|n| n > after))
            .collect();
        for record in remaining.iter().take(usize::from(limit)) {
            page["records"]
                .as_array_mut()
                .ok_or_else(invalid)?
                .push((*record).clone());
            page["next_after"] = record["position"].clone();
            if encoded(&page)?.len() > PAGE_BYTES - 32 {
                page["records"].as_array_mut().ok_or_else(invalid)?.pop();
                break;
            }
        }
        let selected = page["records"].as_array().ok_or_else(invalid)?;
        if selected.is_empty() && !remaining.is_empty() {
            return Err(invalid());
        }
        let last = selected.last().map(|record| record["position"].clone());
        let complete = selected.len() == remaining.len();
        page["complete"] = json!(complete);
        page["next_after"] = if complete {
            Value::Null
        } else {
            last.ok_or_else(invalid)?
        };
        if encoded(&page)?.len() > PAGE_BYTES {
            return Err(invalid());
        }
        Ok(page)
    }
}
