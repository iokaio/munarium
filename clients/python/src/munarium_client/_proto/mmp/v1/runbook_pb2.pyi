import datetime

from google.protobuf import timestamp_pb2 as _timestamp_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ApplyShapeRequest(_message.Message):
    __slots__ = ("yaml", "version_id")
    YAML_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    yaml: str
    version_id: str
    def __init__(self, yaml: _Optional[str] = ..., version_id: _Optional[str] = ...) -> None: ...

class ApplyShapeResponse(_message.Message):
    __slots__ = ("shape_ref", "event_id")
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    shape_ref: str
    event_id: str
    def __init__(self, shape_ref: _Optional[str] = ..., event_id: _Optional[str] = ...) -> None: ...

class ApplyRunbookRequest(_message.Message):
    __slots__ = ("yaml",)
    YAML_FIELD_NUMBER: _ClassVar[int]
    yaml: str
    def __init__(self, yaml: _Optional[str] = ...) -> None: ...

class ApplyRunbookResponse(_message.Message):
    __slots__ = ("runbook_ref", "event_id")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    event_id: str
    def __init__(self, runbook_ref: _Optional[str] = ..., event_id: _Optional[str] = ...) -> None: ...

class RunRunbookRequest(_message.Message):
    __slots__ = ("runbook_ref", "params_json")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    PARAMS_JSON_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    params_json: str
    def __init__(self, runbook_ref: _Optional[str] = ..., params_json: _Optional[str] = ...) -> None: ...

class RunRunbookResponse(_message.Message):
    __slots__ = ("run_id", "state")
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    run_id: str
    state: str
    def __init__(self, run_id: _Optional[str] = ..., state: _Optional[str] = ...) -> None: ...

class RunbookStepState(_message.Message):
    __slots__ = ("ordinal", "name", "state", "detail_json", "updated_at")
    ORDINAL_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    DETAIL_JSON_FIELD_NUMBER: _ClassVar[int]
    UPDATED_AT_FIELD_NUMBER: _ClassVar[int]
    ordinal: int
    name: str
    state: str
    detail_json: str
    updated_at: _timestamp_pb2.Timestamp
    def __init__(self, ordinal: _Optional[int] = ..., name: _Optional[str] = ..., state: _Optional[str] = ..., detail_json: _Optional[str] = ..., updated_at: _Optional[_Union[datetime.datetime, _timestamp_pb2.Timestamp, _Mapping]] = ...) -> None: ...

class GetRunRequest(_message.Message):
    __slots__ = ("run_id",)
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    run_id: str
    def __init__(self, run_id: _Optional[str] = ...) -> None: ...

class GetRunResponse(_message.Message):
    __slots__ = ("run_id", "runbook_ref", "state", "steps", "version_id")
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    STEPS_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    run_id: str
    runbook_ref: str
    state: str
    steps: _containers.RepeatedCompositeFieldContainer[RunbookStepState]
    version_id: str
    def __init__(self, run_id: _Optional[str] = ..., runbook_ref: _Optional[str] = ..., state: _Optional[str] = ..., steps: _Optional[_Iterable[_Union[RunbookStepState, _Mapping]]] = ..., version_id: _Optional[str] = ...) -> None: ...

class ApproveStepRequest(_message.Message):
    __slots__ = ("run_id", "step_ordinal", "note")
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    STEP_ORDINAL_FIELD_NUMBER: _ClassVar[int]
    NOTE_FIELD_NUMBER: _ClassVar[int]
    run_id: str
    step_ordinal: int
    note: str
    def __init__(self, run_id: _Optional[str] = ..., step_ordinal: _Optional[int] = ..., note: _Optional[str] = ...) -> None: ...

class ApproveStepResponse(_message.Message):
    __slots__ = ("event_id", "state")
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    event_id: str
    state: str
    def __init__(self, event_id: _Optional[str] = ..., state: _Optional[str] = ...) -> None: ...

class ListRunbooksRequest(_message.Message):
    __slots__ = ("include_removed",)
    INCLUDE_REMOVED_FIELD_NUMBER: _ClassVar[int]
    include_removed: bool
    def __init__(self, include_removed: _Optional[bool] = ...) -> None: ...

class RunbookCollectionInfo(_message.Message):
    __slots__ = ("name", "collection_id", "shape_ref", "access_level", "compartments", "active_index", "source_count")
    NAME_FIELD_NUMBER: _ClassVar[int]
    COLLECTION_ID_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COMPARTMENTS_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_INDEX_FIELD_NUMBER: _ClassVar[int]
    SOURCE_COUNT_FIELD_NUMBER: _ClassVar[int]
    name: str
    collection_id: str
    shape_ref: str
    access_level: int
    compartments: _containers.RepeatedScalarFieldContainer[str]
    active_index: str
    source_count: int
    def __init__(self, name: _Optional[str] = ..., collection_id: _Optional[str] = ..., shape_ref: _Optional[str] = ..., access_level: _Optional[int] = ..., compartments: _Optional[_Iterable[str]] = ..., active_index: _Optional[str] = ..., source_count: _Optional[int] = ...) -> None: ...

class RunbookSummary(_message.Message):
    __slots__ = ("runbook_ref", "name", "version", "status", "min_access_level", "collections", "created_at")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    MIN_ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COLLECTIONS_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    name: str
    version: int
    status: str
    min_access_level: int
    collections: _containers.RepeatedCompositeFieldContainer[RunbookCollectionInfo]
    created_at: str
    def __init__(self, runbook_ref: _Optional[str] = ..., name: _Optional[str] = ..., version: _Optional[int] = ..., status: _Optional[str] = ..., min_access_level: _Optional[int] = ..., collections: _Optional[_Iterable[_Union[RunbookCollectionInfo, _Mapping]]] = ..., created_at: _Optional[str] = ...) -> None: ...

class ListRunbooksResponse(_message.Message):
    __slots__ = ("runbooks",)
    RUNBOOKS_FIELD_NUMBER: _ClassVar[int]
    runbooks: _containers.RepeatedCompositeFieldContainer[RunbookSummary]
    def __init__(self, runbooks: _Optional[_Iterable[_Union[RunbookSummary, _Mapping]]] = ...) -> None: ...

class GetRunbookInfoRequest(_message.Message):
    __slots__ = ("name",)
    NAME_FIELD_NUMBER: _ClassVar[int]
    name: str
    def __init__(self, name: _Optional[str] = ...) -> None: ...

class GetRunbookInfoResponse(_message.Message):
    __slots__ = ("runbook_ref", "name", "version", "status", "collections", "versions", "models_json", "retrieval_json", "has_completion", "created_at")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    COLLECTIONS_FIELD_NUMBER: _ClassVar[int]
    VERSIONS_FIELD_NUMBER: _ClassVar[int]
    MODELS_JSON_FIELD_NUMBER: _ClassVar[int]
    RETRIEVAL_JSON_FIELD_NUMBER: _ClassVar[int]
    HAS_COMPLETION_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    name: str
    version: int
    status: str
    collections: _containers.RepeatedCompositeFieldContainer[RunbookCollectionInfo]
    versions: _containers.RepeatedScalarFieldContainer[str]
    models_json: str
    retrieval_json: str
    has_completion: bool
    created_at: str
    def __init__(self, runbook_ref: _Optional[str] = ..., name: _Optional[str] = ..., version: _Optional[int] = ..., status: _Optional[str] = ..., collections: _Optional[_Iterable[_Union[RunbookCollectionInfo, _Mapping]]] = ..., versions: _Optional[_Iterable[str]] = ..., models_json: _Optional[str] = ..., retrieval_json: _Optional[str] = ..., has_completion: _Optional[bool] = ..., created_at: _Optional[str] = ...) -> None: ...

class ValidateRunbookRequest(_message.Message):
    __slots__ = ("yaml", "suggest", "provider", "model", "tier")
    YAML_FIELD_NUMBER: _ClassVar[int]
    SUGGEST_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    TIER_FIELD_NUMBER: _ClassVar[int]
    yaml: str
    suggest: bool
    provider: str
    model: str
    tier: str
    def __init__(self, yaml: _Optional[str] = ..., suggest: _Optional[bool] = ..., provider: _Optional[str] = ..., model: _Optional[str] = ..., tier: _Optional[str] = ...) -> None: ...

class ValidationFinding(_message.Message):
    __slots__ = ("severity", "code", "message", "path")
    SEVERITY_FIELD_NUMBER: _ClassVar[int]
    CODE_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    PATH_FIELD_NUMBER: _ClassVar[int]
    severity: str
    code: str
    message: str
    path: str
    def __init__(self, severity: _Optional[str] = ..., code: _Optional[str] = ..., message: _Optional[str] = ..., path: _Optional[str] = ...) -> None: ...

class RunbookSuggestion(_message.Message):
    __slots__ = ("title", "rationale", "patch_hint")
    TITLE_FIELD_NUMBER: _ClassVar[int]
    RATIONALE_FIELD_NUMBER: _ClassVar[int]
    PATCH_HINT_FIELD_NUMBER: _ClassVar[int]
    title: str
    rationale: str
    patch_hint: str
    def __init__(self, title: _Optional[str] = ..., rationale: _Optional[str] = ..., patch_hint: _Optional[str] = ...) -> None: ...

class ValidateRunbookResponse(_message.Message):
    __slots__ = ("valid", "findings", "suggestions", "suggest_note")
    VALID_FIELD_NUMBER: _ClassVar[int]
    FINDINGS_FIELD_NUMBER: _ClassVar[int]
    SUGGESTIONS_FIELD_NUMBER: _ClassVar[int]
    SUGGEST_NOTE_FIELD_NUMBER: _ClassVar[int]
    valid: bool
    findings: _containers.RepeatedCompositeFieldContainer[ValidationFinding]
    suggestions: _containers.RepeatedCompositeFieldContainer[RunbookSuggestion]
    suggest_note: str
    def __init__(self, valid: _Optional[bool] = ..., findings: _Optional[_Iterable[_Union[ValidationFinding, _Mapping]]] = ..., suggestions: _Optional[_Iterable[_Union[RunbookSuggestion, _Mapping]]] = ..., suggest_note: _Optional[str] = ...) -> None: ...

class RequestRemovalRequest(_message.Message):
    __slots__ = ("runbook_ref",)
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    def __init__(self, runbook_ref: _Optional[str] = ...) -> None: ...

class RequestRemovalResponse(_message.Message):
    __slots__ = ("runbook_ref", "removal_id", "expires_at")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    REMOVAL_ID_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    removal_id: str
    expires_at: str
    def __init__(self, runbook_ref: _Optional[str] = ..., removal_id: _Optional[str] = ..., expires_at: _Optional[str] = ...) -> None: ...

class ConfirmRemovalRequest(_message.Message):
    __slots__ = ("runbook_ref", "removal_id")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    REMOVAL_ID_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    removal_id: str
    def __init__(self, runbook_ref: _Optional[str] = ..., removal_id: _Optional[str] = ...) -> None: ...

class ConfirmRemovalResponse(_message.Message):
    __slots__ = ("runbook_ref", "status")
    RUNBOOK_REF_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    runbook_ref: str
    status: str
    def __init__(self, runbook_ref: _Optional[str] = ..., status: _Optional[str] = ...) -> None: ...
