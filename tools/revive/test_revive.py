#!/usr/bin/env python3
from __future__ import annotations

import io
import json
import unittest
from email.message import Message
from urllib.error import HTTPError

from revive import (
    CprAdmin,
    ReviveClient,
    ReviveError,
    public_summary,
    recover_file,
    split_openai_document,
)


class FakeResponse:
    def __init__(self, payload: bytes, status: int = 200, headers: dict[str, str] | None = None):
        self._payload = payload
        self.status = status
        self.headers = headers or {"Content-Type": "application/json"}

    def read(self) -> bytes:
        return self._payload

    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


class FakeOpener:
    def __init__(self, handler):
        self.handler = handler
        self.calls = []

    def open(self, req, timeout=None):
        self.calls.append(req)
        return self.handler(req)


def json_bytes(value) -> bytes:
    return json.dumps(value).encode("utf-8")


class ReviveClientTests(unittest.TestCase):
    def test_start_verify_keeps_token_off_summary(self):
        job = {
            "job_id": "11111111-1111-1111-1111-111111111111",
            "task_token": "secret-token",
            "status": "queued",
            "preflight_id": "p1",
        }

        def handler(req):
            self.assertEqual(req.method, "POST")
            self.assertIn("/verify/start", req.full_url)
            self.assertIsNone(req.get_header("X-revive-task-token"))
            return FakeResponse(json_bytes({"ok": True, "job": job}))

        client = ReviveClient("https://example.test/api/revive/v1", opener=FakeOpener(handler))
        started = client.start_verify({"accounts": []})
        self.assertEqual(started["task_token"], "secret-token")
        self.assertNotIn("task_token", public_summary(started))

    def test_poll_verify_then_fetch_result(self):
        states = iter(
            [
                {"ok": True, "job": {"job_id": "j1", "status": "running"}},
                {"ok": True, "job": {"job_id": "j1", "status": "completed"}},
                {
                    "ok": True,
                    "job": {
                        "job_id": "j1",
                        "status": "completed",
                        "unauthorized_count": 2,
                        "preflight_id": "pf",
                        "verification": {"records": [{"status": "unauthorized"}]},
                    },
                },
            ]
        )

        def handler(req):
            return FakeResponse(json_bytes(next(states)))

        client = ReviveClient("https://example.test/api/revive/v1", opener=FakeOpener(handler))
        job = client.poll_verify("j1", "secret-token", interval=0)
        self.assertEqual(job["unauthorized_count"], 2)
        self.assertEqual(job["verification"]["records"][0]["status"], "unauthorized")

    def test_http_429_does_not_embed_token(self):
        def handler(req):
            headers = Message()
            headers["Retry-After"] = "3"
            raise HTTPError(
                req.full_url,
                429,
                "Too Many Requests",
                headers,
                io.BytesIO(json_bytes({"ok": False, "error": "slow down token=secret-token"})),
            )

        client = ReviveClient("https://example.test/api/revive/v1", opener=FakeOpener(handler))
        with self.assertRaises(ReviveError) as ctx:
            client.get_verify("j1", "secret-token")
        self.assertEqual(ctx.exception.status, 429)
        self.assertNotIn("secret-token", str(ctx.exception))

    def test_recover_skips_task_when_no_401(self):
        def handler(req):
            if req.full_url.endswith("/verify/start?workers=50"):
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "job": {
                                "job_id": "j1",
                                "task_token": "secret-token",
                                "status": "queued",
                            },
                        }
                    )
                )
            if "summary=1" in req.full_url:
                return FakeResponse(json_bytes({"ok": True, "job": {"job_id": "j1", "status": "completed"}}))
            if "result=1" in req.full_url:
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "job": {
                                "job_id": "j1",
                                "status": "completed",
                                "unauthorized_count": 0,
                                "normal_count": 3,
                            },
                        }
                    )
                )
            raise AssertionError(req.full_url)

        opener = FakeOpener(handler)
        client = ReviveClient("https://example.test/api/revive/v1", opener=opener)
        job, payload = recover_file(client, {"accounts": [{}]}, workers=50)
        self.assertEqual(job["normal_count"], 3)
        self.assertIsNone(payload)
        self.assertTrue(all("/tasks?" not in call.full_url for call in opener.calls))

    def test_recover_creates_task_and_downloads_json(self):
        def handler(req):
            url = req.full_url
            if url.endswith("/verify/start?workers=50"):
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "job": {
                                "job_id": "j1",
                                "task_token": "secret-token",
                                "status": "queued",
                            },
                        }
                    )
                )
            # 与观澜实测一致（2026-09-19）：统计计数只在 summary 视图，result 视图不带。
            if "/verify/j1" in url and "summary=1" in url:
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "job": {
                                "job_id": "j1",
                                "status": "completed",
                                "normal_count": 1,
                                "unauthorized_count": 1,
                                "preflight_id": "pf-1",
                            },
                        }
                    )
                )
            if "/verify/j1" in url and "result=1" in url:
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "job": {
                                "job_id": "j1",
                                "status": "completed",
                                "preflight_id": "pf-1",
                                "verification": {"records": [{"index": 0, "needs_recovery": True}]},
                            },
                        }
                    )
                )
            if "/tasks?" in url:
                self.assertIn("preflight_id=pf-1", url)
                self.assertIn("auto_start=1", url)
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "task": {
                                "task_id": "t1",
                                "task_token": "secret-token",
                                "status": "running",
                            },
                        }
                    )
                )
            if url.endswith("/tasks/t1"):
                return FakeResponse(
                    json_bytes(
                        {
                            "ok": True,
                            "task": {
                                "task_id": "t1",
                                "status": "recovered",
                                "download_ready": True,
                                "success_count": 1,
                            },
                        }
                    )
                )
            if "/tasks/t1/download" in url:
                self.assertEqual(req.get_header("X-revive-task-token"), "secret-token")
                return FakeResponse(
                    json_bytes({"accounts": [{"name": "ok"}]}),
                    headers={"Content-Type": "application/json"},
                )
            raise AssertionError(url)

        client = ReviveClient("https://example.test/api/revive/v1", opener=FakeOpener(handler))
        summary, payload = recover_file(client, {"accounts": [{}]}, workers=50)
        self.assertEqual(summary["task"]["status"], "recovered")
        self.assertEqual(json.loads(payload)["accounts"][0]["name"], "ok")
        self.assertNotIn("secret-token", json.dumps(summary))


class SplitAndImportTests(unittest.TestCase):
    def test_split_keeps_small_document(self):
        doc = {"data": {"accounts": [{"n": 1}], "proxies": []}}
        self.assertEqual(split_openai_document(doc), [doc])

    def test_split_chunks_accounts_and_keeps_proxies(self):
        accounts = [{"n": i} for i in range(201)]
        doc = {"data": {"accounts": accounts, "proxies": [{"proxy_key": "a"}]}}
        batches = split_openai_document(doc)
        self.assertEqual(len(batches), 2)
        self.assertEqual(len(batches[0]["data"]["accounts"]), 200)
        self.assertEqual(len(batches[1]["data"]["accounts"]), 1)
        self.assertEqual(batches[1]["data"]["proxies"][0]["proxy_key"], "a")

    def test_cpr_import_posts_provider_openai(self):
        seen = {}

        def handler(req):
            seen["url"] = req.full_url
            seen["key"] = req.get_header("X-api-key")
            seen["body"] = json.loads(req.data.decode("utf-8"))
            return FakeResponse(json_bytes({"ok": True}))

        admin = CprAdmin("http://127.0.0.1:18080", "admin-key", opener=FakeOpener(handler))
        admin.import_openai_document({"accounts": [{"name": "a"}]})
        self.assertEqual(seen["url"], "http://127.0.0.1:18080/api/admin/accounts/import")
        self.assertEqual(seen["key"], "admin-key")
        self.assertEqual(seen["body"]["provider"], "openai")
        self.assertEqual(seen["body"]["data"]["accounts"][0]["name"], "a")


if __name__ == "__main__":
    unittest.main()
