from munarium_client._proto.mmp.v1 import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class HybridSearchRequest(_message.Message):
    __slots__ = ("query", "shape_ref", "top_k", "filter_json", "index_version")
    QUERY_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    TOP_K_FIELD_NUMBER: _ClassVar[int]
    FILTER_JSON_FIELD_NUMBER: _ClassVar[int]
    INDEX_VERSION_FIELD_NUMBER: _ClassVar[int]
    query: str
    shape_ref: str
    top_k: int
    filter_json: str
    index_version: str
    def __init__(self, query: _Optional[str] = ..., shape_ref: _Optional[str] = ..., top_k: _Optional[int] = ..., filter_json: _Optional[str] = ..., index_version: _Optional[str] = ...) -> None: ...

class SearchHit(_message.Message):
    __slots__ = ("chunk_id", "source_content_hash", "text", "score", "lexical_rank", "vector_rank", "metadata_json", "source_id", "source_path")
    CHUNK_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_CONTENT_HASH_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    SCORE_FIELD_NUMBER: _ClassVar[int]
    LEXICAL_RANK_FIELD_NUMBER: _ClassVar[int]
    VECTOR_RANK_FIELD_NUMBER: _ClassVar[int]
    METADATA_JSON_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_PATH_FIELD_NUMBER: _ClassVar[int]
    chunk_id: str
    source_content_hash: str
    text: str
    score: float
    lexical_rank: float
    vector_rank: float
    metadata_json: str
    source_id: str
    source_path: str
    def __init__(self, chunk_id: _Optional[str] = ..., source_content_hash: _Optional[str] = ..., text: _Optional[str] = ..., score: _Optional[float] = ..., lexical_rank: _Optional[float] = ..., vector_rank: _Optional[float] = ..., metadata_json: _Optional[str] = ..., source_id: _Optional[str] = ..., source_path: _Optional[str] = ...) -> None: ...

class HybridSearchResponse(_message.Message):
    __slots__ = ("hits", "envelope")
    HITS_FIELD_NUMBER: _ClassVar[int]
    ENVELOPE_FIELD_NUMBER: _ClassVar[int]
    hits: _containers.RepeatedCompositeFieldContainer[SearchHit]
    envelope: _common_pb2.ProvenanceEnvelope
    def __init__(self, hits: _Optional[_Iterable[_Union[SearchHit, _Mapping]]] = ..., envelope: _Optional[_Union[_common_pb2.ProvenanceEnvelope, _Mapping]] = ...) -> None: ...

class GetIndexVersionRequest(_message.Message):
    __slots__ = ("shape_ref",)
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    shape_ref: str
    def __init__(self, shape_ref: _Optional[str] = ...) -> None: ...

class GetIndexVersionResponse(_message.Message):
    __slots__ = ("index_version", "event_watermark", "manifest_json", "active")
    INDEX_VERSION_FIELD_NUMBER: _ClassVar[int]
    EVENT_WATERMARK_FIELD_NUMBER: _ClassVar[int]
    MANIFEST_JSON_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_FIELD_NUMBER: _ClassVar[int]
    index_version: str
    event_watermark: int
    manifest_json: str
    active: bool
    def __init__(self, index_version: _Optional[str] = ..., event_watermark: _Optional[int] = ..., manifest_json: _Optional[str] = ..., active: _Optional[bool] = ...) -> None: ...

class CreateCollectionRequest(_message.Message):
    __slots__ = ("name", "shape_ref", "access_level", "compartments", "description")
    NAME_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COMPARTMENTS_FIELD_NUMBER: _ClassVar[int]
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    name: str
    shape_ref: str
    access_level: int
    compartments: _containers.RepeatedScalarFieldContainer[str]
    description: str
    def __init__(self, name: _Optional[str] = ..., shape_ref: _Optional[str] = ..., access_level: _Optional[int] = ..., compartments: _Optional[_Iterable[str]] = ..., description: _Optional[str] = ...) -> None: ...

class CollectionInfo(_message.Message):
    __slots__ = ("id", "name", "shape_ref", "access_level", "compartments", "status", "description", "created_at", "source_count", "active_index")
    ID_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    ACCESS_LEVEL_FIELD_NUMBER: _ClassVar[int]
    COMPARTMENTS_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    SOURCE_COUNT_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_INDEX_FIELD_NUMBER: _ClassVar[int]
    id: str
    name: str
    shape_ref: str
    access_level: int
    compartments: _containers.RepeatedScalarFieldContainer[str]
    status: str
    description: str
    created_at: str
    source_count: int
    active_index: str
    def __init__(self, id: _Optional[str] = ..., name: _Optional[str] = ..., shape_ref: _Optional[str] = ..., access_level: _Optional[int] = ..., compartments: _Optional[_Iterable[str]] = ..., status: _Optional[str] = ..., description: _Optional[str] = ..., created_at: _Optional[str] = ..., source_count: _Optional[int] = ..., active_index: _Optional[str] = ...) -> None: ...

class ListCollectionsRequest(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class ListCollectionsResponse(_message.Message):
    __slots__ = ("collections",)
    COLLECTIONS_FIELD_NUMBER: _ClassVar[int]
    collections: _containers.RepeatedCompositeFieldContainer[CollectionInfo]
    def __init__(self, collections: _Optional[_Iterable[_Union[CollectionInfo, _Mapping]]] = ...) -> None: ...

class GetCollectionRequest(_message.Message):
    __slots__ = ("id",)
    ID_FIELD_NUMBER: _ClassVar[int]
    id: str
    def __init__(self, id: _Optional[str] = ...) -> None: ...
