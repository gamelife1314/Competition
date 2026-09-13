#!/bin/sh
# Git Bash / macOS / Linux 上的入口：找到一个真的能跑的 Python 3，把参数原样交给
# tools/collect_log.py。
#
#   bash tools/collect_log.sh <日志文件>
#
# **Windows 上不需要这个文件**——cmd 或 PowerShell 里直接：
#
#   python tools\collect_log.py <日志文件>
#
# 真正的实现在 collect_log.py 里（只用标准库，不需要 pip install）。这里只做一件事：
# 在 Windows 上 `python` 有时是应用商店的占位符，`command -v` 找得到、一跑就弹商店，
# 所以要**真的执行一下**才算数——试到能跑的为止。

set -u

here=$(dirname "$0")

for py in python3 py python; do
    if "$py" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 8) else 1)' 2>/dev/null; then
        exec "$py" "$here/collect_log.py" "$@"
    fi
done

cat >&2 <<'EOF'
找不到可用的 Python 3（需要 3.8 或更高）。

装一个：
  Windows       winget install Python.Python.3.12      （或 python.org 下载安装包）
  macOS         brew install python3
  Debian/Ubuntu sudo apt-get install -y python3

装完重开一个终端再跑。装不了就照 WORKFLOW_REQUEST §7.4 写一句
「collect_log.py：跑不了 + 报错原文」，六张表这次没跑——不要自己另编一张表。
EOF
exit 1
