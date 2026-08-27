"""FastAPI transport adapter for the separator domain service."""

from __future__ import annotations

import asyncio
from contextlib import asynccontextmanager
from typing import Annotated, Any

from fastapi import (
    Depends,
    FastAPI,
    Header,
    HTTPException,
    Request,
    Response,
    WebSocket,
    WebSocketDisconnect,
    status,
)
from fastapi.exceptions import RequestValidationError
from fastapi.responses import FileResponse, JSONResponse, PlainTextResponse

from .server_contracts import (
    CapabilitiesResponse,
    ErrorResponse,
    HealthResponse,
    InputRequest,
    InputResponse,
    InputsResponse,
    JobReferenceResponse,
    JobRequest,
    JobResponse,
    JobsResponse,
    ModelsResponse,
    ProgressEvent,
    ReadinessResponse,
)
from .server_core import SeparatorServer, ServerConfig
from .server_runtime import progress_message


def create_app(config: ServerConfig) -> FastAPI:
    """Build the HTTP adapter around one separator server module."""

    server = SeparatorServer(config)

    @asynccontextmanager
    async def lifespan(_app: FastAPI):
        server.start()
        try:
            yield
        finally:
            server.close()

    error_response = {
        "model": ErrorResponse,
        "description": "Machine-readable API error",
    }
    error_responses = {
        "default": error_response,
        422: error_response,
    }
    app = FastAPI(
        title="K3 Separator Server",
        version="1.0.0",
        lifespan=lifespan,
        responses=error_responses,
    )

    @app.exception_handler(HTTPException)
    async def http_error(_request: Request, error: HTTPException) -> JSONResponse:
        if isinstance(error.detail, dict) and {
            "code",
            "message",
            "retryable",
        }.issubset(error.detail):
            payload = error.detail
        else:
            codes = {
                status.HTTP_401_UNAUTHORIZED: "unauthorized",
                status.HTTP_413_CONTENT_TOO_LARGE: "input_rejected",
                status.HTTP_404_NOT_FOUND: "not_found",
                status.HTTP_409_CONFLICT: "conflict",
                status.HTTP_422_UNPROCESSABLE_CONTENT: "invalid_request",
                status.HTTP_429_TOO_MANY_REQUESTS: "queue_full",
                status.HTTP_507_INSUFFICIENT_STORAGE: "insufficient_storage",
            }
            payload = {
                "code": codes.get(error.status_code, "request_failed"),
                "message": str(error.detail),
                "retryable": error.status_code >= 500,
            }
        return JSONResponse(
            status_code=error.status_code,
            content={"error": payload},
            headers=error.headers,
        )

    @app.exception_handler(RequestValidationError)
    async def validation_error(
        _request: Request, error: RequestValidationError
    ) -> JSONResponse:
        return JSONResponse(
            status_code=status.HTTP_422_UNPROCESSABLE_CONTENT,
            content={
                "error": {
                    "code": "invalid_request",
                    "message": "request validation failed",
                    "retryable": False,
                    "details": {"errors": error.errors()},
                }
            },
        )

    def authenticated_token(
        authorization: Annotated[str | None, Header()] = None,
    ) -> str:
        return server.authenticate(authorization)

    @app.get("/healthz", response_model=HealthResponse)
    def health() -> dict[str, str]:
        return {"status": "ok"}

    @app.get("/readyz", response_model=ReadinessResponse)
    def readiness() -> dict[str, Any]:
        return server.readiness()

    @app.get("/metrics", response_class=PlainTextResponse)
    def metrics(
        _token_id: str = Depends(authenticated_token),
    ) -> str:
        return server.metrics()

    @app.get("/v1/capabilities", response_model=CapabilitiesResponse)
    def capabilities(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return server.capabilities()

    @app.get("/v1/models", response_model=ModelsResponse)
    def models(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return {"models": server.models()}

    @app.websocket("/v1/events")
    async def events(websocket: WebSocket) -> None:
        try:
            server.authenticate(websocket.headers.get("authorization"))
        except HTTPException:
            await websocket.close(code=4401, reason="unauthorized")
            return
        await websocket.accept()
        subscribed: set[str] = set()
        last_snapshot: dict[str, tuple[str, str]] = {}
        sequence = 0
        try:
            while True:
                try:
                    message = await asyncio.wait_for(
                        websocket.receive_json(), timeout=0.1
                    )
                except TimeoutError:
                    message = None
                if message is not None:
                    action = message.get("action")
                    job_ids = message.get("job_ids", [])
                    if action == "subscribe" and isinstance(job_ids, list):
                        subscribed.update(str(job_id) for job_id in job_ids)
                    elif action == "unsubscribe" and isinstance(job_ids, list):
                        subscribed.difference_update(str(job_id) for job_id in job_ids)
                    else:
                        await websocket.send_json(
                            {
                                "error": {
                                    "code": "invalid_request",
                                    "message": "expected subscribe or unsubscribe",
                                    "retryable": False,
                                }
                            }
                        )
                for job_id in tuple(subscribed):
                    try:
                        job = server.get_job(job_id)
                    except HTTPException:
                        subscribed.remove(job_id)
                        continue
                    snapshot = (job["status"], job["stage"])
                    if last_snapshot.get(job_id) == snapshot:
                        continue
                    last_snapshot[job_id] = snapshot
                    sequence += 1
                    await websocket.send_json(
                        ProgressEvent(
                            job_id=job_id,
                            sequence=sequence,
                            status=job["status"],
                            stage=job["stage"],
                            message=progress_message(job["stage"]),
                        ).model_dump(mode="json")
                    )
        except WebSocketDisconnect:
            return

    @app.post("/v1/inputs", response_model=InputResponse)
    def create_input(
        request: InputRequest,
        response: Response,
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        result, response.status_code = server.create_input(request)
        return result

    @app.put("/v1/inputs/{input_id}/content", response_model=InputResponse)
    async def upload_input(
        input_id: str,
        request: Request,
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return await server.upload_input(input_id, request)

    @app.get("/v1/inputs", response_model=InputsResponse)
    def list_inputs(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return {"inputs": server.list_inputs()}

    @app.delete("/v1/inputs/{input_id}", status_code=status.HTTP_204_NO_CONTENT)
    def delete_input(
        input_id: str, _token_id: str = Depends(authenticated_token)
    ) -> Response:
        server.delete_input(input_id)
        return Response(status_code=status.HTTP_204_NO_CONTENT)

    @app.post(
        "/v1/jobs",
        status_code=status.HTTP_202_ACCEPTED,
        response_model=JobReferenceResponse,
    )
    def create_job(
        request: JobRequest,
        idempotency_key: Annotated[str, Header(alias="Idempotency-Key")],
        token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return server.create_job(token_id, idempotency_key, request)

    @app.get("/v1/jobs/{job_id}", response_model=JobResponse)
    def get_job(
        job_id: str, _token_id: str = Depends(authenticated_token)
    ) -> dict[str, Any]:
        return server.get_job(job_id)

    @app.get("/v1/jobs", response_model=JobsResponse)
    def list_jobs(
        _token_id: str = Depends(authenticated_token),
    ) -> dict[str, Any]:
        return {"jobs": server.list_jobs()}

    @app.post("/v1/jobs/{job_id}/cancel", response_model=JobReferenceResponse)
    def cancel_job(
        job_id: str, _token_id: str = Depends(authenticated_token)
    ) -> dict[str, Any]:
        return server.cancel_job(job_id)

    @app.delete("/v1/jobs/{job_id}/artifacts", status_code=status.HTTP_204_NO_CONTENT)
    def delete_job_artifacts(
        job_id: str, _token_id: str = Depends(authenticated_token)
    ) -> Response:
        server.delete_job_artifacts(job_id)
        return Response(status_code=status.HTTP_204_NO_CONTENT)

    @app.get("/v1/artifacts/{artifact_id}")
    def download_artifact(
        artifact_id: str,
        _token_id: str = Depends(authenticated_token),
    ) -> FileResponse:
        path, media_type = server.artifact_path(artifact_id)
        return FileResponse(path, media_type=media_type)

    return app
