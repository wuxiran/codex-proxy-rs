#!/usr/bin/env python3
"""统计 fork 对上游文件的侵入面，作为分层收敛的验收指标。

基线默认 `git merge-base HEAD upstream/main`。fork 自有新文件（A）只计数；
上游已有文件（M）按 `git diff -w` 口径统计增删行，忽略只改换行符的迁移。
「标记行」指新增行里带 `fork` 字样（`// fork: <主题>`、`<!-- fork -->` 等）的行，
收敛后留在上游文件里的改动应当都带标记并登记在 docs/FORK_HOOKS_REGISTRY.md。

用法：
    python3 tools/fork_divergence.py [--base REV] [--top N] [--files]
"""

from __future__ import annotations

import argparse
import re
import subprocess
from collections import Counter

BUCKETS = ((5, "<=5"), (20, "6-20"), (100, "21-100"), (None, ">100"))
KINDS = (
    ("test", re.compile(r"(^|/)tests?/|\.spec\.|_test\.")),
    ("wiring", re.compile(r"(src/lib\.rs|/mod\.rs|bootstrap\.rs|bundle\.rs|routes\.ts|AppSidebar\.vue|api/index\.ts)$")),
    ("frontend", re.compile(r"^frontend/")),
    ("docs", re.compile(r"^(docs/|deploy/README|README)")),
)


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True, encoding="utf-8").stdout


def kind_of(path: str) -> str:
    for name, pattern in KINDS:
        if pattern.search(path):
            return name
    return "logic"


def bucket_of(lines: int) -> str:
    for limit, name in BUCKETS:
        if limit is None or lines <= limit:
            return name
    raise AssertionError


def marked_lines(base: str, path: str) -> int:
    diff = git("diff", "-w", "-U0", base, "HEAD", "--", path)
    return sum(
        1
        for line in diff.splitlines()
        if line.startswith("+") and not line.startswith("+++") and "fork" in line.lower()
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", help="基线提交，默认 git merge-base HEAD upstream/main")
    parser.add_argument("--top", type=int, default=20, help="列出改动最多的前 N 个上游文件")
    parser.add_argument("--files", action="store_true", help="列出全部被改动的上游文件")
    args = parser.parse_args()

    base = args.base or git("merge-base", "HEAD", "upstream/main").strip()
    status = [line.split("\t") for line in git("diff", "--name-status", base, "HEAD").splitlines()]
    added_files = [parts[-1] for parts in status if parts[0].startswith("A")]
    modified = []
    for line in git("diff", "-w", "--numstat", base, "HEAD", "--diff-filter=M").splitlines():
        plus, minus, path = line.split("\t")
        if plus == "-":
            continue
        plus, minus = int(plus), int(minus)
        if plus + minus == 0:
            continue
        modified.append((path, plus, minus))

    total_plus = sum(plus for _, plus, _ in modified)
    total_minus = sum(minus for _, _, minus in modified)
    buckets = Counter(bucket_of(plus + minus) for _, plus, minus in modified)
    kinds = Counter(kind_of(path) for path, _, _ in modified)
    marked = sum(marked_lines(base, path) for path, _, _ in modified)

    print(f"base: {base[:10]}  HEAD: {git('rev-parse', '--short', 'HEAD').strip()}")
    print(f"fork-only files: {len(added_files)}")
    print(f"upstream files modified: {len(modified)}  (+{total_plus} / -{total_minus}, -w)")
    print("by size: " + "  ".join(f"{name}:{buckets.get(name, 0)}" for _, name in BUCKETS))
    print("by kind: " + "  ".join(f"{name}:{count}" for name, count in kinds.most_common()))
    print(f"fork-marked added lines: {marked} / {total_plus}")
    rows = sorted(modified, key=lambda row: row[1] + row[2], reverse=True)
    print(f"\ntop {args.top}:")
    for path, plus, minus in rows[: args.top]:
        print(f"  +{plus:<5} -{minus:<5} {kind_of(path):<8} {path}")
    if args.files:
        print("\nall modified upstream files:")
        for path, plus, minus in sorted(modified):
            print(f"  +{plus:<5} -{minus:<5} {path}")


if __name__ == "__main__":
    main()
