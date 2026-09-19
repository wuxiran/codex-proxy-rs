#!/usr/bin/env python3
"""Create Cloudflare DNS for cpr.nocannobb.com. Token is read from acme.sh, never printed."""

from __future__ import annotations

import json
import pathlib
import urllib.request

ZONE_NAME = "nocannobb.com"
RECORD_NAME = "cpr.nocannobb.com"
ORIGIN_IP = "89.208.242.91"


def cf_token() -> str:
    conf = pathlib.Path("/root/.acme.sh/account.conf").read_text()
    for line in conf.splitlines():
        if line.startswith("SAVED_CF_Token="):
            return line.split("=", 1)[1].strip().strip("'\"")
    raise SystemExit("missing CF token")


def cf(method: str, path: str, body: dict | None = None, token: str = "") -> dict:
    req = urllib.request.Request(
        "https://api.cloudflare.com/client/v4" + path,
        data=None if body is None else json.dumps(body).encode(),
        method=method,
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode())


def main() -> None:
    token = cf_token()
    zones = cf("GET", f"/zones?name={ZONE_NAME}", token=token)
    if not zones.get("success") or not zones.get("result"):
        print("zone_ok false")
        raise SystemExit(2)
    zone = zones["result"][0]
    print("zone", zone.get("name"), "status", zone.get("status"))
    existing = cf(
        "GET",
        f"/zones/{zone['id']}/dns_records?name={RECORD_NAME}",
        token=token,
    )
    recs = existing.get("result") or []
    print("existing_records", len(recs))
    for rec in recs:
        print("existing", rec.get("type"), rec.get("name"), "proxied", rec.get("proxied"))
    if recs:
        return
    created = cf(
        "POST",
        f"/zones/{zone['id']}/dns_records",
        {
            "type": "A",
            "name": "cpr",
            "content": ORIGIN_IP,
            "ttl": 1,
            "proxied": True,
            "comment": "CPR origin on sub2api-89",
        },
        token=token,
    )
    print("dns_create_success", created.get("success"))
    if not created.get("success"):
        print("dns_errors", created.get("errors"))
        raise SystemExit(3)
    rec = created.get("result") or {}
    print("dns_created", rec.get("type"), rec.get("name"), "proxied", rec.get("proxied"))


if __name__ == "__main__":
    main()
