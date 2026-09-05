from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class PutSourceRequest(_message.Message):
    __slots__ = ("header", "chunk")
    HEADER_FIELD_NUMBER: _ClassVar[int]
    CHUNK_FIELD_NUMBER: _ClassVar[int]
    header: SourceHeader
    chunk: bytes
    def __init__(self, header: _Optional[_Union[SourceHeader, _Mapping]] = ..., chunk: _Optional[bytes] = ...) -> None: ...

class SourceHeader(_message.Message):
    __slots__ = ("declared_sha256", "media_type", "filename", "shape_ref")
    DECLARED_SHA256_FIELD_NUMBER: _ClassVar[int]
    MEDIA_TYPE_FIELD_NUMBER: _ClassVar[int]
    FILENAME_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    declared_sha256: str
    media_type: str
    filename: str
    shape_ref: str
    def __init__(self, declared_sha256: _Optional[str] = ..., media_type: _Optional[str] = ..., filename: _Optional[str] = ..., shape_ref: _Optional[str] = ...) -> None: ...

class PutSourceResponse(_message.Message):
    __slots__ = ("content_hash", "bytes_len", "already_existed", "source_id")
    CONTENT_HASH_FIELD_NUMBER: _ClassVar[int]
    BYTES_LEN_FIELD_NUMBER: _ClassVar[int]
    ALREADY_EXISTED_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    content_hash: str
    bytes_len: int
    already_existed: bool
    source_id: str
    def __init__(self, content_hash: _Optional[str] = ..., bytes_len: _Optional[int] = ..., already_existed: _Optional[bool] = ..., source_id: _Optional[str] = ...) -> None: ...

class RecordIngestRequest(_message.Message):
    __slots__ = ("version_id", "content_hash", "shape_ref", "metadata_json")
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    CONTENT_HASH_FIELD_NUMBER: _ClassVar[int]
    SHAPE_REF_FIELD_NUMBER: _ClassVar[int]
    METADATA_JSON_FIELD_NUMBER: _ClassVar[int]
    version_id: str
    content_hash: str
    shape_ref: str
    metadata_json: str
    def __init__(self, version_id: _Optional[str] = ..., content_hash: _Optional[str] = ..., shape_ref: _Optional[str] = ..., metadata_json: _Optional[str] = ...) -> None: ...

class RecordIngestResponse(_message.Message):
    __slots__ = ("event_id", "seq")
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    event_id: str
    seq: int
    def __init__(self, event_id: _Optional[str] = ..., seq: _Optional[int] = ...) -> None: ...

class IngestFile(_message.Message):
    __slots__ = ("filename", "media_type", "content", "sha256", "collections")
    FILENAME_FIELD_NUMBER: _ClassVar[int]
    MEDIA_TYPE_FIELD_NUMBER: _ClassVar[int]
    CONTENT_FIELD_NUMBER: _ClassVar[int]
    SHA256_FIELD_NUMBER: _ClassVar[int]
    COLLECTIONS_FIELD_NUMBER: _ClassVar[int]
    filename: str
    media_type: str
    content: bytes
    sha256: str
    collections: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, filename: _Optional[str] = ..., media_type: _Optional[str] = ..., content: _Optional[bytes] = ..., sha256: _Optional[str] = ..., collections: _Optional[_Iterable[str]] = ...) -> None: ...

class IngestFilesRequest(_message.Message):
    __slots__ = ("files",)
    FILES_FIELD_NUMBER: _ClassVar[int]
    files: _containers.RepeatedCompositeFieldContainer[IngestFile]
    def __init__(self, files: _Optional[_Iterable[_Union[IngestFile, _Mapping]]] = ...) -> None: ...

class IngestResult(_message.Message):
    __slots__ = ("filename", "source_id", "sha256", "existed", "bound_to", "error")
    FILENAME_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    SHA256_FIELD_NUMBER: _ClassVar[int]
    EXISTED_FIELD_NUMBER: _ClassVar[int]
    BOUND_TO_FIELD_NUMBER: _ClassVar[int]
    ERROR_FIELD_NUMBER: _ClassVar[int]
    filename: str
    source_id: str
    sha256: str
    existed: bool
    bound_to: _containers.RepeatedScalarFieldContainer[str]
    error: str
    def __init__(self, filename: _Optional[str] = ..., source_id: _Optional[str] = ..., sha256: _Optional[str] = ..., existed: _Optional[bool] = ..., bound_to: _Optional[_Iterable[str]] = ..., error: _Optional[str] = ...) -> None: ...

class IngestFilesResponse(_message.Message):
    __slots__ = ("results",)
    RESULTS_FIELD_NUMBER: _ClassVar[int]
    results: _containers.RepeatedCompositeFieldContainer[IngestResult]
    def __init__(self, results: _Optional[_Iterable[_Union[IngestResult, _Mapping]]] = ...) -> None: ...
