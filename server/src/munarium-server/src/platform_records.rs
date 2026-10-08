// SPDX-License-Identifier: Apache-2.0
//! Protected platform records: current signed identity, mTLS peer, and separate ledger custody.
use crate::{error::ApiError, platform_api::AuthenticatedPeer, state::AppState};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use munarium_core::{
    platform::{EventLedger, RecorderContext},
    platform_actions::{ActionAdmission, ActionLedger, ActionPolicy},
    storage::StorageBackend,
    KernelError,
};
use munarium_store_mem::MemStore;
use munarium_store_pg::PgStore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn denied() -> KernelError {
    KernelError::Forbidden("platform record authority unavailable".into())
}
fn storage(_: sqlx::Error) -> KernelError {
    KernelError::Storage("platform record storage unavailable".into())
}
pub struct Records {
    store: Arc<dyn StorageBackend>,
    version: String,
}
impl Records {
    pub async fn open(
        tenant: &str,
        deployment: &str,
        postgres: Option<&PgStore>,
    ) -> Result<Self, KernelError> {
        let Some(pg) = postgres else {
            let store = Arc::new(MemStore::new());
            let version = store.create_version(None, None).await?;
            return Ok(Self { store, version });
        };
        let physical = format!(
            "platform-records:{:x}",
            Sha256::digest(format!("{deployment}\0{tenant}").as_bytes())
        );
        let store = pg.with_tenant(&physical).await?;
        // Serialize first creation across processes. No authority update can change the mapping.
        let mut tx = pg.pool().begin().await.map_err(storage)?;
        sqlx::query("SELECT tenant_id FROM platform_authority WHERE tenant_id=$1 FOR UPDATE")
            .bind(tenant)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
        let prior: Option<(String, String)> = sqlx::query_as(
            "SELECT storage_tenant,version_id FROM platform_record_ledgers WHERE tenant_id=$1",
        )
        .bind(tenant)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let version = if let Some((stored, version)) = prior {
            if stored != physical {
                return Err(denied());
            }
            store.head(&version).await?;
            version
        } else {
            let version = store.create_version(None, None).await?;
            sqlx::query("INSERT INTO platform_record_ledgers(tenant_id,storage_tenant,version_id) VALUES($1,$2,$3)")
                .bind(tenant).bind(&physical).bind(&version).execute(&mut *tx).await.map_err(storage)?;
            version
        };
        tx.commit().await.map_err(storage)?;
        Ok(Self {
            store: Arc::new(store),
            version,
        })
    }
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    SourceHead,
    Append { event: String },
    Lookup { operation_id: String },
    Archive { bundle: String },
    Replay { operation_id: String },
    ActionArchive { record: String },
    ActionAppend { event: String },
    ActionLookup { operation_id: String },
    ActionTransition { transition_id: String },
    ActionSourceHead { stream_id: String, generation: u64 },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    chain: Vec<String>,
    action: Operation,
}

#[utoipa::path(post,path="/v1/platform/{tenant}/records",params(("tenant"=String,Path)),request_body=Value,
    security(("platformMtls"=[])),responses((status=200,body=Value),(status=403,description="identity or record authority refused"),
    (status=409,description="immutable operation conflict")),tag="platform")]
pub async fn platform_records(
    State(state): State<Arc<AppState>>,
    Path(tenant): Path<String>,
    peer: Option<Extension<AuthenticatedPeer>>,
    crate::rest::ProblemJson(request): crate::rest::ProblemJson<Request>,
) -> Result<Json<Value>, ApiError> {
    let peer = peer.ok_or_else(denied)?.0;
    let write = matches!(
        &request.action,
        Operation::Append { .. }
            | Operation::Archive { .. }
            | Operation::ActionArchive { .. }
            | Operation::ActionAppend { .. }
    );
    let action_record = matches!(
        &request.action,
        Operation::ActionArchive { .. }
            | Operation::ActionAppend { .. }
            | Operation::ActionLookup { .. }
            | Operation::ActionTransition { .. }
            | Operation::ActionSourceHead { .. }
    );
    let runtime = state.platform.as_ref().ok_or_else(denied)?;
    let authority = runtime.tenant(&peer, &tenant, if write { "record" } else { "read" })?;
    let _serial = authority.serial.lock().await;
    let (_fence, snapshot) = authority.fenced_snapshot().await?;
    let now = chrono::Utc::now().timestamp();
    let principal = crate::platform_identity::verify(&snapshot, &peer, &request.chain, now)?;
    if !principal.permits(
        if write { "propose" } else { "read" },
        &format!(
            "{}:{tenant}",
            if action_record {
                "action-records"
            } else {
                "records"
            }
        ),
    ) {
        return Err(denied().into());
    }
    let records = runtime.records.get(&tenant).ok_or_else(denied)?;
    if action_record {
        // Only the fenced governing artifact supplies policy; never the request body.
        let policy: ActionPolicy = serde_json::from_value(
            snapshot
                .artifact
                .as_ref()
                .ok_or_else(denied)?
                .bindings
                .get(&format!("action-records:{}", snapshot.config.audience))
                .ok_or_else(denied)?
                .clone(),
        )
        .map_err(|_| denied())?;
        let admission = ActionAdmission {
            identity: munarium_core::platform::RecorderIdentity {
                origin: principal.origin().into(),
                actor: principal.actor().into(),
                origin_kind: principal.origin_kind().into(),
                principal_digest: principal.fingerprint().into(),
            },
            service: peer.0.service.clone(),
            tenant: tenant.clone(),
            deployment: snapshot.config.deployment.clone(),
            now: u64::try_from(now).map_err(|_| denied())?,
            can_record: write,
            can_read: !write,
        };
        let ledger = ActionLedger::new(records.store.as_ref(), &records.version, &policy);
        let response = match request.action {
            Operation::ActionArchive { record } => {
                ledger.archive(&admission, record.as_bytes()).await?
            }
            Operation::ActionAppend { event } => {
                ledger.append(&admission, event.as_bytes()).await?
            }
            Operation::ActionLookup { operation_id } => {
                ledger.lookup(&admission, &operation_id).await?
            }
            Operation::ActionTransition { transition_id } => {
                ledger.transition(&admission, &transition_id).await?
            }
            Operation::ActionSourceHead {
                stream_id,
                generation,
            } => {
                ledger
                    .source_head(&admission, &stream_id, generation)
                    .await?
            }
            _ => return Err(denied().into()),
        };
        return Ok(Json(response));
    }
    let ledger = EventLedger::new(records.store.as_ref(), &tenant, &records.version);
    let context = RecorderContext {
        identity: munarium_core::platform::RecorderIdentity {
            origin: principal.origin().into(),
            actor: principal.actor().into(),
            origin_kind: principal.origin_kind().into(),
            principal_digest: principal.fingerprint().into(),
        },
        tenant: tenant.clone(),
        service: peer.0.service.clone(),
        source: peer.0.service,
        epoch: snapshot.config.epoch,
        can_record: write,
        can_read: !write,
    };
    let response = match request.action {
        Operation::SourceHead => ledger.source_head(&context).await?,
        Operation::Append { event } => ledger.append(&context, event.as_bytes()).await?,
        Operation::Lookup { operation_id } => {
            json!({"events":ledger.lookup(&context,&operation_id).await?})
        }
        Operation::Archive { bundle } => {
            json!({"digest":ledger.archive_replay(&context,bundle.as_bytes()).await?})
        }
        Operation::Replay { operation_id } => match ledger.replay(&context, &operation_id).await? {
            Some((bytes, digest)) => {
                json!({"digest":digest,"bundle":String::from_utf8(bytes).map_err(|_|denied())?})
            }
            None => json!({"digest":null,"bundle":null}),
        },
        _ => return Err(denied().into()),
    };
    Ok(Json(response))
}
