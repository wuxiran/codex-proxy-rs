#!/usr/bin/env bash
# 发版脚本沙箱干跑：所有外部命令都是假的，不碰任何真实系统。
set -uo pipefail
TMPL=$1
IMAGE=codex-proxy-rs:new
pass=0; fail=0

run_case() { # name expected_rc expect_calls(regex or '-') -- setup commands set env for fakes
  local name=$1 want_rc=$2 want_calls=$3 forbid_calls=$4
  local box; box=$(mktemp -d)
  mkdir -p "$box/bin" "$box/deploy" "$box/backups"
  : > "$box/calls"
  printf 'x' > "$box/deploy/img.tar.gz"
  local sha; sha=$(sha256sum "$box/deploy/img.tar.gz" | cut -d' ' -f1)
  echo '{}' > "$box/deploy/migrations.json"
  printf '{"image":"%s","remoteArchive":"%s","archiveSha256":"%s"}' \
    "${META_IMAGE:-$IMAGE}" "$box/deploy/img.tar.gz" "$sha" > "$box/deploy/metadata.json"
  printf 'proxy_pass http://127.0.0.1:%s;\n' "${NGINX_PORT:-18083}" > "$box/nginx.conf"
  echo "${ACTIVE_IMAGE:-codex-proxy-rs:old}" > "$box/active_image"
  echo "${LEGACY:-true}" > "$box/legacy"

  cat > "$box/bin/rollout" <<EOF
#!/usr/bin/env bash
echo "rollout \$*" >> "$box/calls"
case "\$1" in
  status)
    [ "${STATUS_MODE:-ok}" = fail ] && exit 3
    [ "${STATUS_MODE:-ok}" = garbage ] && { echo "not json"; exit 0; }
    printf '{"activeSlot":"blue","slots":{"green":{"image":"old","holdsAlias":%s},"blue":{"image":"%s","holdsAlias":false}}}\n' "\$(cat $box/legacy)" "\$(cat $box/active_image)";;
  retire-legacy) [ "${RETIRE_MODE:-ok}" = fail ] && exit 4; echo false > "$box/legacy";;
  deploy) [ "${DEPLOY_MODE:-ok}" = fail ] && exit 5; [ "${DEPLOY_MODE:-ok}" = noop ] || echo "$IMAGE" > "$box/active_image";;
esac
EOF
  for cmd in nginx docker sleep; do
    printf '#!/usr/bin/env bash\necho "%s $*" >> "%s/calls"\n[ "%s" = "${FAIL_CMD:-}" ] && exit 9\nexit 0\n' "$cmd" "$box" "$cmd" > "$box/bin/$cmd"
  done
  chmod +x "$box/bin/"*

  sed -e "s#^ROOT=.*#ROOT=$box#" \
      -e "s#^DEPLOY=.*#DEPLOY=$box/deploy#" \
      -e "s#^NGINX_CONF=.*#NGINX_CONF=$box/nginx.conf#" \
      -e "s#^ROLLOUT=.*#ROLLOUT=$box/bin/rollout#" \
      -e "s#^IMAGE=.*#IMAGE='$IMAGE'#" \
      -e "s#^ARCHIVE=.*#ARCHIVE=$box/deploy/img.tar.gz#" \
      -e "s#^ARCHIVE_SHA=.*#ARCHIVE_SHA='$sha'#" \
      -e "s#^METADATA=.*#METADATA=$box/deploy/metadata.json#" \
      -e "s#^MIGRATIONS=.*#MIGRATIONS=$box/deploy/migrations.json#" \
      -e "s#^gate_health().*#gate_health()  { echo ${GATE_HEALTH:-204}; }#" \
      -e "s#^nginx_health().*#nginx_health() { echo ${NGINX_HEALTH:-204}; }#" \
      -e "s#\$ROOT/.runtime/backups#$box/backups#" \
      "$TMPL" > "$box/release.sh"

  PATH="$box/bin:$PATH" bash "$box/release.sh" > "$box/out" 2>&1
  local rc=$? ok=1
  [ "$want_rc" = 0 ] && [ $rc -ne 0 ] && ok=0
  [ "$want_rc" != 0 ] && [ $rc -eq 0 ] && ok=0
  [ "$want_calls" != - ] && ! grep -Eq "$want_calls" "$box/calls" && ok=0
  [ "$forbid_calls" != - ] && grep -Eq "$forbid_calls" "$box/calls" && ok=0
  if [ $ok = 1 ]; then pass=$((pass+1)); printf 'PASS  %-46s rc=%s\n' "$name" "$rc"
  else fail=$((fail+1)); printf 'FAIL  %-46s rc=%s want_rc=%s\n' "$name" "$rc" "$want_rc"; sed 's/^/      /' "$box/calls"; tail -5 "$box/out" | sed 's/^/      | /'; fi
  rm -rf "$box"
}

#                       name                                         rc  must-call                 must-not-call
                        run_case "happy path"                          0  'rollout deploy'          -
LEGACY=false            run_case "no legacy container -> skip retire"  0  'rollout deploy'          'retire-legacy'
STATUS_MODE=fail        run_case "status command fails"                1  -                         'retire-legacy|rollout deploy'
STATUS_MODE=garbage     run_case "status prints garbage"               1  -                         'retire-legacy|rollout deploy'
META_IMAGE=other:tag    run_case "metadata points at another image"    1  -                         'retire-legacy|rollout deploy'
GATE_HEALTH=502         run_case "gate unhealthy before start"         1  -                         'retire-legacy|rollout deploy'
ACTIVE_IMAGE=$IMAGE     run_case "already on target -> verify only"    0  'docker exec cpr-gate'    'retire-legacy|rollout deploy'
ACTIVE_IMAGE=$IMAGE NGINX_HEALTH=000 run_case "already on target but unhealthy" 1 -                 'rollout deploy'
RETIRE_MODE=fail        run_case "retire-legacy fails"                 1  'retire-legacy'           'rollout deploy'
DEPLOY_MODE=fail        run_case "deploy fails"                        1  'rollout deploy'          -
DEPLOY_MODE=noop        run_case "deploy 'succeeds' but image unchanged" 1 'rollout deploy'         -
NGINX_PORT=18082        run_case "nginx still on legacy -> switched"   0  'nginx -s reload'         -
NGINX_PORT=18082 FAIL_CMD=nginx run_case "nginx -t fails -> restore, stop" 1 -                      'retire-legacy|rollout deploy'
NGINX_PORT=9999         run_case "nginx conf unexpected -> refuse"     1  -                         'retire-legacy|rollout deploy'
NGINX_HEALTH=502        run_case "nginx path unhealthy"                1  -                         'retire-legacy|rollout deploy'

echo; echo "passed=$pass failed=$fail"; [ $fail = 0 ]
