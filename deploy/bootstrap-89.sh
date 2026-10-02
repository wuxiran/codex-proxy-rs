#!/bin/bash
set -euo pipefail

INSTALL_DIR=/opt/codex-proxy-rs
REPO_URL=https://github.com/wuxiran/codex-proxy-rs.git

if [ -d "$INSTALL_DIR/.git" ]; then
  git -C "$INSTALL_DIR" fetch origin
  git -C "$INSTALL_DIR" checkout main
  git -C "$INSTALL_DIR" pull --ff-only origin main
else
  git clone --depth 1 "$REPO_URL" "$INSTALL_DIR"
fi

getent group 10001 >/dev/null || groupadd --system --gid 10001 cpr
if ! id -u 10001 >/dev/null 2>&1; then
  useradd --system --uid 10001 --gid 10001 --home-dir "$INSTALL_DIR" --shell /usr/sbin/nologin cpr
fi

install -d -m 0750 "$INSTALL_DIR/.runtime/postgres" "$INSTALL_DIR/.runtime/redis"
install -d -m 0770 -o 10001 -g 10001 "$INSTALL_DIR/.runtime/data" "$INSTALL_DIR/.runtime/logs"
# postgres:18 镜像内用户为 999；目录属主必须可写，不能用 cpr(10001)。
chown -R 999:999 "$INSTALL_DIR/.runtime/postgres"
chown -R 999:999 "$INSTALL_DIR/.runtime/redis" || true

CONFIG="$INSTALL_DIR/deploy/config.yaml"
if [ ! -f "$CONFIG" ]; then
  PG_PW="$(openssl rand -hex 24)"
  RD_PW="$(openssl rand -hex 24)"
  AD_PW="$(openssl rand -base64 18 | tr -d '/+=' | cut -c1-20)"
  python3 - "$INSTALL_DIR/deploy/config.example.yaml" "$CONFIG" "$PG_PW" "$RD_PW" "$AD_PW" <<'PY'
import sys
from pathlib import Path
src, dst, pg, rd, ad = sys.argv[1:6]
text = Path(src).read_text()
text = text.replace("password: &postgres_password ''", f"password: &postgres_password '{pg}'")
text = text.replace("password: &redis_password ''", f"password: &redis_password '{rd}'")
text = text.replace("default_password: ''", f"default_password: '{ad}'")
Path(dst).write_text(text)
Path("/opt/codex-proxy-rs/.runtime/ADMIN_BOOTSTRAP.txt").write_text(
    f"username=admin@cpr.local\npassword={ad}\nlisten=127.0.0.1:18080\n"
)
PY
  chmod 0640 "$CONFIG"
  chown root:10001 "$CONFIG"
  chmod 0600 "$INSTALL_DIR/.runtime/ADMIN_BOOTSTRAP.txt"
fi

chown -R 10001:10001 "$INSTALL_DIR/.runtime/data" "$INSTALL_DIR/.runtime/logs"
git -C "$INSTALL_DIR" log -1 --oneline
ls -ld "$INSTALL_DIR" "$INSTALL_DIR/deploy" "$INSTALL_DIR/.runtime"
test -f "$CONFIG"
test -f "$INSTALL_DIR/deploy/compose.89.yaml"
echo "bootstrap ok"
