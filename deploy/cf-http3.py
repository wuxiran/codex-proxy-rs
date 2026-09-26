#!/usr/bin/env python3
import json
import pathlib
import urllib.request


def token() -> str:
    conf = pathlib.Path("/root/.acme.sh/account.conf").read_text()
    for line in conf.splitlines():
        if line.startswith("SAVED_CF_Token="):
            return line.split("=", 1)[1].strip().strip("'\"")
    raise SystemExit("missing token")


def cf(method: str, path: str, body: dict | None = None) -> tuple[int, dict]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        "https://api.cloudflare.com/client/v4" + path,
        data=data,
        method=method,
        headers={
            "Authorization": "Bearer " + token(),
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        payload = exc.read().decode()
        try:
            parsed = json.loads(payload)
        except json.JSONDecodeError:
            parsed = {"raw": payload[:200]}
        return exc.code, parsed


def main() -> None:
    code, zones = cf("GET", "/zones?name=nocannobb.com")
    print("zones_http", code)
    zid = zones["result"][0]["id"]
    for key in ("http3", "0rtt", "tls_1_3", "ssl", "always_use_https"):
        status, payload = cf("GET", f"/zones/{zid}/settings/{key}")
        value = (payload.get("result") or {}).get("value")
        print("setting", key, "http", status, "value", value)


if __name__ == "__main__":
    main()
