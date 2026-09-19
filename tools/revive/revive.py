#!/usr/bin/env python3
"""对接 https://zzledu.kdns.fr/revive-api 的 401 复活客户端。

只接受服务端已签名的 Sub2API / CPA JSON。CPR 原生导出不能直接上传。
恢复结果可写回本机 CPR 管理接口。令牌只放请求头，不写日志、不进异常文本。
"""

from __future__ import annotations

import argparse
import gzip
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any

DEFAULT_BASE = "https://zzledu.kdns.fr/api/revive/v1"
VERIFY_DONE = frozenset({"completed", "failed"})
TASK_DONE = frozenset({"normal", "recovered", "partial", "failed", "stopped"})
GZIP_THRESHOLD = 256 * 1024
IMPORT_BATCH = 200
TOKEN_HEADER = "X-Revive-Task-Token"


class ReviveError(RuntimeError):
    def __init__(self, message: str, *, status: int | None = None, code: str | None = None):
        super().__init__(message)
        self.status = status
        self.code = code


def _redact(text: str) -> str:
    lowered = text.lower()
    if "task_token" in lowered or "x-revive-task-token" in lowered:
        return "<redacted>"
    return text


def load_json(path: Path) -> Any:
    raw = path.read_bytes()
    if len(raw) > 20 * 1024 * 1024:
        raise ReviveError(f"{path} exceeds the 20 MB upload limit")
    try:
        return json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ReviveError(f"{path} is not valid JSON") from exc


class ReviveClient:
    def __init__(self, base_url: str = DEFAULT_BASE, timeout: float = 60.0, opener=None):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self._opener = opener or urllib.request.build_opener()

    def request(
        self,
        method: str,
        path: str,
        *,
        token: str | None = None,
        query: dict[str, Any] | None = None,
        body: Any = None,
        raw_body: bytes | None = None,
        content_type: str = "application/json",
        gzip_body: bool = False,
        extra_headers: dict[str, str] | None = None,
        expect_json: bool = True,
    ) -> tuple[int, dict[str, str], Any]:
        url = self.base_url + path
        if query:
            parts = []
            for key, value in query.items():
                if value is None:
                    continue
                if isinstance(value, bool):
                    value = "1" if value else "0"
                parts.append(f"{key}={value}")
            if parts:
                url += "?" + "&".join(parts)

        headers = {
            "Accept": "application/json, application/zip, application/octet-stream",
            "User-Agent": "cpr-revive/1.0",
        }
        data = raw_body
        if body is not None:
            data = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if data is not None:
            headers["Content-Type"] = content_type
            if gzip_body:
                data = gzip.compress(data)
                headers["Content-Encoding"] = "gzip"
        if token:
            headers[TOKEN_HEADER] = token
        if extra_headers:
            headers.update(extra_headers)

        req = urllib.request.Request(url, data=data, method=method, headers=headers)
        try:
            with self._opener.open(req, timeout=self.timeout) as resp:
                payload = resp.read()
                status = getattr(resp, "status", 200)
                response_headers = {k.lower(): v for k, v in resp.headers.items()}
        except urllib.error.HTTPError as exc:
            payload = exc.read()
            status = exc.code
            response_headers = {k.lower(): v for k, v in exc.headers.items()} if exc.headers else {}
            if status == 429:
                retry_after = response_headers.get("retry-after")
                raise ReviveError(
                    f"rate limited, retry-after={retry_after or 'missing'}",
                    status=429,
                ) from None
            raise ReviveError(self._error_message(status, payload), status=status) from None
        except urllib.error.URLError as exc:
            raise ReviveError(f"request failed: {_redact(str(exc.reason))}") from None

        if not expect_json:
            return status, response_headers, payload
        if not payload:
            return status, response_headers, {}
        try:
            parsed = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ReviveError(f"invalid JSON response ({status})") from exc
        if isinstance(parsed, dict) and parsed.get("ok") is False:
            raise ReviveError(
                _redact(str(parsed.get("error") or parsed.get("code") or status)),
                status=status,
                code=parsed.get("code"),
            )
        return status, response_headers, parsed

    def _error_message(self, status: int, payload: bytes) -> str:
        try:
            parsed = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return f"HTTP {status}"
        if isinstance(parsed, dict):
            return _redact(str(parsed.get("error") or parsed.get("code") or f"HTTP {status}"))
        return f"HTTP {status}"

    def start_verify(self, document: Any, *, workers: int = 50, gzip_body: bool | None = None) -> dict[str, Any]:
        raw = json.dumps(document, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if gzip_body is None:
            gzip_body = len(raw) >= GZIP_THRESHOLD
        _, _, parsed = self.request(
            "POST",
            "/verify/start",
            query={"workers": workers},
            raw_body=raw,
            gzip_body=gzip_body,
        )
        job = _require_job(parsed)
        if not job.get("job_id"):
            raise ReviveError("verify start did not return job_id")
        return job

    def get_verify(self, job_id: str, token: str, *, summary: bool = False, result: bool = False) -> dict[str, Any]:
        _, _, parsed = self.request(
            "GET",
            f"/verify/{job_id}",
            token=token,
            query={"summary": summary, "result": result},
        )
        return _require_job(parsed)

    def poll_verify(self, job_id: str, token: str, *, interval: float = 2.0, timeout: float = 1800.0) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while True:
            job = self.get_verify(job_id, token, summary=True)
            status = str(job.get("status") or "")
            if status in VERIFY_DONE:
                # result 视图不带 unauthorized_count 等统计计数；缺失会让 recover 误判
                # “无需复活”而静默跳过建任务，所以把 summary 的计数并回去。
                result = self.get_verify(job_id, token, result=True)
                for key, value in job.items():
                    result.setdefault(key, value)
                return result
            if time.monotonic() >= deadline:
                raise ReviveError(f"verify {job_id} timed out still {status or 'unknown'}")
            time.sleep(interval)

    def download_verify(self, job_id: str, token: str, *, fmt: str = "sub2api") -> bytes:
        _, _, payload = self.request(
            "GET",
            f"/verify/{job_id}/download",
            token=token,
            query={"format": fmt},
            expect_json=False,
        )
        return payload

    def create_task(
        self,
        document: Any,
        *,
        preflight_id: str,
        workers: int = 10,
        auto_start: bool = True,
        gzip_body: bool | None = None,
    ) -> dict[str, Any]:
        raw = json.dumps(document, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if gzip_body is None:
            gzip_body = len(raw) >= GZIP_THRESHOLD
        _, _, parsed = self.request(
            "POST",
            "/tasks",
            query={
                "preflight_id": preflight_id,
                "workers": workers,
                "auto_start": auto_start,
            },
            raw_body=raw,
            gzip_body=gzip_body,
        )
        return _require_task(parsed)

    def get_task(self, task_id: str, token: str) -> dict[str, Any]:
        _, _, parsed = self.request("GET", f"/tasks/{task_id}", token=token)
        return _require_task(parsed)

    def poll_task(self, task_id: str, token: str, *, interval: float = 3.0, timeout: float = 3600.0) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while True:
            task = self.get_task(task_id, token)
            status = str(task.get("status") or "")
            if status in TASK_DONE:
                return task
            if time.monotonic() >= deadline:
                raise ReviveError(f"task {task_id} timed out still {status or 'unknown'}")
            time.sleep(interval)

    def download_task(self, task_id: str, token: str, *, scope: str = "recovered", fmt: str = "json") -> bytes:
        _, headers, payload = self.request(
            "GET",
            f"/tasks/{task_id}/download",
            token=token,
            query={"scope": scope, "format": fmt},
            expect_json=False,
        )
        if headers.get("content-type", "").startswith("application/json"):
            return payload
        return payload


class CprAdmin:
    def __init__(self, base_url: str, api_key: str, timeout: float = 120.0, opener=None):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.timeout = timeout
        self._opener = opener or urllib.request.build_opener()

    def import_openai_document(self, document: Any) -> list[dict[str, Any]]:
        results = []
        for batch in split_openai_document(document):
            results.append(self._import_one(batch))
        return results

    def _import_one(self, document: Any) -> dict[str, Any]:
        body = json.dumps(
            {"provider": "openai", "data": document},
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
        req = urllib.request.Request(
            self.base_url + "/api/admin/accounts/import",
            data=body,
            method="POST",
            headers={
                "Content-Type": "application/json",
                "x-api-key": self.api_key,
                "Accept": "application/json",
            },
        )
        try:
            with self._opener.open(req, timeout=self.timeout) as resp:
                payload = resp.read()
                status = getattr(resp, "status", 200)
        except urllib.error.HTTPError as exc:
            payload = exc.read()
            raise ReviveError(_cpr_error(exc.code, payload), status=exc.code) from None
        try:
            parsed = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ReviveError(f"CPR import returned invalid JSON ({status})") from exc
        if isinstance(parsed, dict) and parsed.get("code") not in (None, 200, "200"):
            raise ReviveError(_redact(str(parsed.get("message") or parsed.get("code"))))
        return parsed if isinstance(parsed, dict) else {"raw": parsed}


def _require_job(parsed: Any) -> dict[str, Any]:
    if not isinstance(parsed, dict) or not isinstance(parsed.get("job"), dict):
        raise ReviveError("response missing job")
    return parsed["job"]


def _require_task(parsed: Any) -> dict[str, Any]:
    if not isinstance(parsed, dict) or not isinstance(parsed.get("task"), dict):
        raise ReviveError("response missing task")
    return parsed["task"]


def _cpr_error(status: int, payload: bytes) -> str:
    try:
        parsed = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return f"CPR HTTP {status}"
    if isinstance(parsed, dict):
        return _redact(str(parsed.get("message") or parsed.get("code") or f"CPR HTTP {status}"))
    return f"CPR HTTP {status}"


def split_openai_document(document: Any) -> list[Any]:
    if not isinstance(document, dict):
        raise ReviveError("recovered file is not a JSON object")
    root = document
    accounts_parent = document
    if isinstance(document.get("data"), dict) and isinstance(document["data"].get("accounts"), list):
        accounts_parent = document["data"]
    accounts = accounts_parent.get("accounts")
    if not isinstance(accounts, list) or len(accounts) <= IMPORT_BATCH:
        return [root]
    batches = []
    for offset in range(0, len(accounts), IMPORT_BATCH):
        chunk = json.loads(json.dumps(root))
        parent = chunk["data"] if "data" in document and isinstance(document["data"], dict) else chunk
        parent["accounts"] = accounts[offset : offset + IMPORT_BATCH]
        batches.append(chunk)
    return batches


def public_summary(job_or_task: dict[str, Any]) -> dict[str, Any]:
    keys = (
        "job_id",
        "task_id",
        "status",
        "preflight_id",
        "total_count",
        "completed_count",
        "normal_count",
        "unauthorized_count",
        "success_count",
        "failure_count",
        "latest_download_ready",
        "download_ready",
        "all_download_ready",
    )
    return {key: job_or_task[key] for key in keys if key in job_or_task}


def recover_file(
    client: ReviveClient,
    document: Any,
    *,
    workers: int = 50,
    task_workers: int = 10,
    scope: str = "recovered",
) -> tuple[dict[str, Any], bytes | None]:
    job = client.start_verify(document, workers=workers)
    token = job.get("task_token")
    if not isinstance(token, str) or not token:
        raise ReviveError("verify start did not return task_token")
    job = client.poll_verify(job["job_id"], token)
    if job.get("status") == "failed":
        raise ReviveError("verification failed")
    unauthorized = int(job.get("unauthorized_count") or 0)
    if unauthorized <= 0:
        return job, None
    preflight_id = job.get("preflight_id")
    if not preflight_id:
        raise ReviveError("verification completed without preflight_id")
    task = client.create_task(
        document,
        preflight_id=str(preflight_id),
        workers=task_workers,
        auto_start=True,
    )
    task_token = task.get("task_token") or token
    task = client.poll_task(task["task_id"], str(task_token))
    if not task.get("download_ready") and not task.get("all_download_ready"):
        return {"verify": public_summary(job), "task": public_summary(task)}, None
    payload = client.download_task(task["task_id"], str(task_token), scope=scope, fmt="json")
    return {"verify": public_summary(job), "task": public_summary(task)}, payload


def _print_json(value: Any) -> None:
    json.dump(value, sys.stdout, ensure_ascii=False, indent=2)
    sys.stdout.write("\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="401 revive-api client for signed Sub2API/CPA files")
    parser.add_argument("--base", default=os.environ.get("REVIVE_BASE", DEFAULT_BASE))
    sub = parser.add_subparsers(dest="cmd", required=True)

    verify = sub.add_parser("verify", help="upload and poll verification")
    verify.add_argument("file", type=Path)
    verify.add_argument("--workers", type=int, default=50)
    verify.add_argument("--download", type=Path)

    recover = sub.add_parser("recover", help="verify 401 accounts, revive, optionally import into CPR")
    recover.add_argument("file", type=Path)
    recover.add_argument("--workers", type=int, default=50)
    recover.add_argument("--task-workers", type=int, default=10)
    recover.add_argument("--scope", choices=("recovered", "all"), default="recovered")
    recover.add_argument("--out", type=Path)
    recover.add_argument("--import-cpr", action="store_true")
    recover.add_argument("--cpr-url", default=os.environ.get("CPR_ADMIN_URL", "http://127.0.0.1:18080"))
    recover.add_argument("--cpr-key", default=os.environ.get("CPR_ADMIN_API_KEY"))

    import_cpr = sub.add_parser("import-cpr", help="import an already recovered JSON into CPR")
    import_cpr.add_argument("file", type=Path)
    import_cpr.add_argument("--cpr-url", default=os.environ.get("CPR_ADMIN_URL", "http://127.0.0.1:18080"))
    import_cpr.add_argument("--cpr-key", default=os.environ.get("CPR_ADMIN_API_KEY"))

    args = parser.parse_args(argv)
    client = ReviveClient(args.base)
    try:
        if args.cmd == "verify":
            document = load_json(args.file)
            job = client.start_verify(document, workers=args.workers)
            token = job["task_token"]
            job = client.poll_verify(job["job_id"], token)
            _print_json(public_summary(job))
            if args.download:
                if not job.get("latest_download_ready"):
                    raise ReviveError("latest file is not ready")
                args.download.write_bytes(client.download_verify(job["job_id"], token))
            return 0
        if args.cmd == "recover":
            document = load_json(args.file)
            summary, payload = recover_file(
                client,
                document,
                workers=args.workers,
                task_workers=args.task_workers,
                scope=args.scope,
            )
            _print_json(summary if "verify" in summary else public_summary(summary))
            if payload is None:
                return 0
            out = args.out or Path(f"revived-{uuid.uuid4().hex[:8]}.json")
            out.write_bytes(payload)
            print(f"wrote {out}", file=sys.stderr)
            if args.import_cpr:
                if not args.cpr_key:
                    raise ReviveError("CPR admin API key missing (--cpr-key or CPR_ADMIN_API_KEY)")
                recovered = json.loads(payload.decode("utf-8"))
                results = CprAdmin(args.cpr_url, args.cpr_key).import_openai_document(recovered)
                _print_json({"imported_batches": len(results)})
            return 0
        if args.cmd == "import-cpr":
            if not args.cpr_key:
                raise ReviveError("CPR admin API key missing (--cpr-key or CPR_ADMIN_API_KEY)")
            document = load_json(args.file)
            results = CprAdmin(args.cpr_url, args.cpr_key).import_openai_document(document)
            _print_json({"imported_batches": len(results)})
            return 0
    except ReviveError as exc:
        print(_redact(str(exc)), file=sys.stderr)
        return 2
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
