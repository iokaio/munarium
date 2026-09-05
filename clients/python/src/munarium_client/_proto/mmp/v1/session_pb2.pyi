from munarium_client._proto.mmp.v1 import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class CreateSessionRequest(_message.Message):
    __slots__ = ("runbook_name",)
    RUNBOOK_NAME_FIELD_NUMBER: _ClassVar[int]
    runbook_name: str
    def __init__(self, runbook_name: _Optional[str] = ...) -> None: ...

class CreateSessionResponse(_message.Message):
    __slots__ = ("session_id", "runbook_ref", "permitted_collections")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    PERMITTED_COLLECTIONS_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    runbook_ref: str
    permitted_collections: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, session_id: _Optional[str] = ..., runbook_ref: _Optional[str] = ..., permitted_collections: _Optional[_Iterable[str]] = ...) -> None: ...

class SessionModelOverride(_message.Message):
    __slots__ = ("provider", "model", "tier")
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    TIER_FIELD_NUMBER: _ClassVar[int]
    provider: str
    model: str
    tier: str
    def __init__(self, provider: _Optional[str] = ..., model: _Optional[str] = ..., tier: _Optional[str] = ...) -> None: ...

class TurnRequest(_message.Message):
    __slots__ = ("session_id", "query", "top_k", "complete", "model_override", "research_profile")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    QUERY_FIELD_NUMBER: _ClassVar[int]
    TOP_K_FIELD_NUMBER: _ClassVar[int]
    COMPLETE_FIELD_NUMBER: _ClassVar[int]
    MODEL_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    RESEARCH_PROFILE_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    query: str
    top_k: int
    complete: bool
    model_override: SessionModelOverride
    research_profile: str
    def __init__(self, session_id: _Optional[str] = ..., query: _Optional[str] = ..., top_k: _Optional[int] = ..., complete: _Optional[bool] = ..., model_override: _Optional[_Union[SessionModelOverride, _Mapping]] = ..., research_profile: _Optional[str] = ...) -> None: ...

class TurnHit(_message.Message):
    __slots__ = ("collection", "chunk_id", "source_id", "source_path", "source_content_hash", "text", "score")
    COLLECTION_FIELD_NUMBER: _ClassVar[int]
    CHUNK_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_PATH_FIELD_NUMBER: _ClassVar[int]
    SOURCE_CONTENT_HASH_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    SCORE_FIELD_NUMBER: _ClassVar[int]
    collection: str
    chunk_id: str
    source_id: str
    source_path: str
    source_content_hash: str
    text: str
    score: float
    def __init__(self, collection: _Optional[str] = ..., chunk_id: _Optional[str] = ..., source_id: _Optional[str] = ..., source_path: _Optional[str] = ..., source_content_hash: _Optional[str] = ..., text: _Optional[str] = ..., score: _Optional[float] = ...) -> None: ...

class CollectionEnvelope(_message.Message):
    __slots__ = ("collection", "envelope")
    COLLECTION_FIELD_NUMBER: _ClassVar[int]
    ENVELOPE_FIELD_NUMBER: _ClassVar[int]
    collection: str
    envelope: _common_pb2.ProvenanceEnvelope
    def __init__(self, collection: _Optional[str] = ..., envelope: _Optional[_Union[_common_pb2.ProvenanceEnvelope, _Mapping]] = ...) -> None: ...

class TurnVerification(_message.Message):
    __slots__ = ("checks", "retries", "first_pass_violations", "violations")
    CHECKS_FIELD_NUMBER: _ClassVar[int]
    RETRIES_FIELD_NUMBER: _ClassVar[int]
    FIRST_PASS_VIOLATIONS_FIELD_NUMBER: _ClassVar[int]
    VIOLATIONS_FIELD_NUMBER: _ClassVar[int]
    checks: _containers.RepeatedScalarFieldContainer[str]
    retries: int
    first_pass_violations: _containers.RepeatedScalarFieldContainer[str]
    violations: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, checks: _Optional[_Iterable[str]] = ..., retries: _Optional[int] = ..., first_pass_violations: _Optional[_Iterable[str]] = ..., violations: _Optional[_Iterable[str]] = ...) -> None: ...

class TurnCompletion(_message.Message):
    __slots__ = ("provider", "model", "was_override", "text", "input_tokens", "output_tokens", "verification")
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    WAS_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    INPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    VERIFICATION_FIELD_NUMBER: _ClassVar[int]
    provider: str
    model: str
    was_override: bool
    text: str
    input_tokens: int
    output_tokens: int
    verification: TurnVerification
    def __init__(self, provider: _Optional[str] = ..., model: _Optional[str] = ..., was_override: _Optional[bool] = ..., text: _Optional[str] = ..., input_tokens: _Optional[int] = ..., output_tokens: _Optional[int] = ..., verification: _Optional[_Union[TurnVerification, _Mapping]] = ...) -> None: ...

class TurnResponse(_message.Message):
    __slots__ = ("session_id", "ordinal", "collections_searched", "skipped", "hits", "envelopes", "completion", "hierarchy")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    ORDINAL_FIELD_NUMBER: _ClassVar[int]
    COLLECTIONS_SEARCHED_FIELD_NUMBER: _ClassVar[int]
    SKIPPED_FIELD_NUMBER: _ClassVar[int]
    HITS_FIELD_NUMBER: _ClassVar[int]
    ENVELOPES_FIELD_NUMBER: _ClassVar[int]
    COMPLETION_FIELD_NUMBER: _ClassVar[int]
    HIERARCHY_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    ordinal: int
    collections_searched: _containers.RepeatedScalarFieldContainer[str]
    skipped: _containers.RepeatedScalarFieldContainer[str]
    hits: _containers.RepeatedCompositeFieldContainer[TurnHit]
    envelopes: _containers.RepeatedCompositeFieldContainer[CollectionEnvelope]
    completion: TurnCompletion
    hierarchy: EvidenceHierarchyDecision
    def __init__(self, session_id: _Optional[str] = ..., ordinal: _Optional[int] = ..., collections_searched: _Optional[_Iterable[str]] = ..., skipped: _Optional[_Iterable[str]] = ..., hits: _Optional[_Iterable[_Union[TurnHit, _Mapping]]] = ..., envelopes: _Optional[_Iterable[_Union[CollectionEnvelope, _Mapping]]] = ..., completion: _Optional[_Union[TurnCompletion, _Mapping]] = ..., hierarchy: _Optional[_Union[EvidenceHierarchyDecision, _Mapping]] = ...) -> None: ...

class LayerOutcome(_message.Message):
    __slots__ = ("layer", "role", "requirement", "block", "evidence_id", "supports_completeness", "refusal_code", "elapsed_ms")
    LAYER_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    REQUIREMENT_FIELD_NUMBER: _ClassVar[int]
    BLOCK_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_ID_FIELD_NUMBER: _ClassVar[int]
    SUPPORTS_COMPLETENESS_FIELD_NUMBER: _ClassVar[int]
    REFUSAL_CODE_FIELD_NUMBER: _ClassVar[int]
    ELAPSED_MS_FIELD_NUMBER: _ClassVar[int]
    layer: str
    role: str
    requirement: str
    block: str
    evidence_id: str
    supports_completeness: bool
    refusal_code: str
    elapsed_ms: int
    def __init__(self, layer: _Optional[str] = ..., role: _Optional[str] = ..., requirement: _Optional[str] = ..., block: _Optional[str] = ..., evidence_id: _Optional[str] = ..., supports_completeness: _Optional[bool] = ..., refusal_code: _Optional[str] = ..., elapsed_ms: _Optional[int] = ...) -> None: ...

class EvidenceHierarchyDecision(_message.Message):
    __slots__ = ("profile", "intent_kind", "intent_explicit", "layers", "completeness_available", "disclosed_conflicts", "conflicts_policy")
    PROFILE_FIELD_NUMBER: _ClassVar[int]
    INTENT_KIND_FIELD_NUMBER: _ClassVar[int]
    INTENT_EXPLICIT_FIELD_NUMBER: _ClassVar[int]
    LAYERS_FIELD_NUMBER: _ClassVar[int]
    COMPLETENESS_AVAILABLE_FIELD_NUMBER: _ClassVar[int]
    DISCLOSED_CONFLICTS_FIELD_NUMBER: _ClassVar[int]
    CONFLICTS_POLICY_FIELD_NUMBER: _ClassVar[int]
    profile: str
    intent_kind: str
    intent_explicit: bool
    layers: _containers.RepeatedCompositeFieldContainer[LayerOutcome]
    completeness_available: bool
    disclosed_conflicts: int
    conflicts_policy: str
    def __init__(self, profile: _Optional[str] = ..., intent_kind: _Optional[str] = ..., intent_explicit: _Optional[bool] = ..., layers: _Optional[_Iterable[_Union[LayerOutcome, _Mapping]]] = ..., completeness_available: _Optional[bool] = ..., disclosed_conflicts: _Optional[int] = ..., conflicts_policy: _Optional[str] = ...) -> None: ...

class GetSessionRequest(_message.Message):
    __slots__ = ("session_id",)
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    def __init__(self, session_id: _Optional[str] = ...) -> None: ...

class SessionTurn(_message.Message):
    __slots__ = ("ordinal", "query", "collections_searched", "hits_json", "envelope_json", "completion_json", "created_at")
    ORDINAL_FIELD_NUMBER: _ClassVar[int]
    QUERY_FIELD_NUMBER: _ClassVar[int]
    COLLECTIONS_SEARCHED_FIELD_NUMBER: _ClassVar[int]
    HITS_JSON_FIELD_NUMBER: _ClassVar[int]
    ENVELOPE_JSON_FIELD_NUMBER: _ClassVar[int]
    COMPLETION_JSON_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    ordinal: int
    query: str
    collections_searched: _containers.RepeatedScalarFieldContainer[str]
    hits_json: str
    envelope_json: str
    completion_json: str
    created_at: str
    def __init__(self, ordinal: _Optional[int] = ..., query: _Optional[str] = ..., collections_searched: _Optional[_Iterable[str]] = ..., hits_json: _Optional[str] = ..., envelope_json: _Optional[str] = ..., completion_json: _Optional[str] = ..., created_at: _Optional[str] = ...) -> None: ...

class GetSessionResponse(_message.Message):
    __slots__ = ("session_id", "uid", "runbook_ref", "access_level", "compartments", "state", "created_at", "turns")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    UID_FIELD_NUMBER: _ClassVar[int]
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COMPARTMENTS_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    TURNS_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    uid: str
    runbook_ref: str
    access_level: int
    compartments: _containers.RepeatedScalarFieldContainer[str]
    state: str
    created_at: str
    turns: _containers.RepeatedCompositeFieldContainer[SessionTurn]
    def __init__(self, session_id: _Optional[str] = ..., uid: _Optional[str] = ..., runbook_ref: _Optional[str] = ..., access_level: _Optional[int] = ..., compartments: _Optional[_Iterable[str]] = ..., state: _Optional[str] = ..., created_at: _Optional[str] = ..., turns: _Optional[_Iterable[_Union[SessionTurn, _Mapping]]] = ...) -> None: ...

class CloseSessionRequest(_message.Message):
    __slots__ = ("session_id",)
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    def __init__(self, session_id: _Optional[str] = ...) -> None: ...
