#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""交付用的日志切片：把一场的 stdout 原始日志跑成「证据表」。

    python tools/collect_log.py <日志文件>
    python tools/collect_log.py <日志文件> --day 5

把**整段输出**贴进 issue 即可（WORKFLOW_REQUEST §7.3）。这个脚本**就是 §7.3 那六张表**，
而且是唯一要跑的东西：筛哪些事件、怎么聚合、每张表印多少行，全在这里定死。
所以你不需要判断「贴多少才够」「要不要汇总」——给它一个日志文件，它给你一页，那一页就是
要贴的东西。

为什么是 Python 而不是 jq：内网 agent 在 **Windows** 上跑，jq / awk / grep / gzip 不一定
有——jq 恰恰是 Git for Windows 唯一不自带的那个。Python 3 的标准库自带 json 和 gzip，
一个依赖顶四个，而且三个平台跑法一样。**整个脚本只用标准库**，不需要 pip install。

四个刻意的设计：

  * **永不失败。** 某个事件一行都没有，它印「（0 行）」而不是报错退出——「表印出 0 行」
    和「这件事没发生过」是两件事，前者同样要出现在 issue 里。
  * **行数封顶在脚本里，不在脑子里。** 每张表都有上限，被截断时明说「本表共 N 行」，
    所以读到的一定是完整的一页，或一句诚实的截断声明。
  * **只读不写。** 不改日志、不在日志旁边留中间文件。原始日志自己留着（§1）。
  * **不认识的东西要说出来。** 解不开的压缩包、读不了的编码、解析不了的行，都计数并在
    开头声明——十六张空表绝不能被读成「这场什么都没发生」。

输入格式：`.jsonl`、`.jsonl.gz`、`.zip`（Windows 上多半是压缩包）都行。BOM、CRLF、
UTF-16、GBK 都能读，都是 Windows 上真的会遇到的东西。
"""

import gzip
import io
import json
import sys
import zipfile
import zlib
from collections import Counter

# 解压会扔出来的东西，一个都不能漏：坏 DEFLATE 流是 zlib.error，被截断的 gz 是
# EOFError，两者都不是 OSError——只接 OSError 的话，一个下载到一半的日志会让脚本
# 甩一段 traceback 出来，而这时最该说清楚的一句话（"下面全是空的，不是这场没数据"）
# 恰恰没印出来。
ARCHIVE_ERRORS = (OSError, EOFError, zlib.error, zipfile.BadZipFile)

# 每张表的行数封顶。**这是预算的唯一出处**——WORKFLOW_REQUEST §7.3 的预算表和 §8 的
# cap_lines_total 都是这个数，CoreGeek/tests/collect_log.rs 会核对总量不超过 640 行。
# 加一节而忘了给预算，或者把某一节放大到超出总量，都会在那里失败。
CAPS = {
    "tower_plan": 20,      # 聚合式，恒定
    "ledger_in_out": 60,   # 钱动了多少回
    "shopping": 20,        # 聚合式，恒定
    "wallet": 60,          # 金币台阶，一天最多几级
    "endings": 20,         # 聚合式，恒定
    "sessions": 40,        # session 数
    "submits": 40,         # 提交次数
    "judger_errors": 40,   # 出错回合数
    "cmd_results": 40,     # 沙盒命令数（一场 10-20 条，留足余量）
    "cmd_reasons": 20,     # 聚合式，恒定
    "gate_rounds": 160,    # 每天最多 15 行，10 天 150 + 封门行
    "gate_nights": 20,     # 一天一行
    "gate_culprit": 20,    # 聚合式，恒定
    "night_reasons": 20,   # 聚合式，恒定
    "night_towers": 40,    # 塔数 × 原因数
    "score_attr": 12,      # 一天一行，10 天 + 余量
}
CAP_TOTAL = sum(CAPS.values())

# `--day N` 点名的逐回合明细。**不计入 640**：那十六张是每批都要贴的，这个是点了名才印的。
DRILL_CAP = 160

# 一天/一夜的回合数，用来把 round 换算成「第几天、当天第几回合」。
ROUNDS_PER_DAY = 130


# --------------------------------------------------------------------- 读日志

def read_text(path):
    """把日志文件读成文本，顺手解压、认编码。

    返回 (text, notes)，notes 是给读者看的说明（空列表 = 一切正常）。这里不抛异常：
    读不了就是「读不了」，由调用方决定怎么说，而不是让脚本半路死掉。
    """
    notes = []
    try:
        with open(path, "rb") as fh:
            raw = fh.read()
    except OSError as err:
        return None, ["**打不开这个文件**：%s" % err]

    # 压缩包先解开。看开头两字节而不是看扩展名——从浏览器/内网下载下来的东西，
    # 扩展名经常是错的（.gz 里其实是个 zip，或者反过来）。
    if raw[:2] == b"\x1f\x8b":
        try:
            raw = gzip.decompress(raw)
        except ARCHIVE_ERRORS as err:
            return None, [
                "**这个文件是坏的 gzip，解不开**：%s" % err,
                "下面全是空的，**不是**这场没数据。检查一下是不是下载时就断了。",
            ]
    elif raw[:2] == b"PK":
        try:
            with zipfile.ZipFile(io.BytesIO(raw)) as zf:
                names = [i for i in zf.infolist() if not i.is_dir()]
                if not names:
                    return None, ["**这个 zip 里没有文件。**"]
                # 挑最像日志的那个：优先 .jsonl / .txt / .log，否则挑最大的。
                liked = [i for i in names if i.filename.lower().endswith((".jsonl", ".txt", ".log"))]
                chosen = max(liked or names, key=lambda i: i.file_size)
                raw = zf.read(chosen)
                notes.append("（解压出 %s）" % chosen.filename)
        except ARCHIVE_ERRORS as err:
            return None, [
                "**这个文件是坏的 zip，解不开**：%s" % err,
                "下面全是空的，**不是**这场没数据。",
            ]

    # 编码。UTF-16 只看 BOM：Python 的 utf-16 解码器在没有 BOM 时按小端硬解，几乎不会
    # 失败——一段 GBK 中文会被它"成功"解成乱码，然后一路安静地贴进 issue。宁可只认
    # BOM，也不能让一个能解错的解码器排在真解码器前面。
    if raw[:3] == b"\xef\xbb\xbf":
        return raw.decode("utf-8-sig"), notes
    if raw[:2] in (b"\xff\xfe", b"\xfe\xff"):
        # 记事本另存过的 UTF-16：Windows 上真的会遇到
        return raw.decode("utf-16"), notes
    for enc in ("utf-8", "gbk"):
        try:
            return raw.decode(enc), notes
        except UnicodeDecodeError:
            continue
    notes.append("**这个文件不是 UTF-8 也不是 GBK**，坏字节已替换成 ?，个别字段可能不准。")
    return raw.decode("utf-8", "replace"), notes


def parse(text):
    """JSONL -> 记录列表，另附解析不了的行数。

    首行是 `listening on 0.0.0.0:<port>`，不是 JSON（§2），所以只认 { 开头的行。
    """
    records, unparsed = [], 0
    for line in text.splitlines():
        line = line.strip()          # CRLF：Windows 上一定会遇到
        if not line.startswith("{"):
            continue
        try:
            rec = json.loads(line)
        except ValueError:
            unparsed += 1
            continue
        if isinstance(rec, dict):
            records.append(rec)
        else:
            unparsed += 1
    return records, unparsed


# ----------------------------------------------------------- jq 的那几个语义

def pick(records, *events):
    """按事件名取记录。**这个脚本里每一处按事件筛选都必须走这里。**

    集中成一个函数有两个理由：一是读起来就是「我要这两个事件」，二是
    CoreGeek/tests/collect_log.rs 会把这个函数的每个参数拿去和 src/ 里
    log::event("…") 的调用点对账——事件在那边改了名而这里没跟着改，那一节就会
    安安静静地印「（0 行）」，而 0 行会被读成「这件事没发生过」。
    """
    wanted = set(events)
    return [r for r in records if r.get("event") in wanted]


def data(rec):
    """记录里的 data 块。缺了当空字典——`select(.data.x)` 在 jq 里也是这个语义。"""
    block = rec.get("data")
    return block if isinstance(block, dict) else {}


def jstr(value):
    """jq 字符串插值 `"\\(.x)"` 的语义：null 印成 null，布尔印成 true/false。"""
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (dict, list)):
        return json.dumps(value, separators=(",", ":"), ensure_ascii=False)
    return str(value)


def tsv(values):
    """jq `@tsv` 的语义：制表符分隔，null 是**空字符串**（不是 "null"）。"""
    return "\t".join("" if v is None else jstr(v) for v in values)


def tostring(value):
    """jq `tostring`：紧凑 JSON，没有多余空格，中文不转义。"""
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def one_line(value):
    """一格里的自由文本（脚本打印的开头）：把制表符和换行压成空格。

    表是 TSV，一格里的 `\\t` 会把这一行劈成两行——`cmd_result.head` 是脚本文本的
    前 160 字符，正是最可能带制表符的东西。空/缺席印空串（缺失当没发生）。
    """
    if value is None:
        return ""
    return " ".join(str(value).split())


def day_of(round_no):
    return (round_no - 1) // ROUNDS_PER_DAY + 1


def in_day(round_no):
    """当天第几回合（1..130）。等价于 jq 的 `((.round-1)%130)+1`。"""
    return round_no - (day_of(round_no) - 1) * ROUNDS_PER_DAY


def counted(rows, order=sorted, post=None):
    """`sort | uniq -c | sed 's/^ *//'` —— 计数在前，后面跟原始行。

    两个排序参数对应管道里的两个 sort，位置不能混：`order` 作用在**去重后的原始行**上
    （管道第一个 `sort`），`post` 作用在**加好计数的整行**上（表 1 末尾那个
    `sort -k2 -n`）。混了就会按错的列排——表 1 的"第二列"是塔数还是 mayBuild，差的就是
    这一步。
    """
    tally = Counter(rows)
    lines = ["%d %s" % (tally[row], row) for row in order(list(tally))]
    return sorted(lines, key=post) if post else lines


def by_number_in(field):
    """`sort -k<n> -n`：按空白分隔的第 n 列**数值**排序（不是字典序，否则 10 < 2）。"""
    def key(row):
        parts = row.split()
        try:
            return (0, float(parts[field - 1]), row)
        except (IndexError, ValueError):
            return (1, 0.0, row)
    return key


# ------------------------------------------------------------------ 出表的壳

def section(
title, lines, cap):
    """印一张表。超上限就截断，并**声明真实行数**。"""
    _emit(title, lines, cap)


def drill(title, lines, cap):
    """`--day N` 时加印的逐回合明细。和 section 分开只是为了封顶预算好核对：
    这十三张是每次都要贴的，drill 是点了名才印的。"""
    _emit(title, lines, cap)


def _emit(title, lines, cap):
    out("")
    out("----- %s -----" % title)
    total = len(lines)
    if total == 0:
        out("（0 行）")
    elif total > cap:
        for line in lines[:cap]:
            out(line)
        out("…（本表共 %d 行，以上为前 %d 行）" % (total, cap))
    else:
        for line in lines:
            out(line)


def out(text):
    sys.stdout.write(text + "\n")


# --------------------------------------------------------------------- 主流程

def main(argv):
    # 参数从头到尾扫一遍，按顺序吃掉 --day 的值——不然 `--day 5 log.jsonl` 会把 5
    # 当成日志文件名（顺序反了就是错的，而它看起来完全合理）。
    path, day, coach, rest = None, None, False, []
    todo = list(argv[1:])
    while todo:
        arg = todo.pop(0)
        if arg in ("--help", "-h"):
            out(__doc__.strip())
            return 0
        if arg == "--coach":
            coach = True
            continue
        if arg == "--day":
            if not todo:
                sys.stderr.write("--day 后面要跟一个天数，比如 --day 5\n")
                return 1
            arg = todo.pop(0)
        elif arg.startswith("--day="):
            arg = arg.split("=", 1)[1]
        elif arg.startswith("-"):
            sys.stderr.write("不认识的参数：%s（只有 --day N）\n" % arg)
            return 1
        else:
            rest.append(arg)
            continue
        try:
            day = int(arg)
        except ValueError:
            sys.stderr.write("--day 要的是一个天数，比如 --day 5\n")
            return 1

    if not rest:
        sys.stderr.write("用法：python tools/collect_log.py <日志文件.jsonl|.gz|.zip> [--day N]\n")
        return 1
    path = rest[0]

    out("===== collect_log.py · %s =====" % path)

    text, notes = read_text(path)
    for note in notes:
        out(note)
    if text is None:
        return 1

    records, unparsed = parse(text)
    if unparsed:
        out("**有 %d 行以 { 开头但不是合法 JSON**，已跳过。日志可能被截断或改写过。" % unparsed)

    # 事件计数。它也是「日志全不全」的唯一自检：round 的行数明显小于该场回合数，
    # 就说明这份日志被截过。
    tally = Counter(r.get("event") for r in records)
    counts = " ".join(
        "%s=%s" % (jstr(name), n)
        for name, n in sorted(tally.items(), key=lambda kv: (-kv[1], jstr(kv[0])))
    )
    if not counts:
        out("事件计数：（解析不出任何事件——文件是空的，或者里面没有以 { 开头的行）")
    else:
        out("事件计数：%s" % counts)
        out("（第一个数是 round 的行数；它明显小于该场回合数就说明日志被截过）")

    if coach:
        coach_block(records)
    else:
        build_tables(records, day)
        out("")
        # 结尾这句是给司机看的最后一句，所以它得说清「贴到哪为止」。带 --day 时后面还有
        # 几节 drill，笼统写「以上即六张表」会让他只贴前半段。
        if day is None:
            out("===== 以上即 §7.3 六张表，整段贴进 issue 就行 =====")
        else:
            out("===== 以上整段贴进 issue：先是六张表，后面是第 %d 天的逐回合明细 =====" % day)
    return 0


def coach_block(records):
    """`--coach`：回执里 `coach` 那一块（§5），以及 issue 头部的教练汇总行。

    单独一个模式，不混进那十三张表里，因为**去向不同**：表是贴进 issue 正文的，
    这一块是填进 `receipt.json` 的。混在一起，司机就得在一页输出里挑挑拣拣。
    """
    moves = []
    for r in pick(records, "coach_move"):
        d = data(r)
        moves.append({k: d.get(k) for k in ("round", "day", "switch", "from", "to", "why")})
    out("receipt.coach.moves（整段贴进 receipt.json 的 coach.moves）：")
    out(json.dumps(moves, separators=(",", ":"), ensure_ascii=False))

    halves, source = 0, "本场没有 coach_half，按 0 写"
    half = pick(records, "coach_half")
    if half:
        halves, source = data(half[-1]).get("halves") or 0, "最后一个 coach_half"
    else:
        ready = pick(records, "coach_ready")
        if ready:
            halves, source = data(ready[-1]).get("halvesLearned") or 0, "coach_ready.halvesLearned"
    out("receipt.coach.halves：%s（%s）" % (jstr(halves), source))

    by_switch = Counter(m.get("switch") for m in moves)
    detail = "（%s）" % " / ".join(
        "%s %d" % (jstr(s), n)
        for s, n in sorted(by_switch.items(), key=lambda kv: (-kv[1], jstr(kv[0])))
    ) if moves else ""
    out("本场汇总：移动 %d 次%s%s" % (
        len(moves), detail,
        "" if moves else "　← 这一行照样要有，写 0",
    ))
    if not moves and not half and not pick(records, "coach_ready"):
        out("（本场没有任何 coach_* 事件——日志可能是旧版本，或教练没启用。"
            "回执里照实写 0，别猜。）")


def score_rows(records):
    """每天最后一个 `round` 记录里的分数归属，外加最后一行。

    `任务书` 第六章把总分拆成三块：`score_1` 任务、`score_2` 击杀、`score_3` 生存
    （`Σ 10×day×存活系数`，满 550）。回执里只有**总分**，于是"这 41 分是怎么来的"
    一直靠 `coach_night` 的夜间掉血去倒推——倒推得出"基地哪天倒"，推不出三块各多少。

    这三块其实**就在日志里**：`round.scoreAttr` 把当时的总分拆成
    `kill`（score_2，累计）、`survival`（score_3，累计）、`residual`（余项，即 score_1
    加归属误差）。每个 `round` 记录都带，而收集脚本一行都没印过。
    """
    rounds = pick(records, "round")
    if not rounds:
        return []
    rows, seen = [], set()
    for index, rec in enumerate(rounds):
        block = data(rec)
        round_no = block.get("round")
        if not isinstance(round_no, int):
            continue
        # 每天的最后一个回合：一天一行，最后一行一定是当天的收官。
        is_day_end = (index + 1 == len(rounds)) or (
            isinstance(data(rounds[index + 1]).get("round"), int)
            and day_of(data(rounds[index + 1]).get("round")) != day_of(round_no)
        )
        if not is_day_end or day_of(round_no) in seen:
            continue
        seen.add(day_of(round_no))
        attr = block.get("scoreAttr") or {}
        rows.append(tsv([
            day_of(round_no), round_no, block.get("score"),
            attr.get("kill"), attr.get("survival"), attr.get("residual"),
            block.get("stationHp"), block.get("enemyStationHp"),
        ]))
    return rows


def build_tables(records, day):
    # ------------------------------------------------------------ 表 0 分数归属
    section(
        "表 0 · 分数归属（每天最后一个回合）　列：第几天 回合 总分 击杀分 survival residual 我方基地 对方基地"
        "（`survival` = 任务书第六章的 score_3，满 550；`residual` = score_1 加归属误差）",
        score_rows(records), CAPS["score_attr"],
    )

    # ------------------------------------------------------------ 表 1 造塔计划
    section(
        "表 1 · 造塔计划 tower_plan　列：回合数 塔数 mayBuild upgradeReachable reserve guard 外层缺口",
        counted(
            [
                tsv([data(r).get("towers"), data(r).get("mayBuild"),
                     data(r).get("upgradeReachable"), data(r).get("reserve"),
                     data(r).get("guard"), data(r).get("secondLayer")])
                for r in pick(records, "tower_plan")
            ],
            post=by_number_in(2),   # `sort -k2 -n`：加好计数之后再按第二列（塔数）升序
        ), CAPS["tower_plan"],
    )

    # ------------------------------------------------------------ 表 2 经济台账
    ledger = []
    for r in pick(records, "buy", "sell"):
        d = data(r)
        ledger.append(tsv([d.get("round"), r.get("event"), d.get("role"),
                           d.get("item"), d.get("num"), d.get("gold")]))
    section("表 2a · 一进一出　列：回合 方向 角色 物品 数量 当时金币", ledger, CAPS["ledger_in_out"])

    section(
        "表 2b · 想买 vs 买得起　列：回合数 affordable 需求（affordable=false 就是没钱买）",
        counted([tsv([data(r).get("affordable"), data(r).get("need")])
                 for r in pick(records, "shopping")],
                lambda t: sorted(t)), CAPS["shopping"],
    )

    # 钱包台阶：只留 gold 真正变动的回合，所以 20 天的金币曲线是一列台阶而不是 1300 行。
    steps, prev = [], object()
    for r in pick(records, "round"):
        gold = data(r).get("gold")
        if gold != prev:
            steps.append(tsv([data(r).get("round"), gold]))
            prev = gold
    section("表 2c · 钱包台阶（只留金币真正变动的回合）　列：回合 金币", steps, CAPS["wallet"])

    # ------------------------------------------------------------ 表 3 任务结局
    section(
        "表 3a · 结局直方图　列：session 数 结束原因 成功否",
        counted([tsv([data(r).get("reason"), data(r).get("success")])
                 for r in pick(records, "task_ended")],
                lambda t: sorted(t)), CAPS["endings"],
    )
    # P1-1：`wrongAnswers` 现在是"放弃计数器"，判题器说出新错因就会被清零——所以它
    # 单独一列会在一场提交了四次、被拒四次的 session 上印 0。`rejections` 是单调的
    # 那个（本次 session 一共烧掉几个答案），两列一起才对得上"几次"问的是哪一个。
    section(
        "表 3b · 逐 session　列：session 原因 成功 拒绝次数 判错次数 回合数",
        [tsv([data(r).get("session"), data(r).get("reason"), data(r).get("success"),
              data(r).get("rejections"), data(r).get("wrongAnswers"),
              data(r).get("cmdRounds")])
         for r in pick(records, "task_ended")], CAPS["sessions"],
    )

    # ---------------------------------------------------------- 表 4 判题器裁定
    # 末两列是同一件事的两个问题：`rejections` = 这个 session 一共烧掉几个答案（单调），
    # `wrongSoFar` = 当前的放弃计数（判题器说新东西就归零）。P1-1 之前两者恒等，之后不
    # 是——只印后者会在一场第四次重试上印 0，读起来像"第一次提交"。
    # 末列 `rejectionFeedback` = 本次重试的 prompt 里带了几条判题器原话（§10 请求六之三）：
    # 它是"P0-1 的反馈到底有没有进 prompt"唯一能证伪的一格。
    section(
        "表 4a · 每次提交　列：回合 session 字符数 换过外形 被改写 判错几次 拒绝几次 带回错因",
        [tsv([data(r).get("round"), data(r).get("session"), data(r).get("chars"),
              data(r).get("flipped"), data(r).get("rewritten"),
              data(r).get("wrongSoFar"), data(r).get("rejections"),
              data(r).get("rejectionFeedback")])
         for r in pick(records, "task_answer_submit")], CAPS["submits"],
    )
    # `errors` 缺席 = 这回合没出错（压缩规则 1：空数组不落盘），所以这一节天然只印
    # 判题器回过错的回合——不是这一节漏了。
    section(
        "表 4b · 判题器回过错的回合　列：回合 错误码 判题器原话（码 2 = 答案不对或不完整）",
        [tsv([data(r).get("round"),
              ",".join(jstr(e) for e in data(r).get("errors") or []),
              " | ".join(str(x) for x in data(r).get("errorDescs") or [])])
         for r in pick(records, "round") if data(r).get("errors")], CAPS["judger_errors"],
    )

    # 表 4c/4d 是 v15 §10 请求七要的两张：2026-09-14(b) 那五场里，13 条
    # `task_cmd_failed` 只有 `{"exit":0,"timeout":false}` 两个字段，被读成"沙盒坏了"
    # ——实际是脚本跑完了但没打印 `ANSWER:` 行；而 `cmd_result` 只有字符数，没有一行
    # 说得清脚本到底返回了什么。现在两个字段都有了，这两张表把它们摊开。
    #
    # `有答案` 为空 = 那条日志是加这个字段之前跑的；**不是 false**。
    section(
        "表 4c · 每条沙盒命令　列：回合 session 字符数 有答案 结果开头"
        "（`有答案` 空 = 该日志早于 v15，没有这一格）",
        [tsv([data(r).get("requestRound"), data(r).get("session"),
              data(r).get("chars"), data(r).get("answer"),
              one_line(data(r).get("head"))])
         for r in pick(records, "cmd_result")], CAPS["cmd_results"],
    )
    section(
        "表 4d · 没有答案的原因　列：次数 原因"
        "（`no_answer_marker` = 沙盒跑完了、脚本没打印 ANSWER 行，不是沙盒故障；"
        "`timeout` = 脚本超过 15 秒；`exit_nonzero` = 脚本自己退出码非 0）",
        counted([jstr(data(r).get("reason")) for r in pick(records, "task_cmd_failed")],
                lambda t: sorted(t)), CAPS["cmd_reasons"],
    )

    # ---------------------------------------------------------------- 表 5 封门
    gate = pick(records, "wall_gate_seal", "wall_gate_open")
    section(
        "表 5a · 封门逐回合　列：回合 第几天 当天第几回合 事件 没归队的 走不回岗位的",
        [tsv([data(r).get("round"), day_of(data(r).get("round") or 0),
              in_day(data(r).get("round") or 0),
              r.get("event"), tostring(data(r).get("away") or []),
              tostring(data(r).get("stuck") or [])])
         for r in gate], CAPS["gate_rounds"],
    )
    open_nights = Counter(day_of(data(r).get("round") or 0)
                          for r in pick(records, "wall_gate_open"))
    section(
        "表 5b · 每天开了几回合（15 = 那一夜整夜没封）　列：回合数 第几天",
        ["%d %d" % (n, d) for d, n in sorted(open_nights.items())], CAPS["gate_nights"],
    )
    stuck_on = {}
    for r in pick(records, "wall_gate_open"):
        for who in data(r).get("away") or []:
            if isinstance(who, list) and who:
                entry = stuck_on.setdefault(who[0], [0, ""])
                entry[0] += 1
                entry[1] = ",".join(str(v) for v in who[1:])
    section(
        "表 5c · 卡在谁身上　列：角色 没归队的回合数 最后一次位置",
        ["%s %d %s" % (who, n, last) for who, (n, last)
         in sorted(stuck_on.items(), key=lambda kv: (-kv[1][0], str(kv[0])))], CAPS["gate_culprit"],
    )

    # ------------------------------------------------------------ 表 6 夜间塔况
    section(
        "表 6a · 夜间沉默原因计数（fired 是默认值 = 开火了）　列：回合数 原因",
        counted([jstr(p.get("reason"))
                 for r in pick(records, "night_debug")
                 for p in data(r).get("pairs") or [] if isinstance(p, dict)],
                lambda t: sorted(t)), CAPS["night_reasons"],
    )
    section(
        "表 6b · 哪座塔在沉默　列：回合数 塔id 原因",
        counted([tsv([p.get("tower"), p.get("reason")])
                 for r in pick(records, "night_debug")
                 for p in data(r).get("pairs") or [] if isinstance(p, dict)],
                lambda t: sorted(t)), CAPS["night_towers"],
    )

    if day is not None:
        drill_day(records, day)


def drill_day(records, day):
    """点名要某一天/某一夜时加印的逐回合行（§7.3 里"我点名要哪一天"那条路）。

    三条明细各自封顶 160 行——一天最多 70 个白天回合 + 60 个夜晚回合，封顶够用，
    但不会因为日志里有重复行就无限长。
    """
    def on_day(rec):
        return day_of(data(rec).get("round") or 0) == day

    drill(
        "drill · 第 %d 天造塔计划逐回合　列：回合 塔数 缺口 预留 守备 墙缺 石需 队石 能造 能升基地" % day,
        [tsv([data(r).get("round"), data(r).get("towers"), data(r).get("gaps"),
              data(r).get("reserve"), data(r).get("guard"), data(r).get("wallGaps"),
              data(r).get("stoneDemand"), data(r).get("teamStone"),
              data(r).get("mayBuild"), data(r).get("upgradeReachable")])
         for r in pick(records, "tower_plan") if on_day(r)],
        DRILL_CAP,
    )
    drill(
        "drill · 第 %d 天封门逐回合　列：回合 第几回合 事件 没归队的 走不回岗位的" % day,
        [tsv([data(r).get("round"), in_day(data(r).get("round") or 0),
              r.get("event"), tostring(data(r).get("away") or []),
              tostring(data(r).get("stuck") or [])])
         for r in pick(records, "wall_gate_seal", "wall_gate_open") if on_day(r)],
        DRILL_CAP,
    )
    drill(
        "drill · 第 %d 夜夜间塔况逐回合　列：回合 机器人 pairs" % day,
        [tsv([data(r).get("round"), tostring(data(r).get("robots") or []),
              tostring(data(r).get("pairs") or [])])
         for r in pick(records, "night_debug") if on_day(r)],
        DRILL_CAP,
    )
    drill(
        "drill · 第 %d 天商店逐回合　列：回合 买得起否 需求 需求量 价格 金币 预留 最近商店 截止" % day,
        [tsv([data(r).get("round"), data(r).get("affordable"), data(r).get("need"),
              data(r).get("needNum"), data(r).get("price"), data(r).get("gold"),
              data(r).get("reserve"), data(r).get("buyerShopDist"), data(r).get("deadline")])
         for r in pick(records, "shopping") if on_day(r)],
        DRILL_CAP,
    )


if __name__ == "__main__":
    # Windows 上控制台默认是 GBK/cp936，中文表头会在 print 时炸掉；文件重定向时
    # 默认编码也可能是 cp936。统一按 UTF-8 输出并允许替换，**任何情况下都不因为
    # 一个字符印不出来就中断整页**。
    try:
        native = sys.stdout.encoding or ""
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        if native.lower().replace("-", "") not in ("utf8", "utf8bom", "cp65001", ""):
            out("（输出按 UTF-8 编码。终端里中文若是乱码，用 `> tables.txt` 重定向到文件再打开）")
    except (AttributeError, ValueError):
        pass
    sys.exit(main(sys.argv))
