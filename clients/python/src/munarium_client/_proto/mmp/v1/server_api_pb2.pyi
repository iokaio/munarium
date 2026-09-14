from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ServerApiRequest(_message.Message):
    __slots__ = ("path_parameters", "query_parameters", "body", "content_type", "source_headers")
    class PathParametersEntry(_message.Message):
        __slots__ = ("key", "value")
        KEY_FIELD_NUMBER: _ClassVar[int]
        VALUE_FIELD_NUMBER: _ClassVar[int]
        key: str
        value: str
        def __init__(self, key: _Optional[str] = ..., value: _Optional[str] = ...) -> None: ...
    class SourceHeadersEntry(_message.Message):
        __slots__ = ("key", "value")
        KEY_FIELD_NUMBER: _ClassVar[int]
        VALUE_FIELD_NUMBER: _ClassVar[int]
        key: str
        value: str
        def __init__(self, key: _Optional[str] = ..., value: _Optional[str] = ...) -> None: ...
    PATH_PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    QUERY_PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    BODY_FIELD_NUMBER: _ClassVar[int]
    CONTENT_TYPE_FIELD_NUMBER: _ClassVar[int]
    SOURCE_HEADERS_FIELD_NUMBER: _ClassVar[int]
    path_parameters: _containers.ScalarMap[str, str]
    query_parameters: _containers.RepeatedCompositeFieldContainer[ServerApiParameter]
    body: bytes
    content_type: str
    source_headers: _containers.ScalarMap[str, str]
    def __init__(self, path_parameters: _Optional[_Mapping[str, str]] = ..., query_parameters: _Optional[_Iterable[_Union[ServerApiParameter, _Mapping]]] = ..., body: _Optional[bytes] = ..., content_type: _Optional[str] = ..., source_headers: _Optional[_Mapping[str, str]] = ...) -> None: ...

class ServerApiParameter(_message.Message):
    __slots__ = ("name", "value")
    NAME_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    name: str
    value: str
    def __init__(self, name: _Optional[str] = ..., value: _Optional[str] = ...) -> None: ...

class ServerApiResponse(_message.Message):
    __slots__ = ("status", "content_type", "body")
    STATUS_FIELD_NUMBER: _ClassVar[int]
    CONTENT_TYPE_FIELD_NUMBER: _ClassVar[int]
    BODY_FIELD_NUMBER: _ClassVar[int]
    status: int
    content_type: str
    body: bytes
    def __init__(self, status: _Optional[int] = ..., content_type: _Optional[str] = ..., body: _Optional[bytes] = ...) -> None: ...
