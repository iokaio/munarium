import datetime

from google.protobuf import timestamp_pb2 as _timestamp_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Tenant(_message.Message):
    __slots__ = ("tenant_id", "slug", "cell", "created_at")
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    SLUG_FIELD_NUMBER: _ClassVar[int]
    CELL_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    slug: str
    cell: str
    created_at: _timestamp_pb2.Timestamp
    def __init__(self, tenant_id: _Optional[str] = ..., slug: _Optional[str] = ..., cell: _Optional[str] = ..., created_at: _Optional[_Union[datetime.datetime, _timestamp_pb2.Timestamp, _Mapping]] = ...) -> None: ...

class CreateTenantRequest(_message.Message):
    __slots__ = ("slug",)
    SLUG_FIELD_NUMBER: _ClassVar[int]
    slug: str
    def __init__(self, slug: _Optional[str] = ...) -> None: ...

class CreateTenantResponse(_message.Message):
    __slots__ = ("tenant",)
    TENANT_FIELD_NUMBER: _ClassVar[int]
    tenant: Tenant
    def __init__(self, tenant: _Optional[_Union[Tenant, _Mapping]] = ...) -> None: ...

class ListTenantsRequest(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class ListTenantsResponse(_message.Message):
    __slots__ = ("tenants",)
    TENANTS_FIELD_NUMBER: _ClassVar[int]
    tenants: _containers.RepeatedCompositeFieldContainer[Tenant]
    def __init__(self, tenants: _Optional[_Iterable[_Union[Tenant, _Mapping]]] = ...) -> None: ...

class UsageRequest(_message.Message):
    __slots__ = ("tenant_id",)
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    tenant_id: str
    def __init__(self, tenant_id: _Optional[str] = ...) -> None: ...

class UsageResponse(_message.Message):
    __slots__ = ("ledger_events", "sources_bytes", "invocations", "invocation_cost_usd")
    LEDGER_EVENTS_FIELD_NUMBER: _ClassVar[int]
    SOURCES_BYTES_FIELD_NUMBER: _ClassVar[int]
    INVOCATIONS_FIELD_NUMBER: _ClassVar[int]
    INVOCATION_COST_USD_FIELD_NUMBER: _ClassVar[int]
    ledger_events: int
    sources_bytes: int
    invocations: int
    invocation_cost_usd: float
    def __init__(self, ledger_events: _Optional[int] = ..., sources_bytes: _Optional[int] = ..., invocations: _Optional[int] = ..., invocation_cost_usd: _Optional[float] = ...) -> None: ...

class IssueAccessTokenRequest(_message.Message):
    __slots__ = ("uid", "access_level", "compartments", "scopes", "runbook_refs", "ttl_secs")
    UID_FIELD_NUMBER: _ClassVar[int]
    ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COMPARTMENTS_FIELD_NUMBER: _ClassVar[int]
    SCOPES_FIELD_NUMBER: _ClassVar[int]
    RUNBOOK_REFS_FIELD_NUMBER: _ClassVar[int]
    TTL_SECS_FIELD_NUMBER: _ClassVar[int]
    uid: str
    access_level: int
    compartments: _containers.RepeatedScalarFieldContainer[str]
    scopes: _containers.RepeatedScalarFieldContainer[str]
    runbook_refs: _containers.RepeatedScalarFieldContainer[str]
    ttl_secs: int
    def __init__(self, uid: _Optional[str] = ..., access_level: _Optional[int] = ..., compartments: _Optional[_Iterable[str]] = ..., scopes: _Optional[_Iterable[str]] = ..., runbook_refs: _Optional[_Iterable[str]] = ..., ttl_secs: _Optional[int] = ...) -> None: ...

class IssueAccessTokenResponse(_message.Message):
    __slots__ = ("token", "jti", "expires_at")
    TOKEN_FIELD_NUMBER: _ClassVar[int]
    JTI_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    token: str
    jti: str
    expires_at: str
    def __init__(self, token: _Optional[str] = ..., jti: _Optional[str] = ..., expires_at: _Optional[str] = ...) -> None: ...

class ListAccessTokensRequest(_message.Message):
    __slots__ = ("uid", "active")
    UID_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_FIELD_NUMBER: _ClassVar[int]
    uid: str
    active: bool
    def __init__(self, uid: _Optional[str] = ..., active: _Optional[bool] = ...) -> None: ...

class AccessTokenInfo(_message.Message):
    __slots__ = ("jti", "uid", "access_level", "compartments", "scopes", "runbook_refs", "issued_by", "issued_at", "expires_at", "revoked_at")
    JTI_FIELD_NUMBER: _ClassVar[int]
    UID_FIELD_NUMBER: _ClassVar[int]
    ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COMPARTMENTS_FIELD_NUMBER: _ClassVar[int]
    SCOPES_FIELD_NUMBER: _ClassVar[int]
    RUNBOOK_REFS_FIELD_NUMBER: _ClassVar[int]
    ISSUED_BY_FIELD_NUMBER: _ClassVar[int]
    ISSUED_AT_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    REVOKED_AT_FIELD_NUMBER: _ClassVar[int]
    jti: str
    uid: str
    access_level: int
    compartments: _containers.RepeatedScalarFieldContainer[str]
    scopes: _containers.RepeatedScalarFieldContainer[str]
    runbook_refs: _containers.RepeatedScalarFieldContainer[str]
    issued_by: str
    issued_at: str
    expires_at: str
    revoked_at: str
    def __init__(self, jti: _Optional[str] = ..., uid: _Optional[str] = ..., access_level: _Optional[int] = ..., compartments: _Optional[_Iterable[str]] = ..., scopes: _Optional[_Iterable[str]] = ..., runbook_refs: _Optional[_Iterable[str]] = ..., issued_by: _Optional[str] = ..., issued_at: _Optional[str] = ..., expires_at: _Optional[str] = ..., revoked_at: _Optional[str] = ...) -> None: ...

class ListAccessTokensResponse(_message.Message):
    __slots__ = ("tokens",)
    TOKENS_FIELD_NUMBER: _ClassVar[int]
    tokens: _containers.RepeatedCompositeFieldContainer[AccessTokenInfo]
    def __init__(self, tokens: _Optional[_Iterable[_Union[AccessTokenInfo, _Mapping]]] = ...) -> None: ...

class RevokeAccessTokenRequest(_message.Message):
    __slots__ = ("jti",)
    JTI_FIELD_NUMBER: _ClassVar[int]
    jti: str
    def __init__(self, jti: _Optional[str] = ...) -> None: ...

class RevokeAccessTokenResponse(_message.Message):
    __slots__ = ("jti", "revoked", "revocation_check_enabled")
    JTI_FIELD_NUMBER: _ClassVar[int]
    REVOKED_FIELD_NUMBER: _ClassVar[int]
    REVOCATION_CHECK_ENABLED_FIELD_NUMBER: _ClassVar[int]
    jti: str
    revoked: bool
    revocation_check_enabled: bool
    def __init__(self, jti: _Optional[str] = ..., revoked: _Optional[bool] = ..., revocation_check_enabled: _Optional[bool] = ...) -> None: ...
