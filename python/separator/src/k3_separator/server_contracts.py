"""Typed HTTP and WebSocket contracts for the separator service."""

from __future__ import annotations

from enum import StrEnum
from typing import Annotated, Any, Literal

from fastapi import HTTPException
from pydantic import BaseModel, Field


def api_error(
    status_code: int,
    code: str,
    message: str,
    *,
    retryable: bool = False,
) -> HTTPException:
    """Build the service's stable machine-readable HTTP error."""

    return HTTPException(
        status_code,
        {"code": code, "message": message, "retryable": retryable},
    )


class JobStatus(StrEnum):
    QUEUED = "queued"
    RUNNING = "running"
    CANCELLING = "cancelling"
    CANCELLED = "cancelled"
    COMPLETED = "completed"
    FAILED = "failed"


class InputStatus(StrEnum):
    UPLOADING = "uploading"
    READY = "ready"


class InputRequest(BaseModel):
    filename: str = Field(min_length=1)
    size_bytes: int = Field(ge=1)
    sha256: str = Field(pattern=r"^[0-9a-f]{64}$")


class JobRequest(BaseModel):
    input_id: str = Field(min_length=1)
    model_id: str = Field(min_length=1)
    preset: str = Field(pattern=r"^(fast|balanced|quality)$")
    output_layout: str = Field(pattern=r"^(two_stem|karaoke)$")
    force: bool = False


class ProtocolVersion(BaseModel):
    major: int
    minor: int


class FeatureSet(BaseModel):
    websocket_progress: bool
    http_polling: bool
    cancellation: bool
    result_cache: bool


class ServerLimits(BaseModel):
    max_concurrent_jobs: int
    max_queued_jobs: int
    max_upload_bytes: int
    max_duration_seconds: int
    max_input_channels: int
    min_free_bytes: int


class CapabilitiesResponse(BaseModel):
    server_id: str
    protocol: ProtocolVersion
    features: FeatureSet
    backend: str
    device: str | None
    limits: ServerLimits


class ReadinessResponse(BaseModel):
    status: str
    backend: str
    device: str | None = None


class ModelResponse(BaseModel):
    id: str
    display_name: str
    provider: str
    architecture: str
    checkpoint_sha256: str | None
    status: str
    backends: list[str]
    presets: list[str]
    output_layouts: list[str]


class ModelsResponse(BaseModel):
    models: list[ModelResponse]


class InputResponse(BaseModel):
    input_id: str
    status: InputStatus
    upload_required: bool | None = None


class InputListItem(BaseModel):
    input_id: str
    size_bytes: int
    sha256: str
    status: InputStatus
    created_at: float


class InputsResponse(BaseModel):
    inputs: list[InputListItem]


class ArtifactResponse(BaseModel):
    role: str
    artifact_id: str
    media_type: str
    size_bytes: int
    sha256: str


class JobResultResponse(BaseModel):
    artifacts: list[ArtifactResponse]
    provenance: dict[str, Any]


class ApiErrorDetail(BaseModel):
    code: str
    message: str
    retryable: bool
    details: dict[str, Any] | None = None


class ErrorResponse(BaseModel):
    error: ApiErrorDetail


class JobReferenceResponse(BaseModel):
    job_id: str
    status: JobStatus


class JobBaseResponse(BaseModel):
    job_id: str
    input_id: str | None = None
    model_id: str | None = None
    preset: str | None = None
    output_layout: str | None = None
    stage: str | None = None
    resolved_spec: dict[str, Any] | None = None


class QueuedJobResponse(JobBaseResponse):
    status: Literal[JobStatus.QUEUED]


class RunningJobResponse(JobBaseResponse):
    status: Literal[JobStatus.RUNNING, JobStatus.CANCELLING]


class CompletedJobResponse(JobBaseResponse):
    status: Literal[JobStatus.COMPLETED]
    result: JobResultResponse


class FailedJobResponse(JobBaseResponse):
    status: Literal[JobStatus.FAILED]
    error: ApiErrorDetail


class CancelledJobResponse(JobBaseResponse):
    status: Literal[JobStatus.CANCELLED]


JobResponse = Annotated[
    QueuedJobResponse
    | RunningJobResponse
    | CompletedJobResponse
    | FailedJobResponse
    | CancelledJobResponse,
    Field(discriminator="status"),
]


class JobsResponse(BaseModel):
    jobs: list[JobResponse]


class HealthResponse(BaseModel):
    status: str


class ProgressEvent(BaseModel):
    job_id: str
    sequence: int
    status: JobStatus
    stage: str
    message: str
