#!/bin/bash
# 交付用的日志切片：把一场的 stdout 原始日志跑成「六张证据表」。
#
#   bash tools/collect_log.sh <日志文件>        # .jsonl 或 .jsonl.gz 都行
#
# 把**整段输出**贴进 issue 即可（WORKFLOW_REQUEST §7.3）。这个脚本是那份规范里十三条
# 配方的机器版：跑哪些命令、怎么聚合、每张表印多少行，全在这里定死。所以你不需要判断
# 「贴多少才够」「要不要汇总」——给它一个日志路径，它给你一页，那一页就是要贴的东西。
#
# 三个刻意的设计：
#
#   * **永不失败。** 某个事件一行都没有，它印「（0 行）」而不是报错退出——「配方命中 0 行」
#     和「这件事没发生过」是两件事，前者同样要出现在 issue 里。
#   * **行数封顶在脚本里，不在脑子里。** 每张表都有上限，被截断时明说「本表共 N 行」，
#     所以读到的一定是完整的一页，或一句诚实的截断声明。
#   * **只读不写。** 不改日志、不在日志旁边留中间文件。原始日志自己留着（§1）。

set -u

LOG="${1:-}"
if [ -z "$LOG" ]; then
    echo "usage: bash tools/collect_log.sh <日志文件.jsonl|.jsonl.gz>" >&2
    exit 1
fi
if [ ! -f "$LOG" ]; then
    echo "找不到日志文件：$LOG" >&2
    exit 1
fi
for tool in jq awk; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "缺少 $tool，装一个再跑：apt-get install -y $tool" >&2
        exit 1
    }
done

TMP="$(mktemp)" || exit 1
trap 'rm -f "$TMP"' EXIT

# 只取 JSON 行：首行是 "listening on 0.0.0.0:<port>"，不是 JSON（§2）。
if [ "${LOG##*.}" = "gz" ]; then
    events() { gzip -dc "$LOG" 2>/dev/null | grep '^{'; }
else
    events() { cat "$LOG" 2>/dev/null | grep '^{'; }
fi

# 印一张表。配方先把结果写进 $TMP，再调这里；超上限就截断并声明总行数。
section() {
    local title="$1" cap="$2" total
    echo
    echo "----- $title -----"
    total=$(wc -l < "$TMP" | tr -d ' ')
    if [ "$total" -eq 0 ]; then
        echo "（0 行）"
    elif [ "$total" -gt "$cap" ]; then
        head -n "$cap" "$TMP"
        echo "…（本表共 $total 行，以上为前 $cap 行）"
    else
        cat "$TMP"
    fi
}

echo "===== collect_log.sh · $LOG ====="

# 解不开的 gz 和空文件都会让下面十三张表全部印「0 行」，而那是**假的**——所以先验一次，
# 说清楚。看日志的人不该从「十三张空表」里推断「这场什么都没发生」。
if [ "${LOG##*.}" = "gz" ] && ! gzip -t "$LOG" 2>/dev/null; then
    echo "**这个文件不是有效的 gzip，解不开。**下面全是空的，不是这场没数据。" >&2
    echo "（检查一下是不是下载时就坏了，或者其实不是 .gz）" >&2
fi
COUNTED="$(events | jq -r '.event' | sort | uniq -c | sort -rn | awk '{printf "%s=%s ", $2, $1}')"
if [ -z "$COUNTED" ]; then
    echo "事件计数：（解析不出任何事件——文件是空的，或者里面没有以 { 开头的行）"
else
    echo "事件计数：$COUNTED"
    echo "（第一个数是 round 的行数；它明显小于该场回合数就说明日志被截过）"
fi

# ---------------------------------------------------------------- 表 1 造塔计划
events | jq -r 'select(.event=="tower_plan")
  | "\(.data.towers)\t\(.data.mayBuild)\t\(.data.upgradeReachable)\t\(.data.reserve)\t\(.data.guard)"' \
  | sort | uniq -c | sed 's/^ *//' | sort -k2 -n > "$TMP"
section "表 1 · 造塔计划 tower_plan　列：回合数 塔数 mayBuild upgradeReachable reserve guard" 20

# ---------------------------------------------------------------- 表 2 经济台账
events | jq -r 'select(.event=="buy" or .event=="sell")
  | [.data.round,.event,.data.role,.data.item,.data.num,.data.gold] | @tsv' > "$TMP"
section "表 2a · 一进一出　列：回合 方向 角色 物品 数量 当时金币" 60

events | jq -r 'select(.event=="shopping") | "\(.data.affordable)\t\(.data.need)"' \
  | sort | uniq -c | sed 's/^ *//' > "$TMP"
section "表 2b · 想买 vs 买得起　列：回合数 affordable 需求（affordable=false 就是没钱买）" 20

events | jq -r 'select(.event=="round") | "\(.data.round)\t\(.data.gold)"' \
  | awk -F'\t' '$2 != prev {print; prev = $2}' > "$TMP"
section "表 2c · 钱包台阶（只留金币真正变动的回合）　列：回合 金币" 60

# ---------------------------------------------------------------- 表 3 任务结局
events | jq -r 'select(.event=="task_ended") | "\(.data.reason)\t\(.data.success)"' \
  | sort | uniq -c | sed 's/^ *//' > "$TMP"
section "表 3a · 结局直方图　列：session 数 结束原因 成功否" 20

events | jq -r 'select(.event=="task_ended")
  | [.data.session,.data.reason,.data.success,.data.wrongAnswers,.data.cmdRounds] | @tsv' > "$TMP"
section "表 3b · 逐 session　列：session 原因 成功 判错次数 回合数" 40

# ---------------------------------------------------------------- 表 4 判题器裁定
events | jq -r 'select(.event=="task_answer_submit")
  | [.data.round,.data.session,.data.chars,.data.flipped,.data.rewritten,.data.wrongSoFar] | @tsv' > "$TMP"
section "表 4a · 每次提交　列：回合 session 字符数 换过外形 被改写 当时已错几次" 40

events | jq -r 'select(.event=="round") | select(.data.errors)
  | [.data.round, (.data.errors|join(",")), (.data.errorDescs // [] | join(" | "))] | @tsv' > "$TMP"
section "表 4b · 判题器回过错的回合　列：回合 错误码 判题器原话（码 2 = 答案不对或不完整）" 40

# ---------------------------------------------------------------- 表 5 封门
events | jq -r 'select(.event=="wall_gate_seal" or .event=="wall_gate_open")
  | [.data.round, (((.data.round-1)/130)|floor)+1, ((.data.round-1)%130)+1, .event,
     ((.data.away // []) | tostring), ((.data.stuck // []) | tostring)] | @tsv' > "$TMP"
section "表 5a · 封门逐回合　列：回合 第几天 当天第几回合 事件 没归队的 走不回岗位的" 160

events | jq -r 'select(.event=="wall_gate_open") | (((.data.round-1)/130)|floor)+1' \
  | sort -n | uniq -c | sed 's/^ *//' > "$TMP"
section "表 5b · 每天开了几回合（15 = 那一夜整夜没封）　列：回合数 第几天" 20

events | jq -r 'select(.event=="wall_gate_open") | .data.away[]? | @tsv' \
  | awk -F'\t' '{c[$1]++; last[$1]=$2","$3} END {for (id in c) print id, c[id], last[id]}' \
  | sort -k2 -nr > "$TMP"
section "表 5c · 卡在谁身上　列：角色 没归队的回合数 最后一次位置" 20

# ---------------------------------------------------------------- 表 6 夜间塔况
events | jq -r 'select(.event=="night_debug") | .data.pairs[]?.reason' \
  | sort | uniq -c | sed 's/^ *//' > "$TMP"
section "表 6a · 夜间沉默原因计数（fired 是默认值 = 开火了）　列：回合数 原因" 20

events | jq -r 'select(.event=="night_debug") | .data.pairs[]? | "\(.tower)\t\(.reason)"' \
  | sort | uniq -c | sed 's/^ *//' > "$TMP"
section "表 6b · 哪座塔在沉默　列：回合数 塔id 原因" 40

echo
echo "===== 以上即 §7.3 六张表，整段贴进 issue 就行 ====="
