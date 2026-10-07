// SPDX-License-Identifier: Apache-2.0
//! Direct mTLS transport. Certificate pins bind peers; forwarding headers are ignored.
use crate::{
    error::ApiError,
    platform_api::{AuthenticatedPeer, PlatformConfig},
    state::AppState,
};
use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use munarium_core::KernelError;
use std::{io, sync::Arc, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{rustls, server::TlsStream, TlsAcceptor};

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub identity: Option<AuthenticatedPeer>,
}
pub struct MtlsListener {
    listener: TcpListener,
    tls: TlsAcceptor,
    config: Arc<PlatformConfig>,
    pending: tokio::task::JoinSet<Option<(TlsStream<TcpStream>, PeerInfo)>>,
}
fn invalid_tls() -> io::Error {
    io::Error::other("invalid platform TLS configuration")
}
impl MtlsListener {
    pub fn new(listener: TcpListener, config: PlatformConfig) -> io::Result<Self> {
        let cert = std::fs::read(&config.certificate_file)?;
        let key = std::fs::read(&config.private_key_file)?;
        let ca = std::fs::read(&config.client_ca_file)?;
        let certificates = rustls_pemfile::certs(&mut &cert[..]).collect::<io::Result<Vec<_>>>()?;
        let key = rustls_pemfile::private_key(&mut &key[..])?.ok_or_else(invalid_tls)?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut &ca[..]) {
            roots.add(cert?).map_err(|_| invalid_tls())?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .map_err(|_| invalid_tls())?;
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| invalid_tls())?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certificates, key)
            .map_err(|_| invalid_tls())?;
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self {
            listener,
            tls: TlsAcceptor::from(Arc::new(tls)),
            config: Arc::new(config),
            pending: tokio::task::JoinSet::new(),
        })
    }
}
impl axum::serve::Listener for MtlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = PeerInfo;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                connection = self.listener.accept(), if self.pending.len() < 64 => {
                    if let Ok((stream, _)) = connection {
                        let tls = self.tls.clone(); let config = self.config.clone();
                        self.pending.spawn(async move {
                            let stream = tokio::time::timeout(Duration::from_secs(5), tls.accept(stream)).await.ok()?.ok()?;
                            let certificate = stream.get_ref().1.peer_certificates()?.first()?;
                            let identity = config.peer(certificate.as_ref()).ok()?;
                            Some((stream, PeerInfo { identity: Some(identity) }))
                        });
                    } else { tokio::time::sleep(Duration::from_millis(100)).await; }
                }
                result = self.pending.join_next(), if !self.pending.is_empty() => {
                    if let Some(Ok(Some(connection))) = result { return connection; }
                }
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()?;
        Ok(PeerInfo { identity: None })
    }
}
impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, MtlsListener>>
    for PeerInfo
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, MtlsListener>) -> Self {
        stream.remote_addr().clone()
    }
}
pub fn grpc_tls(config: &PlatformConfig) -> io::Result<tonic::transport::ServerTlsConfig> {
    Ok(tonic::transport::ServerTlsConfig::new()
        .identity(tonic::transport::Identity::from_pem(
            std::fs::read(&config.certificate_file)?,
            std::fs::read(&config.private_key_file)?,
        ))
        .client_ca_root(tonic::transport::Certificate::from_pem(std::fs::read(
            &config.client_ca_file,
        )?)))
}
pub fn grpc_peer<T>(
    state: &AppState,
    request: &tonic::Request<T>,
) -> Result<Option<AuthenticatedPeer>, tonic::Status> {
    let Some(config) = &state.config.platform else {
        return Ok(None);
    };
    let certificates = request
        .peer_certs()
        .ok_or_else(|| tonic::Status::permission_denied("platform mTLS peer required"))?;
    let certificate = certificates
        .first()
        .ok_or_else(|| tonic::Status::permission_denied("platform mTLS peer required"))?;
    config
        .peer(certificate.as_ref())
        .map(Some)
        .map_err(|_| tonic::Status::permission_denied("platform peer is not enrolled"))
}

fn denied() -> Response {
    ApiError::from(KernelError::Forbidden(
        "platform profile requires an admitted peer and explicit governing authority".into(),
    ))
    .into_response()
}
/// Legacy writes that cannot install rules. Everything else requires the separate
/// authority API, including any future route not yet included in this inventory.
pub fn ordinary_write(route: &str) -> bool {
    matches!(
        route,
        "/v1/versions"
            | "/v1/versions/{version_id}/claims"
            | "/v1/versions/{version_id}/events"
            | "/v1/versions/{version_id}/promises"
            | "/v1/versions/{version_id}/promises/{key}/fulfill"
            | "/v1/versions/{version_id}/anchors"
            | "/v1/versions/{version_id}/counters"
            | "/v1/versions/{version_id}/digests"
    )
}
pub fn legacy_rpc_allowed<B>(state: &AppState, request: &axum::http::Request<B>) -> bool {
    let Some(config) = &state.config.platform else {
        return true;
    };
    let Some(tls) = request
        .extensions()
        .get::<tonic::transport::server::TlsConnectInfo<tonic::transport::server::TcpConnectInfo>>(
        )
    else {
        return false;
    };
    let Some(certificates) = tls.peer_certs() else {
        return false;
    };
    let Some(certificate) = certificates.first() else {
        return false;
    };
    let Ok(peer) = config.peer(certificate.as_ref()) else {
        return false;
    };
    let path = request.uri().path();
    if path.starts_with("/mmp.v1.ServerApiService/") {
        return true;
    } // Same REST admission below dispatch.
    let Ok(principal) = state.authenticate_principal(crate::rest::bearer(request.headers())) else {
        return false;
    };
    if !peer.0.tenants.contains(principal.tenant_id()) {
        return false;
    }
    let write = matches!(
        path,
        "/mmp.v1.CommandService/CreateVersion"
            | "/mmp.v1.CommandService/ProposeClaim"
            | "/mmp.v1.CommandService/AppendEvents"
            | "/mmp.v1.CommandService/OpenPromise"
            | "/mmp.v1.CommandService/FulfillPromise"
            | "/mmp.v1.CommandService/LockAnchor"
            | "/mmp.v1.CommandService/RecordCounts"
            | "/mmp.v1.CommandService/UpsertDigest"
    );
    let read = matches!(
        path,
        "/mmp.v1.QueryService/GetHead"
            | "/mmp.v1.QueryService/GetClaim"
            | "/mmp.v1.QueryService/SliceFacts"
            | "/mmp.v1.QueryService/GetLineage"
            | "/mmp.v1.QueryService/ListAnchors"
            | "/mmp.v1.QueryService/ListPromises"
            | "/mmp.v1.QueryService/ComposeContext"
            | "/mmp.v1.QueryService/CounterTotals"
            | "/mmp.v1.QueryService/ListDigests"
            | "/mmp.v1.RetrievalService/HybridSearch"
            | "/mmp.v1.RetrievalService/GetIndexVersion"
            | "/mmp.v1.RetrievalService/ListCollections"
            | "/mmp.v1.RetrievalService/GetCollection"
    );
    (write || read)
        && peer
            .0
            .scopes
            .contains(if write { "record" } else { "read" })
}
pub async fn admit(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    if state.platform.is_none() {
        return next.run(request).await;
    }
    let peer = request
        .extensions()
        .get::<AuthenticatedPeer>()
        .cloned()
        .or_else(|| {
            request
                .extensions()
                .get::<ConnectInfo<PeerInfo>>()
                .and_then(|p| p.0.identity.clone())
        });
    let Some(peer) = peer else {
        return denied();
    };
    request.extensions_mut().insert(peer.clone());
    let path = request.uri().path();
    if path.starts_with("/v1/platform/") {
        return next.run(request).await;
    }
    if matches!(
        path,
        "/healthz" | "/readyz" | "/version" | "/openapi.json" | "/docs"
    ) && request.method() == axum::http::Method::GET
    {
        return next.run(request).await;
    }
    let principal = match state.authenticate_principal(crate::rest::bearer(request.headers())) {
        Ok(principal) => principal,
        Err(_) => return denied(),
    };
    if !peer.0.tenants.contains(principal.tenant_id()) {
        return denied();
    }
    let read = matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    );
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str())
        .unwrap_or("");
    if (!read && !ordinary_write(route))
        || !peer.0.scopes.contains(if read { "read" } else { "record" })
    {
        return denied();
    }
    next.run(request).await
}

/// Creating ordinary versions can inherit policy, but cannot install or replace it.
pub async fn ordinary_version(
    state: &AppState,
    store: &dyn munarium_core::storage::StorageBackend,
    parent: Option<&str>,
    metadata: Option<&serde_json::Value>,
) -> Result<(), KernelError> {
    if state.platform.is_none() {
        return Ok(());
    }
    if metadata.is_some_and(|m| m.get("governance_transition").is_some()) {
        return Err(KernelError::Forbidden(
            "governance transitions require platform authority".into(),
        ));
    }
    if metadata.is_some_and(|m| m.get("governance_policy").is_some()) {
        use munarium_core::governance::GovernancePolicy;
        let parent = parent.ok_or_else(|| {
            KernelError::Forbidden("initial governance requires platform authority".into())
        })?;
        let inherited = store.version_metadata(parent).await?;
        if GovernancePolicy::from_metadata(metadata)?
            != GovernancePolicy::from_stored_metadata(inherited.as_ref())?
        {
            return Err(KernelError::Forbidden(
                "changed governance requires platform authority".into(),
            ));
        }
    }
    Ok(())
}
