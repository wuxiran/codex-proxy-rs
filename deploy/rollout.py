#!/usr/bin/env python3
"""89 上的 CPR 发版：新槽位先起、入口切流、旧槽位排空后退。

拓扑见 deploy/89.md。CPR 在 green / blue 两个槽位间轮换，槽位容器不带网络别名、
不发布宿主机端口；调用方只认 cpr-gate（deploy/compose.gate.yaml）。发版全程
cpr-gate 不重启，因此 sub2api 看不到 connection refused。

用法（在 89 上以 root 执行）：
  rollout.py status
  rollout.py deploy --metadata <metadata.json> --migrations <manifest.json>
  rollout.py deploy --image <已 docker load 的 tag> --migrations <manifest.json>
  rollout.py rollback
  rollout.py migrations-manifest backend/migrations   # 在构建镜像的那份检出里执行

含数据库迁移的版本不能走本流程：旧槽位要在新 schema 上继续服务到排空结束。
deploy 发现迁移清单与库里不一致会直接拒绝，这类版本走低峰维护窗口。
"""

import argparse
import copy
import datetime
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import time

ROOT = pathlib.Path(os.environ.get('CPR_ROOT', '/opt/codex-proxy-rs'))
DEPLOY = ROOT / 'deploy'
RUNTIME = ROOT / '.runtime'
UPSTREAM = RUNTIME / 'gate' / 'upstream.conf'
GATE_COMPOSE = DEPLOY / 'compose.gate.yaml'
GATE = 'cpr-gate'
SERVICE = 'codex-proxy-rs'
SLOTS = ('green', 'blue')
POSTGRES = 'codex-proxy-rs-postgres-1'
# 发版不得波及的容器：StartedAt 在发版前后必须一致。
BYSTANDERS = ('sub2api', POSTGRES, 'codex-proxy-rs-redis-1')
# 与 config.yaml 的 host.drain_timeout_seconds=600 + worker_shutdown_timeout_seconds=30 配套。
STOP_GRACE = '11m'
RUNTIME_UID = 10001


def run(args, timeout=90, **kwargs):
    return subprocess.check_output(args, text=True, timeout=timeout, **kwargs).strip()


def log(event, **fields):
    print(json.dumps({'event': event, **fields}, ensure_ascii=False), flush=True)


def container(slot):
    return f'{SERVICE}-{slot}-{SERVICE}-1'


def compose_path(slot):
    return DEPLOY / f'compose.{slot}.json'


def inspect(name):
    try:
        return json.loads(run(['docker', 'inspect', name], stderr=subprocess.DEVNULL))[0]
    except subprocess.CalledProcessError:
        return None


def is_running(name):
    info = inspect(name)
    return bool(info and info['State']['Running'])


def holds_alias(info):
    """旧形态的容器自己带着 cpr-green 别名；它一启动就会抢入口的流量。"""
    networks = (info or {}).get('NetworkSettings', {}).get('Networks', {})
    return any('cpr-green' in (net.get('Aliases') or []) for net in networks.values())


def active_slot():
    """入口当前指向的槽位；入口尚未接入（首次发版）时返回 None。"""
    if not UPSTREAM.exists():
        return None
    match = re.search(rf'{SERVICE}-(green|blue)-{SERVICE}-1', UPSTREAM.read_text())
    if not match:
        raise SystemExit(f'无法从 {UPSTREAM} 识别当前槽位')
    return match.group(1)


def other(slot):
    return SLOTS[1] if slot == SLOTS[0] else SLOTS[0]


def psql(query):
    result = subprocess.run(
        ['docker', 'exec', '-i', POSTGRES, 'sh', '-c',
         'PGPASSWORD="$POSTGRES_PASSWORD" psql -U codex_proxy -d codex_proxy -Atq'],
        input=query, text=True, capture_output=True, timeout=30,
    )
    if result.returncode:
        raise SystemExit('数据库预检失败')
    return result.stdout.strip()


def write_atomic(path, content, mode=0o644):
    path.parent.mkdir(parents=True, exist_ok=True)
    candidate = path.with_name(path.name + '.new')
    candidate.write_text(content)
    os.chmod(candidate, mode)
    candidate.replace(path)


def write_upstream(slot):
    write_atomic(UPSTREAM, f'map "" $cpr_slot {{ default "{container(slot)}:8080"; }}\n')


def reload_gate():
    run(['docker', 'exec', GATE, 'nginx', '-t'], stderr=subprocess.STDOUT)
    run(['docker', 'exec', GATE, 'nginx', '-s', 'reload'])


def healthz_from_sub2api(host):
    run(['docker', 'exec', 'sub2api', 'wget', '-q', '-O', '/dev/null', '-T', '10',
         f'http://{host}:8080/healthz'])


def verify_gate():
    # 调用方视角：sub2api 经别名 → 入口 → 当前槽位。
    healthz_from_sub2api('cpr-green')
    run(['docker', 'exec', GATE, 'wget', '-q', '-O', '/dev/null', '-T', '10',
         'http://127.0.0.1:8080/healthz'])


def slot_spec(source, slot, image):
    """由任一现有 compose 派生槽位 compose：只改项目名、镜像、日志目录，去掉别名与端口。"""
    spec = copy.deepcopy(source)
    spec['name'] = f'{SERVICE}-{slot}'
    service = spec['services'][SERVICE]
    service['image'] = image
    service['stop_grace_period'] = STOP_GRACE
    service.pop('ports', None)
    for network in service.get('networks', {}).values():
        if isinstance(network, dict):
            network.pop('aliases', None)
    for volume in service.get('volumes', []):
        if volume.get('target') == '/app/.runtime/logs':
            volume['source'] = str(RUNTIME / f'logs-{slot}')
    return spec


def ensure_logs_dir(slot):
    path = RUNTIME / f'logs-{slot}'
    path.mkdir(mode=0o750, exist_ok=True)
    os.chown(path, RUNTIME_UID, RUNTIME_UID)


def compose_up(path):
    run(['docker', 'compose', '-f', str(path), 'config', '--quiet'])
    run(['docker', 'compose', '-f', str(path), 'up', '-d', '--no-deps', '--no-build',
         '--wait', '--wait-timeout', '180', SERVICE], timeout=240)


def stop_detached(name):
    """排空最长可达 STOP_GRACE，不阻塞发版脚本；容器保留为 stopped 以便回滚。"""
    subprocess.Popen(['docker', 'stop', name], stdin=subprocess.DEVNULL,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)


def remove_slot(slot):
    subprocess.run(['docker', 'rm', '-f', container(slot)], capture_output=True, timeout=120)


def acquire_lock():
    import fcntl  # 仅 Unix；migrations-manifest 需要能在 Windows 检出里执行

    lock = (RUNTIME / 'rollout.lock').open('w')
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise SystemExit('另一个发版正在进行')
    return lock


def cmd_status(_args):
    slot = active_slot()
    report = {'activeSlot': slot, 'gate': None, 'slots': {}}
    gate = inspect(GATE)
    if gate:
        report['gate'] = {'running': gate['State']['Running'], 'startedAt': gate['State']['StartedAt']}
    for candidate in SLOTS:
        info = inspect(container(candidate))
        if info:
            report['slots'][candidate] = {
                'image': info['Config']['Image'],
                'status': info['State']['Status'],
                'health': info['State'].get('Health', {}).get('Status'),
                'startedAt': info['State']['StartedAt'],
                'holdsAlias': holds_alias(info),
            }
    print(json.dumps(report, indent=2, ensure_ascii=False))


def cmd_deploy(args):
    lock = acquire_lock()  # noqa: F841 进程存活期间持锁

    meta = json.loads(pathlib.Path(args.metadata).read_text()) if args.metadata else {}
    image = args.image or meta.get('image')
    if not image:
        raise SystemExit('需要 --image 或 --metadata')

    current = active_slot()
    bootstrap = current is None
    source_slot = current or 'green'
    target = other(source_slot)
    source_path = compose_path(source_slot)
    source_spec = json.loads(source_path.read_text())
    old_name = container(source_slot)
    old = inspect(old_name)
    if not (old and old['State']['Running']):
        raise SystemExit(f'当前槽位 {source_slot} 未在运行，先排查')
    if is_running(container(target)):
        raise SystemExit(f'槽位 {target} 仍在运行（多半是上次发版的排空未结束），稍后再发')
    if not bootstrap and not is_running(GATE):
        raise SystemExit('cpr-gate 未运行')

    # —— 预检 ——
    if meta.get('remoteArchive'):
        digest = hashlib.sha256(pathlib.Path(meta['remoteArchive']).read_bytes()).hexdigest()
        if digest != meta['archiveSha256']:
            raise SystemExit('镜像包 sha256 不符')
    expected = json.loads(pathlib.Path(args.migrations).read_text())
    rows = psql("select version, encode(checksum, 'hex') from _sqlx_migrations order by version;")
    applied = dict(line.split('|', 1) for line in rows.splitlines())
    if applied != expected:
        raise SystemExit('新镜像的迁移清单与数据库不一致：含迁移的版本不能走先起后停，改走维护窗口')
    # 备份 worker 按单副本设计，新实例启动时会无锁「恢复」进行中的备份。
    if int(psql("select count(*) from backup_records where status in ('dumping', 'uploading');")):
        raise SystemExit('有备份任务正在执行，结束后再发版')

    watched = BYSTANDERS if bootstrap else BYSTANDERS + (GATE,)
    started_at = {name: inspect(name)['State']['StartedAt'] for name in watched}

    backup = RUNTIME / 'backups' / ('rollout-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
    backup.mkdir(mode=0o700, parents=True)
    for path in (source_path, compose_path(target), UPSTREAM, DEPLOY / 'CURRENT_RELEASE.md'):
        if path.exists():
            (backup / path.name).write_bytes(path.read_bytes())
    log('backup', path=str(backup))

    if meta.get('remoteArchive'):
        run(['docker', 'load', '-i', meta['remoteArchive']], timeout=300)

    # —— 起新槽位（此时零流量）——
    target_name = container(target)
    remove_slot(target)
    ensure_logs_dir(target)
    write_atomic(compose_path(target), json.dumps(slot_spec(source_spec, target, image), indent=2) + '\n', mode=0o600)
    switched = False
    try:
        compose_up(compose_path(target))
        fresh = inspect(target_name)
        assert fresh['State']['Health']['Status'] == 'healthy', '新槽位未就绪'
        assert fresh['Config']['Image'] == image, '新槽位镜像不符'
        assert not holds_alias(fresh), '新槽位不应带 cpr-green 别名'
        for src, key in (('/app/bin/codex-proxy-rs', 'binarySha256'), ('/app/web/dist/index.html', 'indexSha256')):
            if meta.get(key):
                assert run(['docker', 'exec', target_name, 'sha256sum', src]).split()[0] == meta[key], key
        healthz_from_sub2api(target_name)
        log('slot_ready', slot=target, image=image)

        # —— 切流 ——
        write_upstream(target)
        switched = True
        if bootstrap:
            run(['docker', 'compose', '-f', str(GATE_COMPOSE), 'up', '-d', '--wait', '--wait-timeout', '60'], timeout=120)
        else:
            reload_gate()
        verify_gate()
        for name, value in started_at.items():
            assert inspect(name)['State']['StartedAt'] == value, f'{name} 被重启了'
        log('switched', slot=target)
    except Exception:
        if switched and not bootstrap:
            write_upstream(source_slot)
            reload_gate()
        elif switched:
            subprocess.run(['docker', 'compose', '-f', str(GATE_COMPOSE), 'down'], capture_output=True, timeout=120)
            UPSTREAM.unlink(missing_ok=True)
        remove_slot(target)
        log('aborted', keptSlot=source_slot)
        raise

    # —— 旧槽位后退 ——
    if bootstrap:
        # 旧形态容器自带别名和宿主机端口，留着会在下次被拉起时抢入口的流量：
        # 这里只停不删，确认入口稳定后由人执行 `rollout.py retire-legacy`。
        log('legacy_left_running', container=old_name,
            hint='先把宿主机 nginx 改指 cpr-gate，再执行 rollout.py retire-legacy')
    else:
        stop_detached(old_name)
        log('draining', slot=source_slot, grace=STOP_GRACE)

    (DEPLOY / 'CURRENT_RELEASE.md').write_text(
        f'# 当前 CPR 部署\n\n镜像：`{image}`\n槽位：`{target}`（入口 cpr-gate）\n'
        f'上一槽位：`{source_slot}`（`rollout.py rollback` 可切回）\n备份：`{backup}`\n')
    result = {'status': 'deployed', 'image': image, 'slot': target, 'previousSlot': source_slot,
              'bootstrap': bootstrap, 'backup': str(backup)}
    (backup / 'result.json').write_text(json.dumps(result, indent=2))
    log('done', **result)


def cmd_retire_legacy(_args):
    """首次接入入口后的收尾：停掉并删除自带别名的旧形态容器，把它的 compose 改成槽位形态。"""
    lock = acquire_lock()  # noqa: F841
    current = active_slot()
    if current is None:
        raise SystemExit('入口尚未接入')
    legacy_slot = other(current)
    name = container(legacy_slot)
    info = inspect(name)
    if not (info and holds_alias(info)):
        raise SystemExit(f'{name} 不是旧形态容器，无需处理')
    verify_gate()
    run(['docker', 'stop', name], timeout=900)
    run(['docker', 'rm', name])
    spec = json.loads(compose_path(legacy_slot).read_text())
    image = spec['services'][SERVICE]['image']
    write_atomic(compose_path(legacy_slot), json.dumps(slot_spec(spec, legacy_slot, image), indent=2) + '\n', mode=0o600)
    verify_gate()
    log('legacy_retired', container=name)


def cmd_rollback(_args):
    lock = acquire_lock()  # noqa: F841
    current = active_slot()
    if current is None:
        raise SystemExit('入口尚未接入，无可回滚')
    previous = other(current)
    name = container(previous)
    info = inspect(name)
    if not info:
        raise SystemExit(f'槽位 {previous} 的容器已不存在；用旧镜像 tag 再走一次 deploy')
    if holds_alias(info):
        raise SystemExit(f'{name} 是自带别名的旧形态容器，拉起会抢入口流量；用旧镜像 tag 再走一次 deploy')
    if info['State']['Running']:
        raise SystemExit(f'槽位 {previous} 仍在排空，等它停下再回滚')
    run(['docker', 'start', name])
    deadline = time.time() + 180
    while inspect(name)['State'].get('Health', {}).get('Status') != 'healthy':
        if time.time() > deadline:
            run(['docker', 'stop', name], timeout=900)
            raise SystemExit('旧槽位未能恢复健康，入口保持不变')
        time.sleep(2)
    healthz_from_sub2api(name)
    write_upstream(previous)
    try:
        reload_gate()
        verify_gate()
    except Exception:
        write_upstream(current)
        reload_gate()
        raise
    stop_detached(container(current))
    log('rolled_back', slot=previous, image=info['Config']['Image'])


def cmd_migrations_manifest(args):
    """sqlx 的 checksum 是迁移文件内容的 SHA-384；必须在构建镜像的那份检出里生成。"""
    manifest = {}
    for path in sorted(pathlib.Path(args.directory).glob('*.sql')):
        version = str(int(path.name.split('_', 1)[0]))
        manifest[version] = hashlib.sha384(path.read_bytes()).hexdigest()
    print(json.dumps(manifest, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('status').set_defaults(handler=cmd_status)
    deploy = commands.add_parser('deploy')
    deploy.add_argument('--metadata', help='构建产物 metadata.json（image / remoteArchive / *Sha256）')
    deploy.add_argument('--image', help='已 docker load 的镜像 tag；与 --metadata 二选一')
    deploy.add_argument('--migrations', required=True, help='migrations-manifest 生成的清单')
    deploy.set_defaults(handler=cmd_deploy)
    commands.add_parser('retire-legacy').set_defaults(handler=cmd_retire_legacy)
    commands.add_parser('rollback').set_defaults(handler=cmd_rollback)
    manifest = commands.add_parser('migrations-manifest')
    manifest.add_argument('directory')
    manifest.set_defaults(handler=cmd_migrations_manifest)
    args = parser.parse_args()
    args.handler(args)


if __name__ == '__main__':
    sys.exit(main())
