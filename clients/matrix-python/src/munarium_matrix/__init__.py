# SPDX-License-Identifier: Apache-2.0
"""Munarium Matrix — the Python client for the structured-evidence plane.

The client for Matrix. It speaks Matrix's REST API and nothing
else: there is no gRPC transport here, because Matrix's gRPC plane serves
`Execute` alone and `Execute` is service-to-service — the server calls it, not
an application. When that changes, this package grows a transport rather than
a second client.

What it deliberately does NOT do, mirroring the Rust client:

* **No sealing.** A manifest is a statement about work the *sealer* did. An
  SDK offering `seal_evidence` would invite an application to assert
  provenance it cannot vouch for. Evidence is read through the *server's*
  client, not this one.
* **No local validation.** `validate()` posts the YAML and returns Matrix's
  own findings. A client that carried its own copy of the rules would drift
  from the service that enforces them, and the drift would show up as an
  asset that validates here and is refused there.
* **No SQL.** Nothing in this surface takes a statement.

    from munarium_matrix import MatrixClient

    mx = MatrixClient("https://matrix.example", token="...")
    mx.apply(open("datasource.crm.yaml").read())
    outcome = mx.verify("open-pipeline-by-region")
    if outcome.failed:
        raise SystemExit(3)   # the same exit discipline as mxctl
"""

from ._client import (
    ApplyOutcome,
    AsyncMatrixClient,
    JobAccepted,
    MatrixClient,
    MatrixError,
    PromotionGates,
    PromotionStatus,
    Validation,
    ValidationFinding,
    VerifiedQuestion,
    VerifyOutcome,
    Version,
)

__all__ = [
    "ApplyOutcome",
    "AsyncMatrixClient",
    "JobAccepted",
    "MatrixClient",
    "MatrixError",
    "PromotionGates",
    "PromotionStatus",
    "Validation",
    "ValidationFinding",
    "VerifiedQuestion",
    "VerifyOutcome",
    "Version",
]

__version__ = "1.0.0"
