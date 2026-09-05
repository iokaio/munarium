// SPDX-License-Identifier: Apache-2.0
//! The gRPC data plane: `matrix.v1.MatrixQuery`, one server-streaming
//! `Execute`, on its own port.
//!
//! Three rules, each with a scenario in the `grpc` conformance tier:
//!
//! - **A refusal is an answer.** It rides the stream as a `Refusal` message
//!   and the call completes OK, exactly as REST returns it in a 200 body.
//!   gRPC status codes are for the transport and the caller's own mistakes —
//!   UNAUTHENTICATED, UNIMPLEMENTED for a role that does not serve this
//!   plane, INVALID_ARGUMENT for an intent that does not convert,
//!   DEADLINE_EXCEEDED.
//! - **The path is the REST path.** Both planes call
//!   [`crate::execute::execute_intent`]; the only thing this file adds is
//!   turning its progress calls into events.
//! - **Cancellation is native.** The execution runs in its own task and the
//!   stream owns that task; when the client drops the stream, the task is
//!   aborted at its next await. A budget unit reserved for it is reclaimed by
//!   the sweep, which is the documented failure direction.

use crate::state::AppState;
use munarium_matrix_proto::v1::matrix_query_server::{MatrixQuery, MatrixQueryServer};
use munarium_matrix_proto::v1::{execute_event::Event, ExecuteEvent, ExecuteRequest};
use munarium_matrix_proto::{convert, v1 as pb};
use munarium_matrix_types::contract::QueryIntent;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

pub struct MatrixQuerySvc {
    state: Arc<AppState>,
}

impl MatrixQuerySvc {
    pub fn server(state: Arc<AppState>) -> MatrixQueryServer<Self> {
        MatrixQueryServer::new(Self { state })
    }
}

/// A stream that aborts the task feeding it when it is dropped. This is what
/// makes a client's cancellation reach the execution rather than leaving it
/// to run to completion for nobody.
struct AbortOnDrop {
    inner: tokio_stream::wrappers::ReceiverStream<Result<ExecuteEvent, Status>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Stream for AbortOnDrop {
    type Item = Result<ExecuteEvent, Status>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn metadata_str<'a>(req: &'a Request<ExecuteRequest>, key: &str) -> Option<&'a str> {
    req.metadata().get(key).and_then(|v| v.to_str().ok())
}

#[tonic::async_trait]
impl MatrixQuery for MatrixQuerySvc {
    type ExecuteStream = Pin<Box<dyn Stream<Item = Result<ExecuteEvent, Status>> + Send>>;

    async fn execute(
        &self,
        req: Request<ExecuteRequest>,
    ) -> Result<Response<Self::ExecuteStream>, Status> {
        // Structural, like the REST router: a container that does not serve
        // the query plane does not have this service, and says so with the
        // status that means "not here", not with a refusal.
        if !self.state.config.role.serves_query() {
            return Err(Status::unimplemented(format!(
                "role '{:?}' does not serve the query plane",
                self.state.config.role
            )));
        }

        let bearer = metadata_str(&req, "authorization").and_then(|v| v.strip_prefix("Bearer "));
        let caller = self
            .state
            .authenticate(bearer)
            .map_err(|p| Status::unauthenticated(p.detail))?;
        caller
            .require_rw()
            .map_err(|p| Status::permission_denied(p.detail))?;
        let request_id = metadata_str(&req, "x-munarium-request-id").map(String::from);

        let inner = req.into_inner();
        let intent: QueryIntent = inner
            .intent
            .ok_or_else(|| Status::invalid_argument("intent is required"))?
            .try_into()
            .map_err(|e: convert::ConvertError| Status::invalid_argument(e.0))?;
        let name = inner.contract;
        if name.is_empty() {
            return Err(Status::invalid_argument("contract is required"));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ExecuteEvent, Status>>(32);
        let state = self.state.clone();
        let handle = tokio::spawn(async move {
            let _ = tx.send(Ok(convert::progress("authenticated"))).await;
            let ptx = tx.clone();
            let outcome = crate::execute::execute_intent(
                &state,
                &caller,
                &name,
                &intent,
                request_id,
                "grpc",
                |stage| {
                    // Progress is best-effort: a client that is not reading
                    // fast enough loses a stage, never the answer.
                    let _ = ptx.try_send(Ok(convert::progress(stage)));
                },
            )
            .await;
            let terminal = match outcome {
                Ok(block) => match pb::EvidenceBlock::try_from(&block) {
                    Ok(b) => Ok(ExecuteEvent {
                        event: Some(Event::Block(b)),
                    }),
                    Err(e) => Err(Status::internal(format!(
                        "the block did not convert to the wire form: {e}"
                    ))),
                },
                Err(refusal) => Ok(ExecuteEvent {
                    event: Some(Event::Refusal((&refusal).into())),
                }),
            };
            let _ = tx.send(terminal).await;
        });

        Ok(Response::new(Box::pin(AbortOnDrop {
            inner: tokio_stream::wrappers::ReceiverStream::new(rx),
            handle,
        })))
    }
}

/// The tonic server for this plane: the query service, health, reflection.
pub async fn serve(
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), tonic::transport::Error> {
    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<MatrixQueryServer<MatrixQuerySvc>>()
        .await;
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(munarium_matrix_proto::v1::FILE_DESCRIPTOR_SET)
        // Health too, or reflection lists a plane that answers and an operator
        // cannot reach it: `grpcurl -plaintext host:50151 grpc.health.v1.Health/Check`
        // resolves the method through reflection and got "target server does
        // not expose service" while the service was, in fact, serving. Found
        // by installing the Helm chart on a real cluster (2026-08-30) and
        // running the command the gRPC guide prints.
        .register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET)
        .build_v1()
        .expect("the embedded descriptor set is valid");

    tonic::transport::Server::builder()
        .add_service(health_service)
        .add_service(reflection)
        .add_service(MatrixQuerySvc::server(state))
        .serve_with_shutdown(addr, shutdown)
        .await
}
