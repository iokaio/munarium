import datetime

from google.protobuf import timestamp_pb2 as _timestamp_pb2
from munarium_client._proto.mmp.v1 import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ClaimType(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    CLAIM_TYPE_UNSPECIFIED: _ClassVar[ClaimType]
    CLAIM_TYPE_FACT: _ClassVar[ClaimType]
    CLAIM_TYPE_UPDATE: _ClassVar[ClaimType]
    CLAIM_TYPE_CORRECTION: _ClassVar[ClaimType]

class Provenance(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    PROVENANCE_UNSPECIFIED: _ClassVar[Provenance]
    PROVENANCE_WITNESSED: _ClassVar[Provenance]
    PROVENANCE_BACKFILLED: _ClassVar[Provenance]
    PROVENANCE_REPAIRED: _ClassVar[Provenance]
    PROVENANCE_EMERGENT: _ClassVar[Provenance]
    PROVENANCE_COVERAGE_REPAIR: _ClassVar[Provenance]
CLAIM_TYPE_UNSPECIFIED: ClaimType
CLAIM_TYPE_FACT: ClaimType
CLAIM_TYPE_UPDATE: ClaimType
CLAIM_TYPE_CORRECTION: ClaimType
PROVENANCE_UNSPECIFIED: Provenance
PROVENANCE_WITNESSED: Provenance
PROVENANCE_BACKFILLED: Provenance
PROVENANCE_REPAIRED: Provenance
PROVENANCE_EMERGENT: Provenance
PROVENANCE_COVERAGE_REPAIR: Provenance

class ClaimOrigin(_message.Message):
    __slots__ = ("kind", "source_id", "mapping_version", "row_key", "event_position", "observed_at", "evidence_id")
    KIND_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    MAPPING_VERSION_FIELD_NUMBER: _ClassVar[int]
    ROW_KEY_FIELD_NUMBER: _ClassVar[int]
    EVENT_POSITION_FIELD_NUMBER: _ClassVar[int]
    OBSERVED_AT_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_ID_FIELD_NUMBER: _ClassVar[int]
    kind: str
    source_id: str
    mapping_version: str
    row_key: str
    event_position: str
    observed_at: str
    evidence_id: str
    def __init__(self, kind: _Optional[str] = ..., source_id: _Optional[str] = ..., mapping_version: _Optional[str] = ..., row_key: _Optional[str] = ..., event_position: _Optional[str] = ..., observed_at: _Optional[str] = ..., evidence_id: _Optional[str] = ...) -> None: ...

class Claim(_message.Message):
    __slots__ = ("id", "version_id", "seq", "claim_type", "subject", "key", "value", "normalized_text", "scope_path", "status", "provenance", "supersedes_id", "entity_id", "evidence_json", "confidence", "shape_ref", "recorded_at", "origin")
    ID_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    CLAIM_TYPE_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    NORMALIZED_TEXT_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PATH_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_FIELD_NUMBER: _ClassVar[int]
    SUPERSEDES_ID_FIELD_NUMBER: _ClassVar[int]
    ENTITY_ID_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_JSON_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    RECORDED_AT_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_FIELD_NUMBER: _ClassVar[int]
    id: str
    version_id: str
    seq: int
    claim_type: ClaimType
    subject: str
    key: str
    value: str
    normalized_text: str
    scope_path: str
    status: _common_pb2.ClaimStatus
    provenance: Provenance
    supersedes_id: str
    entity_id: str
    evidence_json: str
    confidence: float
    shape_ref: str
    recorded_at: _timestamp_pb2.Timestamp
    origin: ClaimOrigin
    def __init__(self, id: _Optional[str] = ..., version_id: _Optional[str] = ..., seq: _Optional[int] = ..., claim_type: _Optional[_Union[ClaimType, str]] = ..., subject: _Optional[str] = ..., key: _Optional[str] = ..., value: _Optional[str] = ..., normalized_text: _Optional[str] = ..., scope_path: _Optional[str] = ..., status: _Optional[_Union[_common_pb2.ClaimStatus, str]] = ..., provenance: _Optional[_Union[Provenance, str]] = ..., supersedes_id: _Optional[str] = ..., entity_id: _Optional[str] = ..., evidence_json: _Optional[str] = ..., confidence: _Optional[float] = ..., shape_ref: _Optional[str] = ..., recorded_at: _Optional[_Union[datetime.datetime, _timestamp_pb2.Timestamp, _Mapping]] = ..., origin: _Optional[_Union[ClaimOrigin, _Mapping]] = ...) -> None: ...

class FactSlice(_message.Message):
    __slots__ = ("facts", "as_of_seq", "head_seq")
    FACTS_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    HEAD_SEQ_FIELD_NUMBER: _ClassVar[int]
    facts: _containers.RepeatedCompositeFieldContainer[Claim]
    as_of_seq: int
    head_seq: int
    def __init__(self, facts: _Optional[_Iterable[_Union[Claim, _Mapping]]] = ..., as_of_seq: _Optional[int] = ..., head_seq: _Optional[int] = ...) -> None: ...

class Anchor(_message.Message):
    __slots__ = ("id", "version_id", "detail_key", "locked_value", "locked_at_scope", "status", "seq")
    ID_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    DETAIL_KEY_FIELD_NUMBER: _ClassVar[int]
    LOCKED_VALUE_FIELD_NUMBER: _ClassVar[int]
    LOCKED_AT_SCOPE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    id: str
    version_id: str
    detail_key: str
    locked_value: str
    locked_at_scope: str
    status: str
    seq: int
    def __init__(self, id: _Optional[str] = ..., version_id: _Optional[str] = ..., detail_key: _Optional[str] = ..., locked_value: _Optional[str] = ..., locked_at_scope: _Optional[str] = ..., status: _Optional[str] = ..., seq: _Optional[int] = ...) -> None: ...

class Promise(_message.Message):
    __slots__ = ("id", "version_id", "key", "kind", "description", "origin_scope", "due_scope", "status", "seq", "fulfilled_seq")
    ID_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_SCOPE_FIELD_NUMBER: _ClassVar[int]
    DUE_SCOPE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    FULFILLED_SEQ_FIELD_NUMBER: _ClassVar[int]
    id: str
    version_id: str
    key: str
    kind: str
    description: str
    origin_scope: str
    due_scope: str
    status: str
    seq: int
    fulfilled_seq: int
    def __init__(self, id: _Optional[str] = ..., version_id: _Optional[str] = ..., key: _Optional[str] = ..., kind: _Optional[str] = ..., description: _Optional[str] = ..., origin_scope: _Optional[str] = ..., due_scope: _Optional[str] = ..., status: _Optional[str] = ..., seq: _Optional[int] = ..., fulfilled_seq: _Optional[int] = ...) -> None: ...

class CounterState(_message.Message):
    __slots__ = ("key", "total", "budget")
    KEY_FIELD_NUMBER: _ClassVar[int]
    TOTAL_FIELD_NUMBER: _ClassVar[int]
    BUDGET_FIELD_NUMBER: _ClassVar[int]
    key: str
    total: int
    budget: int
    def __init__(self, key: _Optional[str] = ..., total: _Optional[int] = ..., budget: _Optional[int] = ...) -> None: ...

class Lineage(_message.Message):
    __slots__ = ("version_ids",)
    VERSION_IDS_FIELD_NUMBER: _ClassVar[int]
    version_ids: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, version_ids: _Optional[_Iterable[str]] = ...) -> None: ...

class Digest(_message.Message):
    __slots__ = ("version_id", "tier", "scope_path", "content", "content_hash", "built_from_seq")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    TIER_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PATH_FIELD_NUMBER: _ClassVar[int]
    CONTENT_FIELD_NUMBER: _ClassVar[int]
    CONTENT_HASH_FIELD_NUMBER: _ClassVar[int]
    BUILT_FROM_SEQ_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    tier: int
    scope_path: str
    content: str
    content_hash: str
    built_from_seq: int
    def __init__(self, version_id: _Optional[str] = ..., tier: _Optional[int] = ..., scope_path: _Optional[str] = ..., content: _Optional[str] = ..., content_hash: _Optional[str] = ..., built_from_seq: _Optional[int] = ...) -> None: ...

class ComposedContext(_message.Message):
    __slots__ = ("sections", "text", "estimated_tokens", "content_hash", "as_of_seq")
    class Section(_message.Message):
        __slots__ = ("title", "body")
        TITLE_FIELD_NUMBER: _ClassVar[int]
        BODY_FIELD_NUMBER: _ClassVar[int]
        title: str
        body: str
        def __init__(self, title: _Optional[str] = ..., body: _Optional[str] = ...) -> None: ...
    SECTIONS_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    ESTIMATED_TOKENS_FIELD_NUMBER: _ClassVar[int]
    CONTENT_HASH_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    sections: _containers.RepeatedCompositeFieldContainer[ComposedContext.Section]
    text: str
    estimated_tokens: int
    content_hash: str
    as_of_seq: int
    def __init__(self, sections: _Optional[_Iterable[_Union[ComposedContext.Section, _Mapping]]] = ..., text: _Optional[str] = ..., estimated_tokens: _Optional[int] = ..., content_hash: _Optional[str] = ..., as_of_seq: _Optional[int] = ...) -> None: ...
