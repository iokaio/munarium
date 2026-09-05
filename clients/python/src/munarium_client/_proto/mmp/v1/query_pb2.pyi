from munarium_client._proto.mmp.v1 import common_pb2 as _common_pb2
from munarium_client._proto.mmp.v1 import ledger_pb2 as _ledger_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class GetHeadRequest(_message.Message):
    __slots__ = ("version_id",)
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    def __init__(self, version_id: _Optional[str] = ...) -> None: ...

class GetHeadResponse(_message.Message):
    __slots__ = ("head_seq",)
    HEAD_SEQ_FIELD_NUMBER: _ClassVar[int]
    head_seq: int
    def __init__(self, head_seq: _Optional[int] = ...) -> None: ...

class GetClaimRequest(_message.Message):
    __slots__ = ("claim_id",)
    CLAIM_ID_FIELD_NUMBER: _ClassVar[int]
    claim_id: str
    def __init__(self, claim_id: _Optional[str] = ...) -> None: ...

class GetClaimResponse(_message.Message):
    __slots__ = ("claim", "superseded", "superseded_by")
    CLAIM_FIELD_NUMBER: _ClassVar[int]
    SUPERSEDED_FIELD_NUMBER: _ClassVar[int]
    SUPERSEDED_BY_FIELD_NUMBER: _ClassVar[int]
    claim: _ledger_pb2.Claim
    superseded: bool
    superseded_by: str
    def __init__(self, claim: _Optional[_Union[_ledger_pb2.Claim, _Mapping]] = ..., superseded: _Optional[bool] = ..., superseded_by: _Optional[str] = ...) -> None: ...

class SliceFactsRequest(_message.Message):
    __slots__ = ("version_id", "scope_prefix", "as_of_seq", "statuses", "limit")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_PREFIX_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    STATUSES_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    scope_prefix: str
    as_of_seq: int
    statuses: _containers.RepeatedScalarFieldContainer[_common_pb2.ClaimStatus]
    limit: int
    def __init__(self, version_id: _Optional[str] = ..., scope_prefix: _Optional[str] = ..., as_of_seq: _Optional[int] = ..., statuses: _Optional[_Iterable[_Union[_common_pb2.ClaimStatus, str]]] = ..., limit: _Optional[int] = ...) -> None: ...

class SliceFactsResponse(_message.Message):
    __slots__ = ("slice",)
    SLICE_FIELD_NUMBER: _ClassVar[int]
    slice: _ledger_pb2.FactSlice
    def __init__(self, slice: _Optional[_Union[_ledger_pb2.FactSlice, _Mapping]] = ...) -> None: ...

class GetLineageRequest(_message.Message):
    __slots__ = ("version_id",)
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    def __init__(self, version_id: _Optional[str] = ...) -> None: ...

class GetLineageResponse(_message.Message):
    __slots__ = ("lineage",)
    LINEAGE_FIELD_NUMBER: _ClassVar[int]
    lineage: _ledger_pb2.Lineage
    def __init__(self, lineage: _Optional[_Union[_ledger_pb2.Lineage, _Mapping]] = ...) -> None: ...

class ListAnchorsRequest(_message.Message):
    __slots__ = ("version_id", "as_of_seq")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    as_of_seq: int
    def __init__(self, version_id: _Optional[str] = ..., as_of_seq: _Optional[int] = ...) -> None: ...

class ListAnchorsResponse(_message.Message):
    __slots__ = ("anchors",)
    ANCHORS_FIELD_NUMBER: _ClassVar[int]
    anchors: _containers.RepeatedCompositeFieldContainer[_ledger_pb2.Anchor]
    def __init__(self, anchors: _Optional[_Iterable[_Union[_ledger_pb2.Anchor, _Mapping]]] = ...) -> None: ...

class ListPromisesRequest(_message.Message):
    __slots__ = ("version_id", "status", "as_of_seq")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    status: str
    as_of_seq: int
    def __init__(self, version_id: _Optional[str] = ..., status: _Optional[str] = ..., as_of_seq: _Optional[int] = ...) -> None: ...

class ListPromisesResponse(_message.Message):
    __slots__ = ("promises",)
    PROMISES_FIELD_NUMBER: _ClassVar[int]
    promises: _containers.RepeatedCompositeFieldContainer[_ledger_pb2.Promise]
    def __init__(self, promises: _Optional[_Iterable[_Union[_ledger_pb2.Promise, _Mapping]]] = ...) -> None: ...

class ComposeContextRequest(_message.Message):
    __slots__ = ("version_id", "scope", "budget_tokens", "fact_limit", "as_of_seq", "as_of_date")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    BUDGET_TOKENS_FIELD_NUMBER: _ClassVar[int]
    FACT_LIMIT_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    AS_OF_DATE_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    scope: str
    budget_tokens: int
    fact_limit: int
    as_of_seq: int
    as_of_date: str
    def __init__(self, version_id: _Optional[str] = ..., scope: _Optional[str] = ..., budget_tokens: _Optional[int] = ..., fact_limit: _Optional[int] = ..., as_of_seq: _Optional[int] = ..., as_of_date: _Optional[str] = ...) -> None: ...

class ComposeContextResponse(_message.Message):
    __slots__ = ("context",)
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    context: _ledger_pb2.ComposedContext
    def __init__(self, context: _Optional[_Union[_ledger_pb2.ComposedContext, _Mapping]] = ...) -> None: ...

class CounterTotalsRequest(_message.Message):
    __slots__ = ("version_id", "as_of_seq")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    AS_OF_SEQ_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    as_of_seq: int
    def __init__(self, version_id: _Optional[str] = ..., as_of_seq: _Optional[int] = ...) -> None: ...

class CounterTotalsResponse(_message.Message):
    __slots__ = ("counters",)
    COUNTERS_FIELD_NUMBER: _ClassVar[int]
    counters: _containers.RepeatedCompositeFieldContainer[_ledger_pb2.CounterState]
    def __init__(self, counters: _Optional[_Iterable[_Union[_ledger_pb2.CounterState, _Mapping]]] = ...) -> None: ...

class ListDigestsRequest(_message.Message):
    __slots__ = ("version_id",)
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    def __init__(self, version_id: _Optional[str] = ...) -> None: ...

class ListDigestsResponse(_message.Message):
    __slots__ = ("digests",)
    DIGESTS_FIELD_NUMBER: _ClassVar[int]
    digests: _containers.RepeatedCompositeFieldContainer[_ledger_pb2.Digest]
    def __init__(self, digests: _Optional[_Iterable[_Union[_ledger_pb2.Digest, _Mapping]]] = ...) -> None: ...
