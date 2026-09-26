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


def cf(path: str) -> dict:
    req = urllib.request.Request(
        "https://api.cloudflare.com/client/v4" + path,
        headers={
            "Authorization": "Bearer " + token(),
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode())


def main() -> None:
    zones = cf("/zones?name=nocannobb.com")
    zid = zones["result"][0]["id"]
    recs = cf(f"/zones/{zid}/dns_records?name=cpr.nocannobb.com")["result"]
    for rec in recs:
        print(
            "record",
            rec.get("type"),
            rec.get("name"),
            "proxied",
            rec.get("proxied"),
            "content",
            rec.get("content"),
        )
    ssl = cf(f"/zones/{zid}/settings/ssl")
    print("ssl_mode", (ssl.get("result") or {}).get("value"))
    packs = cf(f"/zones/{zid}/ssl/certificate_packs?status=all").get("result") or []
    print("cert_packs", len(packs))
    for pack in packs[:8]:
        print("pack", pack.get("type"), pack.get("status"), pack.get("hosts"))


if __name__ == "__main__":
    main()
