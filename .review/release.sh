#!/usr/bin/env bash
# CPR 发版 codex-proxy-rs:hunt-20260920-<sha>
#   遍历代理找 state + 到期前自动续期 + 手动观澜复活修复。无数据库迁移。
#   基线 = 现网 controls-20260919 的构建源码（不是 main）。
#
#   1) 宿主机 nginx 管理面入口 18082(旧 green) -> 18083(cpr-gate)   [2026-09-20 已完成，会自动跳过]
#   2) rollout.py retire-legacy   停删自带别名的旧形态 green（排空最长约 11 分钟）
#   3) rollout.py deploy          新槽位先起 -> gate 切流 -> 旧槽位排空
#
# 任何检查失败、查询失败、解析失败都会非零退出，不会继续往下走。
# 已是目标版本时重跑只做验收，不会把另一个槽位也换成新版（那样会丢掉 controls 这个回滚点）。
#
# 出了问题先看事实，不要照着「应该」去猜：
#     python3 /opt/codex-proxy-rs/deploy/rollout.py status      # activeSlot、各槽位镜像与状态
#     cat /opt/codex-proxy-rs/.runtime/gate/upstream.conf        # gate 被告知指向哪个槽位
#     docker logs --tail 50 cpr-gate
# 然后按所处状态处理：
#   - 第 1 步失败：脚本会把 nginx 配置还原并 reload；还原也失败时会明说，需手工用
#     .runtime/backups/nginx-cpr-*.conf 覆盖回去再 `nginx -s reload`。
#   - 第 2 步（retire-legacy）失败或中断：入口 gate 一直指向 active 槽位，服务不受影响；
#     旧 green 可能处于「已停未删」，重跑本脚本会继续处理。
#   - 第 3 步（deploy）在切流前失败：rollout.py 会删掉新槽位、保持旧槽位为入口（日志有 aborted）。重跑本脚本。
#   - 第 3 步在切流后失败（排空或写记录时）：入口已经是新版本，不会有 aborted。此时 status 的
#     activeSlot 就是新槽位；重跑本脚本会走「已是目标版本」的验收分支。
#   - 要回退到 controls：`rollout.py rollback` 要求另一个槽位的容器还在、不是旧形态、且已经停下
#     （刚发完版时它还在排空，要等排空结束，最长约 11 分钟）。它拒绝时，用旧镜像再发一次：
#         python3 deploy/rollout.py deploy --image codex-proxy-rs:controls-20260919 \
#           --migrations .runtime/deploy/cpr-controls-20260919-migrations.json
#     这同样要求「当前槽位在运行、目标槽位已停」。旧镜像保留在本机，不要 docker rmi。
#   - 回滚对账号无副作用：新版的续期参数存在 .runtime/data/turn_state/auto_hunt.json，不进账号凭据，
#     旧版本看不到它。
set -euo pipefail

ROOT=/opt/codex-proxy-rs
DEPLOY=$ROOT/.runtime/deploy
NGINX_CONF=/etc/nginx/conf.d/cpr.nocannobb.com.conf
ROLLOUT="python3 $ROOT/deploy/rollout.py"
IMAGE='codex-proxy-rs:hunt-20260920-<sha>'
ARCHIVE="$DEPLOY/cpr-hunt-20260920-<sha>-image.tar.gz"
ARCHIVE_SHA='<sha256>'
METADATA="$DEPLOY/cpr-hunt-20260920-metadata.json"
MIGRATIONS="$DEPLOY/cpr-hunt-20260920-migrations.json"

say() { printf '\n==== %s\n' "$*"; }
die() { printf '\n!!!! %s\n' "$*" >&2; exit 1; }
# 源站是 Cloudflare 源证书，本机直连不校验证书链；curl 自身失败返回 000，由调用方判断。
healthz() { curl -sk -o /dev/null -w '%{http_code}' --max-time 8 "$@" || true; }
gate_health()  { healthz http://127.0.0.1:18083/healthz; }
nginx_health() { healthz --resolve cpr.nocannobb.com:443:127.0.0.1 https://cpr.nocannobb.com/healthz; }

# 读取 rollout 状态里的一个事实。
# 必须先赋值再比较：  x=$(status_fact ...)
# 写成 if [ "$(status_fact ...)" = ... ] 的话，失败只会退出命令替换的子 shell，外层 if 会把
# 「查询失败」当成「条件不成立」继续往下走。单独的赋值语句在 set -e 下会让整个脚本退出。
status_fact() {
  local json
  json=$($ROLLOUT status) || { echo "rollout.py status 执行失败" >&2; return 1; }
  printf '%s' "$json" | python3 -c '
import json, sys
try:
    s = json.load(sys.stdin)
    slots = s["slots"]
    active = s["activeSlot"]
    fact = sys.argv[1]
    if fact == "legacy":
        print("yes" if any(v and v.get("holdsAlias") for v in slots.values()) else "no")
    elif fact == "active-image":
        if not active:
            raise ValueError("activeSlot 为空：入口尚未接入")
        print(slots[active]["image"])
    else:
        raise ValueError(f"unknown fact {fact}")
except Exception as error:
    sys.exit(f"无法解析 rollout status: {error!r}")
' "$1"
}

say "0/3 预检"
$ROLLOUT status || die "rollout.py status 执行失败"
[ "$(gate_health)" = 204 ] || die "cpr-gate(18083) 不健康"
echo "$ARCHIVE_SHA  $ARCHIVE" | sha256sum -c - || die "镜像包校验和不符"
# rollout.py 部署的是 metadata 里写的镜像和包，不是上面校验的 $ARCHIVE：两者必须是同一个东西，
# 否则可能校验着这次的包、却部署了遗留 metadata 指向的另一个版本。
python3 - "$METADATA" "$IMAGE" "$ARCHIVE" "$ARCHIVE_SHA" <<'PY' || die "metadata 与本次发版目标不一致"
import json, sys
meta = json.load(open(sys.argv[1]))
expected = {"image": sys.argv[2], "remoteArchive": sys.argv[3], "archiveSha256": sys.argv[4]}
wrong = {k: (meta.get(k), v) for k, v in expected.items() if meta.get(k) != v}
if wrong:
    sys.exit(f"metadata 不符: {wrong}")
PY
[ -r "$MIGRATIONS" ] || die "迁移清单不存在: $MIGRATIONS"

active_image=$(status_fact active-image)
if [ "$active_image" = "$IMAGE" ]; then
  say "active 槽位已是目标版本 $IMAGE，只做验收、不重复部署"
  # status 读的是磁盘上的 upstream.conf；上次若在「写文件」与「gate reload」之间中断，gate 实际
  # 还在服务旧槽位。reload 是平滑的、可重复的，让 gate 与文件一致后再验收。
  docker exec cpr-gate nginx -s reload || die "cpr-gate reload 失败"
  sleep 2
  gate=$(gate_health); via_nginx=$(nginx_health)
  echo "healthz  gate=$gate  nginx=$via_nginx"
  [ "$gate" = 204 ] && [ "$via_nginx" = 204 ] || die "已是目标版本，但入口不健康"
  exit 0
fi

say "1/3 nginx 管理面入口改指 cpr-gate"
[ -r "$NGINX_CONF" ] || die "读不到 $NGINX_CONF"
to_gate=$(grep -c '127.0.0.1:18083' "$NGINX_CONF" || true)
to_legacy=$(grep -c '127.0.0.1:18082' "$NGINX_CONF" || true)
if [ "$to_legacy" -gt 0 ]; then
  backup=$ROOT/.runtime/backups/nginx-cpr-$(date -u +%Y%m%dT%H%M%SZ).conf
  cp -p "$NGINX_CONF" "$backup"; echo "备份: $backup"
  restore() {
    cp -p "$backup" "$NGINX_CONF" && nginx -t && nginx -s reload \
      || die "nginx 还原失败！请手工用 $backup 覆盖 $NGINX_CONF 后执行 nginx -s reload"
  }
  sed -i 's#http://127.0.0.1:18082#http://127.0.0.1:18083#g' "$NGINX_CONF"
  nginx -t || { restore; die "nginx -t 失败，已还原"; }
  nginx -s reload || { restore; die "nginx reload 失败，已还原"; }
  sleep 1
  [ "$(nginx_health)" = 204 ] || { restore; die "经 nginx 访问 healthz 不是 204，已还原 nginx"; }
elif [ "$to_gate" -gt 0 ]; then
  echo "已指向 18083，跳过"
  [ "$(nginx_health)" = 204 ] || die "nginx 已指向 gate，但经它访问 healthz 不是 204"
else
  die "$NGINX_CONF 里既没有 18082 也没有 18083，配置与预期不符，不敢动"
fi
echo "管理面经 gate 健康"

say "2/3 退役旧形态 green（排空最长约 11 分钟）"
legacy=$(status_fact legacy)
if [ "$legacy" = yes ]; then
  $ROLLOUT retire-legacy || die "retire-legacy 失败；入口仍指向 active 槽位，查明后重跑"
  [ "$(gate_health)" = 204 ] || die "退役旧 green 后 gate 不健康"
else
  echo "没有自带别名的旧形态容器，跳过"
fi

say "3/3 发版"
$ROLLOUT deploy --metadata "$METADATA" --migrations "$MIGRATIONS" \
  || die "deploy 失败；先看 rollout.py status 判断入口在哪个槽位（见脚本头部说明）"

say "验收"
$ROLLOUT status || die "rollout.py status 执行失败"
active_image=$(status_fact active-image)
[ "$active_image" = "$IMAGE" ] || die "active 槽位的镜像是 $active_image，不是 $IMAGE"
gate=$(gate_health); via_nginx=$(nginx_health)
echo "healthz  gate=$gate  nginx=$via_nginx"
[ "$gate" = 204 ] && [ "$via_nginx" = 204 ] || die "发版后入口不健康"
say "完成：$IMAGE 已生效"
