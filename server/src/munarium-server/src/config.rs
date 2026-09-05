// SPDX-License-Identifier: Apache-2.0
//! Env-var config surface — the contract in README.md.

#[derive(Debug, Clone)]
pub struct Config {
    pub http_addr: String,
    /// None = listener disabled (MUNARIUM_GRPC_ADDR=disabled, the ACA fallback).
    pub grpc_addr: Option<String>,
    pub ops_addr: String,
    pub store: StoreKind,
    pub database_url: Option<String>,
    pub auth: AuthMode,
    pub shutdown_grace_secs: u64,
    /// HS256 secret for capability JWTs. None = token issuance and
    /// JWT auth are unavailable (endpoints answer invalid-input).
    pub token_secret: Option<Vec<u8>>,
    /// Default TTL for issued tokens; issuance clamps to munarium_access::MAX_TTL_SECS.
    pub token_ttl_secs: u64,
    /// Require X-Munarium-Uid / munarium-uid on every /v1 request (the uid contract).
    /// false substitutes uid="anonymous" — the migration escape hatch.
    pub require_uid: bool,
    /// Interaction capture: bodies above this many bytes are stored as a
    /// {sha256, bytes_len} summary instead of verbatim JSON.
    pub interaction_body_max: usize,
    /// Check the access_tokens deny-list on every JWT verify (one indexed
    /// lookup). Off by default so verification stays pure CPU.
    pub token_revocation_check: bool,
    /// Base URL of the Munarium Matrix REST plane. None = the
    /// structured-evidence plane is not configured, and a runbook that
    /// declares data views cannot be verified or served — which the
    /// verifyDataViews step reports as a FAILURE rather than a pass. A
    /// verification step that passes when it verified nothing is the
    /// vacuously-green trap the Postgres conformance tier already fell into.
    pub matrix_base_url: Option<String>,
    /// Where a BROWSER reaches Matrix's own operator
    /// console, for the reciprocal link on `/admin/matrix`. Distinct from
    /// `matrix_base_url` because that one is the service-to-service address —
    /// an internal ingress or a cluster DNS name a person's browser cannot
    /// resolve. Unset, the page falls back to `<matrix_base_url>/admin`, which
    /// is right wherever the two coincide (a single-host deployment), and links nowhere
    /// when neither is set: an `<a>` to nowhere reads as a deployment that
    /// has one.
    pub matrix_admin_url: Option<String>,
    /// In-flight request ceiling per plane, per instance. At the limit new
    /// requests are refused immediately with 503 `overloaded` +
    /// `Retry-After: 1` (REST) / RESOURCE_EXHAUSTED (gRPC) instead of
    /// queueing into a latency collapse.
    pub max_concurrency: usize,
    /// sqlx pool size (per instance). Cluster math: N_instances ×
    /// MUNARIUM_DB_MAX_CONNS + in-flight runbook advisory-lock connections
    /// must stay under postgres max_connections.
    pub db_max_conns: u32,
    /// Idempotency-record retention; the janitor deletes older rows.
    /// 0 disables the janitor (records kept forever).
    pub idempotency_ttl_secs: u64,
    /// How many instances share this database. >1 arms the cluster-mode
    /// config validation and divides the per-process provider rate budgets
    /// so the CLUSTER honors a configured rpm/tpm, not each instance.
    pub replica_count: u32,
    /// Staleness bound for the lazy-loaded shape/provider registries: an
    /// entry loaded longer ago than this is re-read from the table, so a
    /// config applied on another instance converges within the TTL.
    /// 0 = load-once (the single-instance behavior through v0.1.2).
    pub registry_ttl_secs: u64,
    /// Idle-session expiry (2026-08-18, the deferred half of §13 entry 11):
    /// an open session whose last turn (or creation) is older than this is
    /// stamped `expired` by the janitor sweep; further turns answer 409
    /// `session-not-open`. 0 = DISABLED (the default — sessions stay
    /// immortal unless the deployment opts into a policy).
    pub session_idle_ttl_secs: u64,
    /// How often the evidence retention janitor sweeps, in seconds. 0 disables
    /// it — which is the honest default for a deployment that has not decided
    /// its retention policy yet, because a janitor nobody configured deleting
    /// regulated data on a guessed schedule is worse than one that never runs.
    pub evidence_purge_interval_secs: u64,
    /// Per-call output-token budgets (2026-09-02) — the process defaults
    /// every tenant starts from: `MUNARIUM_MAX_TOKENS_*` over the built-ins
    /// (max_tokens_api.rs). A tenant replaces them through
    /// `POST /v1/max-tokens`; a runbook's own `maxTokens` still wins.
    pub max_tokens: munarium_api_types::MaxTokensBudgets,
    /// This instance's identity in logs and interaction rows (debugging N
    /// replicas). Resolution: MUNARIUM_INSTANCE_ID → HOSTNAME → COMPUTERNAME →
    /// a random munarium-<suffix>.
    pub instance_id: String,
    /// Where raw document bytes live.
    pub source_store: SourceStoreConfig,
    /// Optional escalation for documents local extraction cannot read.
    pub doc_intel: DocIntelConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    Memory,
    Postgres,
}

/// Where raw document bytes live. `Azure` is the production default; `Pg`
/// keeps `docker compose up` and the test suite running with no Azure
/// account and no Azurite. `S3`/`Gcs`/`File` ride the object_store adapter
/// (munarium-store-objects) — topology is configured here, credentials come
/// from each cloud's ambient chain unless a static override is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStoreConfig {
    Azure {
        account: String,
        container: String,
        auth: BlobAuthConfig,
        endpoint: Option<String>,
    },
    Pg,
    Mem,
    S3 {
        bucket: String,
        region: String,
        endpoint: Option<String>,
        force_path_style: bool,
        /// Static credentials for off-cloud tooling (MinIO, CI). None = the
        /// ambient AWS chain (env vars, web identity, instance profile).
        access_key_id: Option<String>,
        /// Already resolved through the credential seam — the secret itself.
        secret_access_key: Option<String>,
    },
    Gcs {
        bucket: String,
        /// Service-account key JSON (content, resolved through the
        /// credential seam). None = GOOGLE_APPLICATION_CREDENTIALS or the
        /// metadata server.
        service_account_json: Option<String>,
    },
    File {
        root: String,
    },
}

/// The document-intelligence escalation. `None` is the DEFAULT and is a
/// complete configuration, not a degraded one: every call to a hosted
/// analyzer costs money per page and leaves the cluster, so it is opted into
/// per environment rather than inherited by accident. See
/// docs/guides/document-intelligence.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocIntelConfig {
    None,
    Azure {
        endpoint: String,
        auth: DocIntelAuthConfig,
        model: String,
        max_bytes: usize,
        timeout_secs: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocIntelAuthConfig {
    ManagedIdentity { client_id: Option<String> },
    Key { key: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobAuthConfig {
    /// No secret exists — the workload identity is the credential.
    ManagedIdentity { client_id: Option<String> },
    /// A container SAS resolved through the CredentialRef seam (env var or
    /// mounted file), for tooling that runs outside Azure.
    Sas { token: String },
}

#[derive(Debug, Clone)]
pub enum AuthMode {
    Disabled,
    /// token -> (tenant, role)
    Static(Vec<(String, String, String)>),
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let store = match env_or("MUNARIUM_STORE", "memory").as_str() {
            "postgres" => StoreKind::Postgres,
            "memory" => StoreKind::Memory,
            other => {
                return Err(format!(
                    "MUNARIUM_STORE must be postgres|memory, got '{other}'"
                ))
            }
        };
        let database_url = std::env::var("MUNARIUM_DATABASE_URL").ok();
        if store == StoreKind::Postgres && database_url.is_none() {
            return Err("MUNARIUM_STORE=postgres requires MUNARIUM_DATABASE_URL".into());
        }
        let grpc_addr = match env_or("MUNARIUM_GRPC_ADDR", "0.0.0.0:50051") {
            s if s == "disabled" => None,
            s => Some(s),
        };
        let auth = match env_or("MUNARIUM_AUTH_MODE", "static").as_str() {
            "disabled" => AuthMode::Disabled,
            "static" => {
                let raw = match std::env::var("MUNARIUM_STATIC_TOKENS") {
                    Ok(v) => v,
                    Err(_) => match std::env::var("MUNARIUM_STATIC_TOKEN_FILE") {
                        Ok(path) => std::fs::read_to_string(&path)
                            .map_err(|e| format!("MUNARIUM_STATIC_TOKEN_FILE {path}: {e}"))?,
                        Err(_) => {
                            return Err(
                                "MUNARIUM_AUTH_MODE=static requires MUNARIUM_STATIC_TOKENS or _FILE \
                                 (token:tenant:role,...); use MUNARIUM_AUTH_MODE=disabled to opt out"
                                    .into(),
                            )
                        }
                    },
                };
                let mut tokens = Vec::new();
                for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let parts: Vec<&str> = entry.split(':').collect();
                    if parts.len() != 3 {
                        return Err(format!("bad static token entry '{entry}' (token:tenant:role)"));
                    }
                    // The platform surface extends the role vocabulary: mgmt is the API-management
                    // layer's role (token issuance, reports, admin) — distinct
                    // from rw so a leaked mgmt token cannot write the ledger.
                    if !matches!(parts[2], "rw" | "ro" | "mgmt") {
                        return Err(format!(
                            "bad static token role '{}' in '{entry}' (rw|ro|mgmt)",
                            parts[2]
                        ));
                    }
                    tokens.push((parts[0].into(), parts[1].into(), parts[2].into()));
                }
                AuthMode::Static(tokens)
            }
            other => return Err(format!("MUNARIUM_AUTH_MODE must be static|disabled (oidc arrives with the deploy hardening), got '{other}'")),
        };
        let token_secret = match std::env::var("MUNARIUM_TOKEN_SECRET") {
            Ok(v) => Some(v.into_bytes()),
            Err(_) => match std::env::var("MUNARIUM_TOKEN_SECRET_FILE") {
                Ok(path) => Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| format!("MUNARIUM_TOKEN_SECRET_FILE {path}: {e}"))?
                        .trim()
                        .as_bytes()
                        .to_vec(),
                ),
                Err(_) => None,
            },
        };
        if let Some(secret) = &token_secret {
            if secret.len() < 32 {
                return Err(
                    "MUNARIUM_TOKEN_SECRET must be at least 32 bytes (HS256 key material)".into(),
                );
            }
        }
        let require_uid = match env_or("MUNARIUM_REQUIRE_UID", "true").as_str() {
            "true" | "1" => true,
            "false" | "0" => false,
            other => {
                return Err(format!(
                    "MUNARIUM_REQUIRE_UID must be true|false, got '{other}'"
                ))
            }
        };
        let matrix_base_url = std::env::var("MUNARIUM_MATRIX_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let matrix_admin_url = std::env::var("MUNARIUM_MATRIX_ADMIN_URL")
            .ok()
            .filter(|v| !v.trim().is_empty());
        let token_revocation_check =
            match env_or("MUNARIUM_TOKEN_REVOCATION_CHECK", "false").as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                other => {
                    return Err(format!(
                        "MUNARIUM_TOKEN_REVOCATION_CHECK must be true|false, got '{other}'"
                    ))
                }
            };
        let source_store = source_store_from_env(store)?;
        let doc_intel = doc_intel_from_env()?;
        let config = Self {
            http_addr: env_or("MUNARIUM_HTTP_ADDR", "0.0.0.0:8080"),
            grpc_addr,
            ops_addr: env_or("MUNARIUM_OPS_ADDR", "0.0.0.0:9090"),
            store,
            database_url,
            auth,
            shutdown_grace_secs: env_or("MUNARIUM_SHUTDOWN_GRACE_SECS", "20")
                .parse()
                .map_err(|e| format!("MUNARIUM_SHUTDOWN_GRACE_SECS: {e}"))?,
            token_secret,
            token_ttl_secs: env_or("MUNARIUM_TOKEN_TTL_SECS", "3600")
                .parse()
                .map_err(|e| format!("MUNARIUM_TOKEN_TTL_SECS: {e}"))?,
            require_uid,
            interaction_body_max: env_or("MUNARIUM_INTERACTION_BODY_MAX", "32768")
                .parse()
                .map_err(|e| format!("MUNARIUM_INTERACTION_BODY_MAX: {e}"))?,
            token_revocation_check,
            matrix_base_url,
            matrix_admin_url,
            max_concurrency: parse_max_concurrency()?,
            db_max_conns: parse_db_max_conns()?,
            idempotency_ttl_secs: env_or("MUNARIUM_IDEMPOTENCY_TTL_SECS", "86400")
                .parse()
                .map_err(|e| format!("MUNARIUM_IDEMPOTENCY_TTL_SECS: {e}"))?,
            replica_count: parse_replica_count()?,
            registry_ttl_secs: env_or("MUNARIUM_REGISTRY_TTL_SECS", "15")
                .parse()
                .map_err(|e| format!("MUNARIUM_REGISTRY_TTL_SECS: {e}"))?,
            evidence_purge_interval_secs: env_or("MUNARIUM_EVIDENCE_PURGE_INTERVAL_SECS", "0")
                .parse()
                .map_err(|e| format!("MUNARIUM_EVIDENCE_PURGE_INTERVAL_SECS: {e}"))?,
            session_idle_ttl_secs: env_or("MUNARIUM_SESSION_IDLE_TTL_SECS", "0")
                .parse()
                .map_err(|e| format!("MUNARIUM_SESSION_IDLE_TTL_SECS: {e}"))?,
            max_tokens: crate::max_tokens_api::from_env()?,
            instance_id: resolve_instance_id(),
            source_store,
            doc_intel,
        };
        // Cluster-mode validation (fail closed, like everything else here):
        // more than one instance on a per-process store cannot work — the
        // instances would each hold a private world and agree on nothing.
        if config.replica_count > 1 {
            if matches!(config.store, StoreKind::Memory) {
                return Err(
                    "MUNARIUM_REPLICA_COUNT > 1 requires MUNARIUM_STORE=postgres (the memory \
                     store is per-process; a cluster of memory stores shares nothing)"
                        .into(),
                );
            }
            if matches!(config.source_store, SourceStoreConfig::Mem) {
                return Err("MUNARIUM_REPLICA_COUNT > 1 requires a shared source store \
                     (MUNARIUM_SOURCE_STORE=mem is per-process; use pg|az|s3|gcs, or file \
                     on a shared mount)"
                    .into());
            }
            if matches!(config.source_store, SourceStoreConfig::File { .. }) {
                tracing::warn!(
                    "MUNARIUM_REPLICA_COUNT > 1 with MUNARIUM_SOURCE_STORE=file: every \
                     instance must see the same MUNARIUM_FILE_ROOT (shared mount), or \
                     documents will silently exist on only one instance"
                );
            }
        }
        Ok(config)
    }
}

/// `MUNARIUM_SOURCE_STORE` = az | pg | mem | s3 | gcs | file.
///
/// Defaults to `pg` under the memory ledger (nothing to serve bytes from
/// otherwise) and `az` under Postgres — production posture without making
/// every local `docker compose up` reach for Azure, which the compose file
/// overrides to `pg` explicitly.
fn source_store_from_env(store: StoreKind) -> Result<SourceStoreConfig, String> {
    let default = match store {
        StoreKind::Postgres => "az",
        StoreKind::Memory => "mem",
    };
    match env_or("MUNARIUM_SOURCE_STORE", default).as_str() {
        "pg" => Ok(SourceStoreConfig::Pg),
        "mem" => Ok(SourceStoreConfig::Mem),
        "az" => {
            // Fail closed, exactly like MUNARIUM_DATABASE_URL under
            // MUNARIUM_STORE=postgres: a missing account must not degrade to a
            // silent local-bytes fallback nobody notices until a restart.
            // 'az' is usually the DEFAULT under MUNARIUM_STORE=postgres rather
            // than something the operator typed, so the error must explain
            // both how they got here and the way out — a bare
            // "az requires an account" reads as nonsense on a laptop.
            let account = std::env::var("MUNARIUM_AZURE_STORAGE_ACCOUNT").map_err(|_| {
                "source store is 'az' (the default under MUNARIUM_STORE=postgres): set                  MUNARIUM_AZURE_STORAGE_ACCOUNT, or set MUNARIUM_SOURCE_STORE=pg to keep                  document bytes in Postgres (local/CI posture)"
                    .to_string()
            })?;
            let container = env_or("MUNARIUM_AZURE_BLOB_CONTAINER", "sources");
            let auth = match env_or("MUNARIUM_BLOB_AUTH", "managed_identity").as_str() {
                "managed_identity" => BlobAuthConfig::ManagedIdentity {
                    client_id: std::env::var("MUNARIUM_AZURE_CLIENT_ID").ok(),
                },
                "sas" => {
                    let reference = std::env::var("MUNARIUM_BLOB_SAS_REF").map_err(|_| {
                        "MUNARIUM_BLOB_AUTH=sas requires MUNARIUM_BLOB_SAS_REF".to_string()
                    })?;
                    BlobAuthConfig::Sas {
                        token: resolve_secret(&reference)?,
                    }
                }
                other => {
                    return Err(format!(
                        "MUNARIUM_BLOB_AUTH must be managed_identity|sas, got '{other}'"
                    ))
                }
            };
            Ok(SourceStoreConfig::Azure {
                account,
                container,
                auth,
                endpoint: std::env::var("MUNARIUM_AZURE_BLOB_ENDPOINT").ok(),
            })
        }
        "s3" => {
            let bucket = std::env::var("MUNARIUM_S3_BUCKET").map_err(|_| {
                "source store is 's3': set MUNARIUM_S3_BUCKET, or set                  MUNARIUM_SOURCE_STORE=pg to keep document bytes in Postgres                  (local/CI posture)"
                    .to_string()
            })?;
            let endpoint = std::env::var("MUNARIUM_S3_ENDPOINT").ok();
            // SigV4 needs a region even when an S3-compatible endpoint will
            // ignore it; against real AWS a missing region is a config bug,
            // so only the endpoint case gets the conventional placeholder.
            let region = std::env::var("MUNARIUM_S3_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                .or_else(|_| {
                    if endpoint.is_some() {
                        Ok("us-east-1".to_string())
                    } else {
                        Err(
                            "MUNARIUM_SOURCE_STORE=s3 needs a region: set MUNARIUM_S3_REGION                              (or AWS_REGION); S3-compatible endpoints may skip it because                              MUNARIUM_S3_ENDPOINT implies one"
                                .to_string(),
                        )
                    }
                })?;
            // MinIO and most S3-compatibles require bucket-in-path
            // addressing; AWS prefers virtual-hosted. Default follows the
            // endpoint, override for the exceptions (e.g. R2 works either way).
            let force_path_style = match std::env::var("MUNARIUM_S3_FORCE_PATH_STYLE") {
                Ok(v) => match v.as_str() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    other => {
                        return Err(format!(
                            "MUNARIUM_S3_FORCE_PATH_STYLE must be true|false, got '{other}'"
                        ))
                    }
                },
                Err(_) => endpoint.is_some(),
            };
            // Half a static credential is a misconfiguration, not a signal
            // to fall back to the ambient chain — that fallback would sign
            // with whatever identity the host happens to have.
            let access_key_id = std::env::var("MUNARIUM_S3_ACCESS_KEY_ID").ok();
            let secret_ref = std::env::var("MUNARIUM_S3_SECRET_KEY_REF").ok();
            let (access_key_id, secret_access_key) = match (access_key_id, secret_ref) {
                (Some(id), Some(reference)) => (Some(id), Some(resolve_secret(&reference)?)),
                (None, None) => (None, None),
                (Some(_), None) => {
                    return Err(
                        "MUNARIUM_S3_ACCESS_KEY_ID is set without MUNARIUM_S3_SECRET_KEY_REF                          — static credentials need both, or neither for the ambient                          AWS chain"
                            .to_string(),
                    )
                }
                (None, Some(_)) => {
                    return Err(
                        "MUNARIUM_S3_SECRET_KEY_REF is set without MUNARIUM_S3_ACCESS_KEY_ID                          — static credentials need both, or neither for the ambient                          AWS chain"
                            .to_string(),
                    )
                }
            };
            Ok(SourceStoreConfig::S3 {
                bucket,
                region,
                endpoint,
                force_path_style,
                access_key_id,
                secret_access_key,
            })
        }
        "gcs" => {
            let bucket = std::env::var("MUNARIUM_GCS_BUCKET").map_err(|_| {
                "source store is 'gcs': set MUNARIUM_GCS_BUCKET, or set                  MUNARIUM_SOURCE_STORE=pg to keep document bytes in Postgres                  (local/CI posture)"
                    .to_string()
            })?;
            let service_account_json = match std::env::var("MUNARIUM_GCS_CREDENTIALS_REF") {
                Ok(reference) => Some(resolve_secret(&reference)?),
                Err(_) => None, // ambient: GOOGLE_APPLICATION_CREDENTIALS / metadata server
            };
            Ok(SourceStoreConfig::Gcs {
                bucket,
                service_account_json,
            })
        }
        "file" => {
            // No default directory on purpose: a silent /tmp fallback is
            // precisely the local-bytes surprise the az arm refuses above.
            let root = std::env::var("MUNARIUM_FILE_ROOT").map_err(|_| {
                "MUNARIUM_SOURCE_STORE=file requires MUNARIUM_FILE_ROOT (the directory                  document bytes live under)"
                    .to_string()
            })?;
            Ok(SourceStoreConfig::File { root })
        }
        other => Err(format!(
            "MUNARIUM_SOURCE_STORE must be az|pg|mem|s3|gcs|file, got '{other}'"
        )),
    }
}

/// `MUNARIUM_DOCINTEL` = none | azure. **Default: none.**
///
/// Off by default on purpose. A document-intelligence provider is a paid,
/// network-egressing dependency, and a system that quietly acquires one
/// because a default said so is a system that surprises somebody with a bill
/// or a data-residency problem. Environments that want it say so — dev and
/// std both do, in their Terraform.
fn doc_intel_from_env() -> Result<DocIntelConfig, String> {
    match env_or("MUNARIUM_DOCINTEL", "none").as_str() {
        "none" | "off" | "" => Ok(DocIntelConfig::None),
        "azure" => {
            // Fail closed: enabling the escalation and silently getting
            // nothing would look exactly like "this corpus has no text".
            let endpoint = std::env::var("MUNARIUM_DOCINTEL_ENDPOINT").map_err(|_| {
                "MUNARIUM_DOCINTEL=azure requires MUNARIUM_DOCINTEL_ENDPOINT".to_string()
            })?;
            let auth = match env_or("MUNARIUM_DOCINTEL_AUTH", "managed_identity").as_str() {
                "managed_identity" => DocIntelAuthConfig::ManagedIdentity {
                    client_id: std::env::var("MUNARIUM_AZURE_CLIENT_ID").ok(),
                },
                "key" => {
                    let reference = std::env::var("MUNARIUM_DOCINTEL_KEY_REF").map_err(|_| {
                        "MUNARIUM_DOCINTEL_AUTH=key requires MUNARIUM_DOCINTEL_KEY_REF".to_string()
                    })?;
                    DocIntelAuthConfig::Key {
                        key: resolve_secret(&reference)?,
                    }
                }
                other => {
                    return Err(format!(
                        "MUNARIUM_DOCINTEL_AUTH must be managed_identity|key, got '{other}'"
                    ))
                }
            };
            Ok(DocIntelConfig::Azure {
                endpoint,
                auth,
                model: env_or("MUNARIUM_DOCINTEL_MODEL", "prebuilt-read"),
                max_bytes: env_or("MUNARIUM_DOCINTEL_MAX_BYTES", "104857600")
                    .parse()
                    .map_err(|e| format!("MUNARIUM_DOCINTEL_MAX_BYTES: {e}"))?,
                timeout_secs: env_or("MUNARIUM_DOCINTEL_TIMEOUT_SECS", "180")
                    .parse()
                    .map_err(|e| format!("MUNARIUM_DOCINTEL_TIMEOUT_SECS: {e}"))?,
            })
        }
        other => Err(format!(
            "MUNARIUM_DOCINTEL must be none|azure, got '{other}'.              Other providers plug in here — see docs/guides/document-intelligence.md"
        )),
    }
}

/// The same two-variant credential seam the BYOK provider keys use: an env
/// var name (Key Vault references surface as env on ACA) or a file path
/// (Secrets Store CSI mount on AKS). `file:` prefix selects the latter.
fn resolve_secret(reference: &str) -> Result<String, String> {
    let value = match reference.strip_prefix("file:") {
        Some(path) => {
            std::fs::read_to_string(path).map_err(|e| format!("credential file '{path}': {e}"))?
        }
        None => std::env::var(reference)
            .map_err(|_| format!("credential env var '{reference}' is not set"))?,
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(format!("credential '{reference}' is empty"));
    }
    Ok(value)
}

/// MUNARIUM_DB_MAX_CONNS: sqlx pool size. The floor is 2, not 1 — the append
/// path takes a pool connection for `locked_head`'s FOR UPDATE transaction
/// while other work holds connections; a pool of 1 deadlocks writers
/// against anything else (the store-pg lib.rs locked_head note).
fn parse_db_max_conns() -> Result<u32, String> {
    let n: u32 = env_or("MUNARIUM_DB_MAX_CONNS", "10")
        .parse()
        .map_err(|e| format!("MUNARIUM_DB_MAX_CONNS: {e}"))?;
    if n < 2 {
        return Err(
            "MUNARIUM_DB_MAX_CONNS must be >= 2 (a pool of 1 deadlocks the append path's \
             FOR UPDATE transaction against any concurrent work)"
                .into(),
        );
    }
    Ok(n)
}

/// MUNARIUM_REPLICA_COUNT: how many instances share the database. 0 is a
/// config error (a cluster of zero serves nobody); 1 is the default
/// single-instance posture.
fn parse_replica_count() -> Result<u32, String> {
    let n: u32 = env_or("MUNARIUM_REPLICA_COUNT", "1")
        .parse()
        .map_err(|e| format!("MUNARIUM_REPLICA_COUNT: {e}"))?;
    if n == 0 {
        return Err("MUNARIUM_REPLICA_COUNT must be >= 1".into());
    }
    Ok(n)
}

/// The instance's identity for logs and interaction rows. Kubernetes and
/// compose set HOSTNAME; Windows dev boxes have COMPUTERNAME; the random
/// fallback keeps ids distinct when neither exists.
fn resolve_instance_id() -> String {
    ["MUNARIUM_INSTANCE_ID", "HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|k| {
            std::env::var(k)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| {
            let suffix = uuid::Uuid::now_v7().simple().to_string();
            format!("munarium-{}", &suffix[suffix.len() - 12..])
        })
}

/// MUNARIUM_MAX_CONCURRENCY: in-flight ceiling per plane per instance. 0 is a
/// config error, not "unlimited" — it would refuse every request, and a
/// server that starts only to shed everything is a half-started process.
fn parse_max_concurrency() -> Result<usize, String> {
    let n: usize = env_or("MUNARIUM_MAX_CONCURRENCY", "512")
        .parse()
        .map_err(|e| format!("MUNARIUM_MAX_CONCURRENCY: {e}"))?;
    if n == 0 {
        return Err("MUNARIUM_MAX_CONCURRENCY must be >= 1 (0 would refuse every request)".into());
    }
    Ok(n)
}

#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    // ONE lock for every test that mutates process env, across all the test
    // modules in this file — per-module locks let two modules' clear()
    // helpers race each other (found live 2026-08-17: a cluster-mode test
    // read a var another module's clear() had just removed).
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        super::env_test_lock()
    }

    #[test]
    fn defaults_to_512() {
        let _g = lock();
        std::env::remove_var("MUNARIUM_MAX_CONCURRENCY");
        assert_eq!(parse_max_concurrency().unwrap(), 512);
    }

    #[test]
    fn zero_and_junk_fail_closed() {
        let _g = lock();
        std::env::set_var("MUNARIUM_MAX_CONCURRENCY", "0");
        let err = parse_max_concurrency().expect_err("0 must be rejected");
        assert!(err.contains("MUNARIUM_MAX_CONCURRENCY"), "{err}");
        std::env::set_var("MUNARIUM_MAX_CONCURRENCY", "many");
        assert!(parse_max_concurrency().is_err());
        std::env::remove_var("MUNARIUM_MAX_CONCURRENCY");
    }
}

#[cfg(test)]
mod source_store_tests {
    use super::*;

    /// Serialize: these tests mutate process-wide env.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        super::env_test_lock()
    }

    fn clear() {
        for k in [
            "MUNARIUM_SOURCE_STORE",
            "MUNARIUM_AZURE_STORAGE_ACCOUNT",
            "MUNARIUM_AZURE_BLOB_CONTAINER",
            "MUNARIUM_BLOB_AUTH",
            "MUNARIUM_BLOB_SAS_REF",
            "MUNARIUM_AZURE_CLIENT_ID",
            "MUNARIUM_AZURE_BLOB_ENDPOINT",
            "MUNARIUM_S3_BUCKET",
            "MUNARIUM_S3_REGION",
            "MUNARIUM_S3_ENDPOINT",
            "MUNARIUM_S3_FORCE_PATH_STYLE",
            "MUNARIUM_S3_ACCESS_KEY_ID",
            "MUNARIUM_S3_SECRET_KEY_REF",
            "MUNARIUM_GCS_BUCKET",
            "MUNARIUM_GCS_CREDENTIALS_REF",
            "MUNARIUM_FILE_ROOT",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "MUNARIUM_STORE",
            "MUNARIUM_DATABASE_URL",
            "MUNARIUM_AUTH_MODE",
            "MUNARIUM_STATIC_TOKENS",
            "MUNARIUM_REPLICA_COUNT",
            "MUNARIUM_DB_MAX_CONNS",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn az_without_an_account_fails_closed() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "az");
        // Must NOT silently degrade to local bytes: a missing account is a
        // startup error, exactly like MUNARIUM_DATABASE_URL under postgres.
        let err = source_store_from_env(StoreKind::Postgres).expect_err("must fail closed");
        assert!(err.contains("MUNARIUM_AZURE_STORAGE_ACCOUNT"), "{err}");
        // The default chose 'az', not the operator — the message must say so
        // and must name the pg escape hatch, or a laptop/CI launch reads it
        // as nonsense.
        assert!(
            err.contains("default under MUNARIUM_STORE=postgres"),
            "{err}"
        );
        assert!(err.contains("MUNARIUM_SOURCE_STORE=pg"), "{err}");
        clear();
    }

    #[test]
    fn az_defaults_to_managed_identity_and_the_sources_container() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "az");
        std::env::set_var("MUNARIUM_AZURE_STORAGE_ACCOUNT", "stexampledev01");
        match source_store_from_env(StoreKind::Postgres).expect("config") {
            SourceStoreConfig::Azure {
                account,
                container,
                auth,
                ..
            } => {
                assert_eq!(account, "stexampledev01");
                assert_eq!(container, "sources");
                assert_eq!(auth, BlobAuthConfig::ManagedIdentity { client_id: None });
            }
            other => panic!("expected Azure, got {other:?}"),
        }
        clear();
    }

    #[test]
    fn sas_resolves_through_the_credential_seam() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "az");
        std::env::set_var("MUNARIUM_AZURE_STORAGE_ACCOUNT", "stexampleci01");
        std::env::set_var("MUNARIUM_BLOB_AUTH", "sas");
        // Referenced by NAME, never inlined — same contract as the BYOK keys.
        std::env::set_var("MUNARIUM_BLOB_SAS_REF", "TEST_SAS_VALUE");
        std::env::set_var("TEST_SAS_VALUE", "sv=2021&sig=abc");
        match source_store_from_env(StoreKind::Postgres).expect("config") {
            SourceStoreConfig::Azure { auth, .. } => assert_eq!(
                auth,
                BlobAuthConfig::Sas {
                    token: "sv=2021&sig=abc".into()
                }
            ),
            other => panic!("expected Azure, got {other:?}"),
        }
        // A named-but-unset reference must fail rather than run unauthenticated.
        std::env::remove_var("TEST_SAS_VALUE");
        assert!(source_store_from_env(StoreKind::Postgres).is_err());
        std::env::remove_var("TEST_SAS_VALUE");
        clear();
    }

    #[test]
    fn defaults_keep_offline_modes_working() {
        let _g = lock();
        clear();
        // Memory ledger has no pool to hold blobs.
        assert_eq!(
            source_store_from_env(StoreKind::Memory).expect("mem"),
            SourceStoreConfig::Mem
        );
        // The dev compose profile pins pg explicitly.
        std::env::set_var("MUNARIUM_SOURCE_STORE", "pg");
        assert_eq!(
            source_store_from_env(StoreKind::Postgres).expect("pg"),
            SourceStoreConfig::Pg
        );
        std::env::set_var("MUNARIUM_SOURCE_STORE", "nope");
        // The refusal must teach the full vocabulary.
        let err = source_store_from_env(StoreKind::Postgres).expect_err("unknown value");
        assert!(err.contains("az|pg|mem|s3|gcs|file"), "{err}");
        clear();
    }

    #[test]
    fn s3_without_bucket_fails_closed() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "s3");
        let err = source_store_from_env(StoreKind::Postgres).expect_err("must fail closed");
        assert!(err.contains("MUNARIUM_S3_BUCKET"), "{err}");
        assert!(err.contains("MUNARIUM_SOURCE_STORE=pg"), "{err}");
        clear();
    }

    #[test]
    fn s3_without_region_or_endpoint_fails_closed() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "s3");
        std::env::set_var("MUNARIUM_S3_BUCKET", "sources");
        let err = source_store_from_env(StoreKind::Postgres).expect_err("no region");
        assert!(err.contains("MUNARIUM_S3_REGION"), "{err}");
        clear();
    }

    #[test]
    fn s3_endpoint_defaults_path_style_and_region() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "s3");
        std::env::set_var("MUNARIUM_S3_BUCKET", "sources");
        std::env::set_var("MUNARIUM_S3_ENDPOINT", "http://127.0.0.1:9000");
        match source_store_from_env(StoreKind::Postgres).expect("config") {
            SourceStoreConfig::S3 {
                bucket,
                region,
                endpoint,
                force_path_style,
                access_key_id,
                secret_access_key,
            } => {
                assert_eq!(bucket, "sources");
                // MinIO/R2 ignore the region; the signer still needs one.
                assert_eq!(region, "us-east-1");
                assert_eq!(endpoint.as_deref(), Some("http://127.0.0.1:9000"));
                assert!(force_path_style, "endpoint implies path-style");
                assert_eq!(access_key_id, None);
                assert_eq!(secret_access_key, None);
            }
            other => panic!("expected S3, got {other:?}"),
        }
        // Against real AWS the ambient region is honored.
        std::env::remove_var("MUNARIUM_S3_ENDPOINT");
        std::env::set_var("AWS_REGION", "eu-west-1");
        match source_store_from_env(StoreKind::Postgres).expect("config") {
            SourceStoreConfig::S3 {
                region,
                force_path_style,
                ..
            } => {
                assert_eq!(region, "eu-west-1");
                assert!(!force_path_style, "no endpoint means virtual-hosted");
            }
            other => panic!("expected S3, got {other:?}"),
        }
        clear();
    }

    #[test]
    fn s3_half_a_static_credential_is_refused() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "s3");
        std::env::set_var("MUNARIUM_S3_BUCKET", "sources");
        std::env::set_var("MUNARIUM_S3_REGION", "us-east-1");
        std::env::set_var("MUNARIUM_S3_ACCESS_KEY_ID", "minioadmin");
        // An id without a secret must not silently fall back to the ambient
        // chain — that would sign as whoever the host happens to be.
        let err = source_store_from_env(StoreKind::Postgres).expect_err("half credential");
        assert!(err.contains("MUNARIUM_S3_SECRET_KEY_REF"), "{err}");
        std::env::remove_var("MUNARIUM_S3_ACCESS_KEY_ID");
        std::env::set_var("MUNARIUM_S3_SECRET_KEY_REF", "SOME_REF");
        let err = source_store_from_env(StoreKind::Postgres).expect_err("half credential");
        assert!(err.contains("MUNARIUM_S3_ACCESS_KEY_ID"), "{err}");
        clear();
    }

    #[test]
    fn s3_secret_resolves_through_the_credential_seam() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "s3");
        std::env::set_var("MUNARIUM_S3_BUCKET", "sources");
        std::env::set_var("MUNARIUM_S3_REGION", "us-east-1");
        std::env::set_var("MUNARIUM_S3_ACCESS_KEY_ID", "minioadmin");
        // Referenced by NAME, never inlined — same contract as the SAS.
        std::env::set_var("MUNARIUM_S3_SECRET_KEY_REF", "TEST_S3_SECRET");
        std::env::set_var("TEST_S3_SECRET", "minio-secret");
        match source_store_from_env(StoreKind::Postgres).expect("config") {
            SourceStoreConfig::S3 {
                access_key_id,
                secret_access_key,
                ..
            } => {
                assert_eq!(access_key_id.as_deref(), Some("minioadmin"));
                assert_eq!(secret_access_key.as_deref(), Some("minio-secret"));
            }
            other => panic!("expected S3, got {other:?}"),
        }
        // A named-but-unset reference must fail rather than run unauthenticated.
        std::env::remove_var("TEST_S3_SECRET");
        assert!(source_store_from_env(StoreKind::Postgres).is_err());
        clear();
    }

    #[test]
    fn gcs_without_bucket_fails_closed() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "gcs");
        let err = source_store_from_env(StoreKind::Postgres).expect_err("must fail closed");
        assert!(err.contains("MUNARIUM_GCS_BUCKET"), "{err}");
        assert!(err.contains("MUNARIUM_SOURCE_STORE=pg"), "{err}");
        // With a bucket, ambient credentials are a complete configuration.
        std::env::set_var("MUNARIUM_GCS_BUCKET", "munarium-sources");
        assert_eq!(
            source_store_from_env(StoreKind::Postgres).expect("config"),
            SourceStoreConfig::Gcs {
                bucket: "munarium-sources".into(),
                service_account_json: None,
            }
        );
        clear();
    }

    #[test]
    fn file_without_root_fails_closed() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_SOURCE_STORE", "file");
        // No default directory: a silent /tmp fallback is the exact failure
        // mode the az arm's fail-closed comment warns about.
        let err = source_store_from_env(StoreKind::Postgres).expect_err("must fail closed");
        assert!(err.contains("MUNARIUM_FILE_ROOT"), "{err}");
        std::env::set_var("MUNARIUM_FILE_ROOT", "/var/lib/munarium/sources");
        assert_eq!(
            source_store_from_env(StoreKind::Postgres).expect("config"),
            SourceStoreConfig::File {
                root: "/var/lib/munarium/sources".into(),
            }
        );
        clear();
    }
}

#[cfg(test)]
mod doc_intel_tests {
    use super::*;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        super::env_test_lock()
    }

    fn clear() {
        for k in [
            "MUNARIUM_DOCINTEL",
            "MUNARIUM_DOCINTEL_ENDPOINT",
            "MUNARIUM_DOCINTEL_AUTH",
            "MUNARIUM_DOCINTEL_KEY_REF",
            "MUNARIUM_DOCINTEL_MODEL",
            "MUNARIUM_DOCINTEL_MAX_BYTES",
            "MUNARIUM_DOCINTEL_TIMEOUT_SECS",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn the_default_is_off() {
        let _g = lock();
        clear();
        // A paid, network-egressing dependency must never be acquired by
        // default. This is the assertion that keeps that true.
        assert_eq!(doc_intel_from_env().expect("default"), DocIntelConfig::None);
        clear();
    }

    #[test]
    fn azure_without_an_endpoint_fails_closed() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_DOCINTEL", "azure");
        // Enabling it and silently getting nothing would look exactly like
        // "this corpus has no text" — the failure mode hardest to notice.
        let err = doc_intel_from_env().expect_err("must fail closed");
        assert!(err.contains("MUNARIUM_DOCINTEL_ENDPOINT"), "{err}");
        clear();
    }

    #[test]
    fn azure_defaults_to_managed_identity_and_the_read_model() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_DOCINTEL", "azure");
        std::env::set_var("MUNARIUM_DOCINTEL_ENDPOINT", "https://di.example.net");
        match doc_intel_from_env().expect("config") {
            DocIntelConfig::Azure {
                endpoint,
                auth,
                model,
                timeout_secs,
                ..
            } => {
                assert_eq!(endpoint, "https://di.example.net");
                assert_eq!(model, "prebuilt-read");
                assert_eq!(timeout_secs, 180);
                assert_eq!(
                    auth,
                    DocIntelAuthConfig::ManagedIdentity { client_id: None }
                );
            }
            other => panic!("expected Azure, got {other:?}"),
        }
        clear();
    }

    #[test]
    fn the_key_path_resolves_through_the_credential_seam() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_DOCINTEL", "azure");
        std::env::set_var("MUNARIUM_DOCINTEL_ENDPOINT", "https://di.example.net");
        std::env::set_var("MUNARIUM_DOCINTEL_AUTH", "key");
        std::env::set_var("MUNARIUM_DOCINTEL_KEY_REF", "TEST_DI_KEY");
        std::env::set_var("TEST_DI_KEY", "abc123");
        match doc_intel_from_env().expect("config") {
            DocIntelConfig::Azure { auth, .. } => assert_eq!(
                auth,
                DocIntelAuthConfig::Key {
                    key: "abc123".into()
                }
            ),
            other => panic!("expected Azure, got {other:?}"),
        }
        // A named-but-unset reference must fail rather than run unauthenticated.
        std::env::remove_var("TEST_DI_KEY");
        assert!(doc_intel_from_env().is_err());
        clear();
    }

    #[test]
    fn an_unknown_provider_points_at_the_extension_guide() {
        let _g = lock();
        clear();
        std::env::set_var("MUNARIUM_DOCINTEL", "textract");
        let err = doc_intel_from_env().expect_err("unknown");
        assert!(err.contains("document-intelligence.md"), "{err}");
        clear();
    }

    // ---- cluster-mode validation (MUNARIUM_REPLICA_COUNT > 1) ----------------
    // These exercise the full Config::from_env under the shared env lock.
    // This module's clear() covers only docintel vars, so the cluster vars
    // get their own explicit sweep at start AND end (a leaked MUNARIUM_STORE
    // made the sibling test flaky — found live 2026-08-17).

    fn clear_cluster_vars() {
        for k in [
            "MUNARIUM_STORE",
            "MUNARIUM_DATABASE_URL",
            "MUNARIUM_SOURCE_STORE",
            "MUNARIUM_AUTH_MODE",
            "MUNARIUM_STATIC_TOKENS",
            "MUNARIUM_REPLICA_COUNT",
            "MUNARIUM_DB_MAX_CONNS",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn cluster_mode_rejects_the_memory_store() {
        let _g = lock();
        clear_cluster_vars();
        std::env::set_var("MUNARIUM_AUTH_MODE", "disabled");
        std::env::set_var("MUNARIUM_REPLICA_COUNT", "2");
        let err = Config::from_env().expect_err("memory store must be rejected");
        assert!(err.contains("MUNARIUM_STORE=postgres"), "{err}");
        clear_cluster_vars();
    }

    #[test]
    fn cluster_mode_rejects_the_mem_source_store_and_accepts_pg() {
        let _g = lock();
        clear_cluster_vars();
        std::env::set_var("MUNARIUM_AUTH_MODE", "disabled");
        std::env::set_var("MUNARIUM_REPLICA_COUNT", "2");
        std::env::set_var("MUNARIUM_STORE", "postgres");
        std::env::set_var("MUNARIUM_DATABASE_URL", "postgres://x/y");
        std::env::set_var("MUNARIUM_SOURCE_STORE", "mem");
        let err = Config::from_env().expect_err("mem source store must be rejected");
        assert!(err.contains("shared source store"), "{err}");
        std::env::set_var("MUNARIUM_SOURCE_STORE", "pg");
        let cfg = Config::from_env().expect("postgres+pg is a valid cluster config");
        assert_eq!(cfg.replica_count, 2);
        assert!(!cfg.instance_id.is_empty());
        clear_cluster_vars();
    }

    #[test]
    fn pool_and_replica_floors_fail_closed() {
        let _g = lock();
        clear_cluster_vars();
        std::env::set_var("MUNARIUM_DB_MAX_CONNS", "1");
        let err = parse_db_max_conns().expect_err("pool of 1 must be rejected");
        assert!(err.contains("deadlocks"), "{err}");
        std::env::set_var("MUNARIUM_DB_MAX_CONNS", "10");
        assert_eq!(parse_db_max_conns().unwrap(), 10);
        std::env::set_var("MUNARIUM_REPLICA_COUNT", "0");
        assert!(parse_replica_count().is_err());
        clear_cluster_vars();
    }
}
