from munarium_client._proto.mmp.v1 import common_pb2 as _common_pb2
from munarium_client._proto.mmp.v1 import ledger_pb2 as _ledger_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class CreateVersionRequest(_message.Message):
    __slots__ = ("parent_version_id", "metadata_json")
    PARENT_VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    METADATA_JSON_FIELD_NUMBER: _ClassVar[int]
    parent_version_id: str
    metadata_json: str
    def __init__(self, parent_version_id: _Optional[str] = ..., metadata_json: _Optional[str] = ...) -> None: ...

class CreateVersionResponse(_message.Message):
    __slots__ = ("version_id",)
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    def __init__(self, version_id: _Optional[str] = ...) -> None: ...

class ProposeClaimRequest(_message.Message):
    __slots__ = ("version_id", "expected_head", "claim_type", "subject", "key", "value", "scope_path", "provenance", "supersedes_id", "entity_id", "evidence_json", "confidence", "shape_ref", "origin")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_HEAD_FIELD_NUMBER: _ClassVar[int]
    CLAIM_TYPE_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PATH_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_FIELD_NUMBER: _ClassVar[int]
    SUPERSEDES_ID_FIELD_NUMBER: _ClassVar[int]
    ENTITY_ID_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_JSON_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    expected_head: int
    claim_type: _ledger_pb2.ClaimType
    subject: str
    key: str
    value: str
    scope_path: str
    provenance: _ledger_pb2.Provenance
    supersedes_id: str
    entity_id: str
    evidence_json: str
    confidence: float
    shape_ref: str
    origin: _ledger_pb2.ClaimOrigin
    def __init__(self, version_id: _Optional[str] = ..., expected_head: _Optional[int] = ..., claim_type: _Optional[_Union[_ledger_pb2.ClaimType, str]] = ..., subject: _Optional[str] = ..., key: _Optional[str] = ..., value: _Optional[str] = ..., scope_path: _Optional[str] = ..., provenance: _Optional[_Union[_ledger_pb2.Provenance, str]] = ..., supersedes_id: _Optional[str] = ..., entity_id: _Optional[str] = ..., evidence_json: _Optional[str] = ..., confidence: _Optional[float] = ..., shape_ref: _Optional[str] = ..., origin: _Optional[_Union[_ledger_pb2.ClaimOrigin, _Mapping]] = ...) -> None: ...

class ProposeClaimResponse(_message.Message):
    __slots__ = ("claim", "findings", "head_seq")
    CLAIM_FIELD_NUMBER: _ClassVar[int]
    FINDINGS_FIELD_NUMBER: _ClassVar[int]
    HEAD_SEQ_FIELD_NUMBER: _ClassVar[int]
    claim: _ledger_pb2.Claim
    findings: _containers.RepeatedCompositeFieldContainer[_common_pb2.GateFinding]
    head_seq: int
    def __init__(self, claim: _Optional[_Union[_ledger_pb2.Claim, _Mapping]] = ..., findings: _Optional[_Iterable[_Union[_common_pb2.GateFinding, _Mapping]]] = ..., head_seq: _Optional[int] = ...) -> None: ...

class AppendEventsRequest(_message.Message):
    __slots__ = ("version_id", "expected_head", "claims", "candidate_text")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_HEAD_FIELD_NUMBER: _ClassVar[int]
    CLAIMS_FIELD_NUMBER: _ClassVar[int]
    CANDIDATE_TEXT_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    expected_head: int
    claims: _containers.RepeatedCompositeFieldContainer[ProposeClaimRequest]
    candidate_text: str
    def __init__(self, version_id: _Optional[str] = ..., expected_head: _Optional[int] = ..., claims: _Optional[_Iterable[_Union[ProposeClaimRequest, _Mapping]]] = ..., candidate_text: _Optional[str] = ...) -> None: ...

class AppendEventsResponse(_message.Message):
    __slots__ = ("claims", "findings", "head_seq")
    CLAIMS_FIELD_NUMBER: _ClassVar[int]
    FINDINGS_FIELD_NUMBER: _ClassVar[int]
    HEAD_SEQ_FIELD_NUMBER: _ClassVar[int]
    claims: _containers.RepeatedCompositeFieldContainer[_ledger_pb2.Claim]
    findings: _containers.RepeatedCompositeFieldContainer[_common_pb2.GateFinding]
    head_seq: int
    def __init__(self, claims: _Optional[_Iterable[_Union[_ledger_pb2.Claim, _Mapping]]] = ..., findings: _Optional[_Iterable[_Union[_common_pb2.GateFinding, _Mapping]]] = ..., head_seq: _Optional[int] = ...) -> None: ...

class OpenPromiseRequest(_message.Message):
    __slots__ = ("version_id", "key", "kind", "description", "origin_scope", "due_scope")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_SCOPE_FIELD_NUMBER: _ClassVar[int]
    DUE_SCOPE_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    key: str
    kind: str
    description: str
    origin_scope: str
    due_scope: str
    def __init__(self, version_id: _Optional[str] = ..., key: _Optional[str] = ..., kind: _Optional[str] = ..., description: _Optional[str] = ..., origin_scope: _Optional[str] = ..., due_scope: _Optional[str] = ...) -> None: ...

class OpenPromiseResponse(_message.Message):
    __slots__ = ("promise",)
    PROMISE_FIELD_NUMBER: _ClassVar[int]
    promise: _ledger_pb2.Promise
    def __init__(self, promise: _Optional[_Union[_ledger_pb2.Promise, _Mapping]] = ...) -> None: ...

class FulfillPromiseRequest(_message.Message):
    __slots__ = ("version_id", "key", "result_ref")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    RESULT_REF_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    key: str
    result_ref: str
    def __init__(self, version_id: _Optional[str] = ..., key: _Optional[str] = ..., result_ref: _Optional[str] = ...) -> None: ...

class FulfillPromiseResponse(_message.Message):
    __slots__ = ("fulfilled",)
    FULFILLED_FIELD_NUMBER: _ClassVar[int]
    fulfilled: bool
    def __init__(self, fulfilled: _Optional[bool] = ...) -> None: ...

class LockAnchorRequest(_message.Message):
    __slots__ = ("version_id", "subject", "key", "value", "scope_path", "evidence_json")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PATH_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_JSON_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    subject: str
    key: str
    value: str
    scope_path: str
    evidence_json: str
    def __init__(self, version_id: _Optional[str] = ..., subject: _Optional[str] = ..., key: _Optional[str] = ..., value: _Optional[str] = ..., scope_path: _Optional[str] = ..., evidence_json: _Optional[str] = ...) -> None: ...

class LockAnchorResponse(_message.Message):
    __slots__ = ("anchor",)
    ANCHOR_FIELD_NUMBER: _ClassVar[int]
    anchor: _ledger_pb2.Anchor
    def __init__(self, anchor: _Optional[_Union[_ledger_pb2.Anchor, _Mapping]] = ...) -> None: ...

class RecordCountsRequest(_message.Message):
    __slots__ = ("version_id", "key", "scope_path", "count", "budget")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PATH_FIELD_NUMBER: _ClassVar[int]
    COUNT_FIELD_NUMBER: _ClassVar[int]
    BUDGET_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    key: str
    scope_path: str
    count: int
    budget: int
    def __init__(self, version_id: _Optional[str] = ..., key: _Optional[str] = ..., scope_path: _Optional[str] = ..., count: _Optional[int] = ..., budget: _Optional[int] = ...) -> None: ...

class RecordCountsResponse(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class UpsertDigestRequest(_message.Message):
    __slots__ = ("digest",)
    DIGEST_FIELD_NUMBER: _ClassVar[int]
    digest: _ledger_pb2.Digest
    def __init__(self, digest: _Optional[_Union[_ledger_pb2.Digest, _Mapping]] = ...) -> None: ...

class UpsertDigestResponse(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...
