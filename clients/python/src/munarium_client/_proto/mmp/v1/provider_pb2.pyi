from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ApplyProviderConfigRequest(_message.Message):
    __slots__ = ("yaml",)
    YAML_FIELD_NUMBER: _ClassVar[int]
    yaml: str
    def __init__(self, yaml: _Optional[str] = ...) -> None: ...

class ApplyProviderConfigResponse(_message.Message):
    __slots__ = ("config_name",)
    CONFIG_NAME_FIELD_NUMBER: _ClassVar[int]
    config_name: str
    def __init__(self, config_name: _Optional[str] = ...) -> None: ...

class ProviderHealthRequest(_message.Message):
    __slots__ = ("config_name",)
    CONFIG_NAME_FIELD_NUMBER: _ClassVar[int]
    config_name: str
    def __init__(self, config_name: _Optional[str] = ...) -> None: ...

class ProviderHealthResponse(_message.Message):
    __slots__ = ("healthy", "provider", "endpoint_fingerprint", "detail")
    HEALTHY_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    ENDPOINT_FINGERPRINT_FIELD_NUMBER: _ClassVar[int]
    DETAIL_FIELD_NUMBER: _ClassVar[int]
    healthy: bool
    provider: str
    endpoint_fingerprint: str
    detail: str
    def __init__(self, healthy: _Optional[bool] = ..., provider: _Optional[str] = ..., endpoint_fingerprint: _Optional[str] = ..., detail: _Optional[str] = ...) -> None: ...

class CompleteRequest(_message.Message):
    __slots__ = ("config_name", "model", "system", "prompt", "max_tokens", "temperature", "tools_json", "version_id", "provider", "tier")
    CONFIG_NAME_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    SYSTEM_FIELD_NUMBER: _ClassVar[int]
    PROMPT_FIELD_NUMBER: _ClassVar[int]
    MAX_TOKENS_FIELD_NUMBER: _ClassVar[int]
    TEMPERATURE_FIELD_NUMBER: _ClassVar[int]
    TOOLS_JSON_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    TIER_FIELD_NUMBER: _ClassVar[int]
    config_name: str
    model: str
    system: str
    prompt: str
    max_tokens: int
    temperature: float
    tools_json: str
    version_id: str
    provider: str
    tier: str
    def __init__(self, config_name: _Optional[str] = ..., model: _Optional[str] = ..., system: _Optional[str] = ..., prompt: _Optional[str] = ..., max_tokens: _Optional[int] = ..., temperature: _Optional[float] = ..., tools_json: _Optional[str] = ..., version_id: _Optional[str] = ..., provider: _Optional[str] = ..., tier: _Optional[str] = ...) -> None: ...

class CompleteResponse(_message.Message):
    __slots__ = ("text", "stop_reason", "input_tokens", "output_tokens", "invocation_event_id", "provider", "model")
    TEXT_FIELD_NUMBER: _ClassVar[int]
    STOP_REASON_FIELD_NUMBER: _ClassVar[int]
    INPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    INVOCATION_EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    text: str
    stop_reason: str
    input_tokens: int
    output_tokens: int
    invocation_event_id: str
    provider: str
    model: str
    def __init__(self, text: _Optional[str] = ..., stop_reason: _Optional[str] = ..., input_tokens: _Optional[int] = ..., output_tokens: _Optional[int] = ..., invocation_event_id: _Optional[str] = ..., provider: _Optional[str] = ..., model: _Optional[str] = ...) -> None: ...

class EmbedRequest(_message.Message):
    __slots__ = ("config_name", "model", "inputs", "version_id", "provider")
    CONFIG_NAME_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    INPUTS_FIELD_NUMBER: _ClassVar[int]
    VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    config_name: str
    model: str
    inputs: _containers.RepeatedScalarFieldContainer[str]
    version_id: str
    provider: str
    def __init__(self, config_name: _Optional[str] = ..., model: _Optional[str] = ..., inputs: _Optional[_Iterable[str]] = ..., version_id: _Optional[str] = ..., provider: _Optional[str] = ...) -> None: ...

class EmbedResponse(_message.Message):
    __slots__ = ("vectors", "dimensions", "cache_hit", "invocation_event_id", "provider", "model")
    class Vector(_message.Message):
        __slots__ = ("values",)
        VALUES_FIELD_NUMBER: _ClassVar[int]
        values: _containers.RepeatedScalarFieldContainer[float]
        def __init__(self, values: _Optional[_Iterable[float]] = ...) -> None: ...
    VECTORS_FIELD_NUMBER: _ClassVar[int]
    DIMENSIONS_FIELD_NUMBER: _ClassVar[int]
    CACHE_HIT_FIELD_NUMBER: _ClassVar[int]
    INVOCATION_EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    vectors: _containers.RepeatedCompositeFieldContainer[EmbedResponse.Vector]
    dimensions: int
    cache_hit: bool
    invocation_event_id: str
    provider: str
    model: str
    def __init__(self, vectors: _Optional[_Iterable[_Union[EmbedResponse.Vector, _Mapping]]] = ..., dimensions: _Optional[int] = ..., cache_hit: _Optional[bool] = ..., invocation_event_id: _Optional[str] = ..., provider: _Optional[str] = ..., model: _Optional[str] = ...) -> None: ...
