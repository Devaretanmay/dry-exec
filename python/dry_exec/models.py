"""Type-safe models representing deterministic state deltas in Python."""

from typing import Dict, List, Optional
from pydantic import BaseModel, ConfigDict, Field


class ByteDelta(BaseModel):
    """Discrete byte delta within an isolated memory page or ephemeral file."""

    model_config = ConfigDict(frozen=True)

    offset: int = Field(
        description="Relative byte offset from start of mapped region or file"
    )
    original: bytes = Field(description="Pre-execution baseline byte sequence")
    mutated: bytes = Field(description="Post-execution mutated byte sequence")


class PageMutation(BaseModel):
    """Memory mutation detected on an isolated virtual memory page."""

    model_config = ConfigDict(frozen=True)

    page_index: int = Field(description="Index of the 4KB memory page")
    page_address: int = Field(description="Virtual address of the page")
    deltas: List[ByteDelta] = Field(
        default_factory=list, description="List of discrete byte mutations"
    )


class FsMutation(BaseModel):
    """Ephemeral filesystem mutation within the isolated mount namespace."""

    model_config = ConfigDict(frozen=True)

    mutation_type: str = Field(
        description="Classification: created, modified, or deleted"
    )
    path: str = Field(description="Relative path of the target inode")
    mode: Optional[int] = Field(default=None, description="POSIX file mode")
    size: Optional[int] = Field(default=None, description="File size in bytes")
    deltas: List[ByteDelta] = Field(
        default_factory=list, description="Byte deltas for modified files"
    )


class InterceptedRequest(BaseModel):
    """Outbound network request intercepted by the transparent proxy boundary."""

    model_config = ConfigDict(frozen=True)

    method: str = Field(description="HTTP request method")
    url: str = Field(description="Target request URL path")
    headers: Dict[str, str] = Field(
        default_factory=dict, description="Captured request headers"
    )
    request_body: bytes = Field(
        default=b"", description="Captured outbound request body"
    )
    response_status: int = Field(
        description="HTTP status code returned by transparent proxy"
    )
    response_body: bytes = Field(
        default=b"", description="Mock response payload returned by transparent proxy"
    )


class StateDelta(BaseModel):
    """Complete deterministic state delta computed post-execution."""

    model_config = ConfigDict(frozen=True)

    memory_mutations: List[PageMutation] = Field(
        default_factory=list,
        description="Isolated memory mutations grouped by virtual page",
    )
    fs_mutations: List[FsMutation] = Field(
        default_factory=list,
        description="Ephemeral filesystem mutations recorded in tmpfs overlay",
    )
    network_mutations: List[InterceptedRequest] = Field(
        default_factory=list,
        description="Outbound network mutations intercepted by the transparent proxy",
    )
    total_bytes_mutated: int = Field(description="Aggregate count of mutated bytes")
    duration_nanos: int = Field(
        description="State delta computation latency in nanoseconds"
    )
