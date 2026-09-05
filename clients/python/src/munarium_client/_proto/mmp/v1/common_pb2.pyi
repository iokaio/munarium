import datetime

from google.protobuf import timestamp_pb2 as _timestamp_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ClaimStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    CLAIM_STATUS_UNSPECIFIED: _ClassVar[ClaimStatus]
    CLAIM_STATUS_ACCEPTED: _ClassVar[ClaimStatus]
    CLAIM_STATUS_DISPUTED: _ClassVar[ClaimStatus]

class Severity(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    SEVERITY_UNSPECIFIED: _ClassVar[Severity]
    SEVERITY_INFO: _ClassVar[Severity]
    SEVERITY_WARN: _ClassVar[Severity]
    SEVERITY_BLOCK: _ClassVar[Severity]
CLAIM_STATUS_UNSPECIFIED: ClaimStatus
CLAIM_STATUS_ACCEPTED: ClaimStatus
CLAIM_STATUS_DISPUTED: ClaimStatus
SEVERITY_UNSPECIFIED: Severity
SEVERITY_INFO: Severity
SEVERITY_WARN: Severity
SEVERITY_BLOCK: Severity

class TenantRef(_message.Message):
    __slots__ = ("tenant_id",)
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    def __init__(self, tenant_id: _Optional[str] = ...) -> None: ...

class VersionRef(_message.Message):
    __slots__ = ("version_id",)
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    def __init__(self, version_id: _Optional[str] = ...) -> None: ...

class GateFinding(_message.Message):
    __slots__ = ("rule_id", "severity", "message", "scope_path", "detail_json")
    RULE_ID_FIELD_NUMBER: _ClassVar[int]
    SEVERITY_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PATH_FIELD_NUMBER: _ClassVar[int]
    DETAIL_JSON_FIELD_NUMBER: _ClassVar[int]
    rule_id: str
    severity: Severity
    message: str
    scope_path: str
    detail_json: str
    def __init__(self, rule_id: _Optional[str] = ..., severity: _Optional[_Union[Severity, str]] = ..., message: _Optional[str] = ..., scope_path: _Optional[str] = ..., detail_json: _Optional[str] = ...) -> None: ...

class PolicyRejection(_message.Message):
    __slots__ = ("problem_type", "detail", "findings", "policy_citation")
    PROBLEM_TYPE_FIELD_NUMBER: _ClassVar[int]
    DETAIL_FIELD_NUMBER: _ClassVar[int]
    FINDINGS_FIELD_NUMBER: _ClassVar[int]
    POLICY_CITATION_FIELD_NUMBER: _ClassVar[int]
    problem_type: str
    detail: str
    findings: _containers.RepeatedCompositeFieldContainer[GateFinding]
    policy_citation: str
    def __init__(self, problem_type: _Optional[str] = ..., detail: _Optional[str] = ..., findings: _Optional[_Iterable[_Union[GateFinding, _Mapping]]] = ..., policy_citation: _Optional[str] = ...) -> None: ...

class PageRequest(_message.Message):
    __slots__ = ("page_size", "page_token")
    PAGE_SIZE_FIELD_NUMBER: _ClassVar[int]
    PAGE_TOKEN_FIELD_NUMBER: _ClassVar[int]
    page_size: int
    page_token: str
    def __init__(self, page_size: _Optional[int] = ..., page_token: _Optional[str] = ...) -> None: ...

class PageResponse(_message.Message):
    __slots__ = ("next_page_token",)
    NEXT_PAGE_TOKEN_FIELD_NUMBER: _ClassVar[int]
    next_page_token: str
    def __init__(self, next_page_token: _Optional[str] = ...) -> None: ...

class ProvenanceEnvelope(_message.Message):
    __slots__ = ("chunk_ids", "source_content_hashes", "index_version", "event_watermark", "provider_fingerprint", "generated_at", "source_ids", "source_paths")
    CHUNK_IDS_FIELD_NUMBER: _ClassVar[int]
    SOURCE_CONTENT_HASHES_FIELD_NUMBER: _ClassVar[int]
    INDEX_VERSION_FIELD_NUMBER: _ClassVar[int]
    EVENT_WATERMARK_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FINGERPRINT_FIELD_NUMBER: _ClassVar[int]
    GENERATED_AT_FIELD_NUMBER: _ClassVar[int]
    SOURCE_IDS_FIELD_NUMBER: _ClassVar[int]
    SOURCE_PATHS_FIELD_NUMBER: _ClassVar[int]
    chunk_ids: _containers.RepeatedScalarFieldContainer[str]
    source_content_hashes: _containers.RepeatedScalarFieldContainer[str]
    index_version: str
    event_watermark: int
    provider_fingerprint: str
    generated_at: _timestamp_pb2.Timestamp
    source_ids: _containers.RepeatedScalarFieldContainer[str]
    source_paths: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, chunk_ids: _Optional[_Iterable[str]] = ..., source_content_hashes: _Optional[_Iterable[str]] = ..., index_version: _Optional[str] = ..., event_watermark: _Optional[int] = ..., provider_fingerprint: _Optional[str] = ..., generated_at: _Optional[_Union[datetime.datetime, _timestamp_pb2.Timestamp, _Mapping]] = ..., source_ids: _Optional[_Iterable[str]] = ..., source_paths: _Optional[_Iterable[str]] = ...) -> None: ...
