# 对战数据交付规范（workflow agent 接口）

> **版本 v8** · 本文档是内网自动对战 workflow 的**交付规范**。
> 读者：拉代码 → 发起对战 → 抓日志 → 分析对局 → 生成改进 issue 的 code agent。
> 与上一版的区别：v7 是"请求"体例（请求一～七 + 每条的为什么），v8 按**接口**重写——
> 逐条写明产物、路径、字段、来源与验收方式，作废的要求直接删掉，不留讨论过程。
> §6 是你可以自己跑一遍的自检清单；§8 是同一份内容的机器可读版本。

---

## 0. 一句话

每场交付 3 份文件 + 1 份**结果回执**；每批在 issue 头部给 8 个字段。

分界线只有一条：**棋盘我看得见，结果我看不见。**

每个回合的请求里带着：回合号、地图、我方队名/ID/金币/总分、我方任务板、**双方所有
单位的血量与等级**、机器人、两家商店、判题器的错误码与原文。但 `teamEnemy` 里**只有
`roles`**——对方的总分、金币、任务提交，以及**这一场谁赢了**，我一律看不到。

所以这件事分成两半，各归各管：

| 半边 | 谁提供 | 怎么提供 |
|---|---|---|
| 逐回合过程数据 | 我（stdout JSONL） | 你**原样搬运**，一个字节都别加工（§2） |
| 结果、身份、版本 | 你 | 按 §3 的字段填进回执 |

---

## 1. 交付物清单

每场（`<match_id>`，例 `pk577716`）：

| # | 路径 | 内容 | 硬性要求 |
|---|---|---|---|
| 1 | `workflow/logs/<match_id>/ours.jsonl` | 我方进程的 stdout 逐字 | 不改写、不截断、不合并、不脱敏、**保持行序** |
| 2 | `workflow/logs/<match_id>/enemy.jsonl` | 对手进程的 stdout（可得时） | 同上；拿不到就放一份 `.missing` 说明原因 |
| 3 | `workflow/logs/<match_id>/receipt.json` | 结果回执（§3） | `jq .` 能解析；字段一个不少，取不到填 `null` |
| 4 | `workflow/analysis/<match_id>.md` | 分析报告全文 | 不截断。issue 正文可只摘结论，原文必须落在这条路径 |

> 你现有的目录布局与这里不一致时，**保持你自己的布局**，在 issue 里给出实际路径即可。
> 不要为了对齐本文档搬动文件——我按 issue 里给的路径找。

每批：issue 头部 8 个字段（§4）+ 每场一份 `receipt.json`（§3）+ 一行教练汇总（§5）。

---

## 2. 我方日志：只搬运，不加工

**格式**：stdout 是 JSONL，一行一个事件：

```json
{"ts":1789281855133,"event":"round","data":{"round":1,"day":1,…}}
```

- **首行不是 JSON**（`listening on 0.0.0.0:<port>`）。解析前跳过不以 `{` 开头的行。
- 每回合一条 `round`，另有一批 `task_*` / `coach_*` / `volley_review` / `night_debug` /
  `shopping` / `wall_build` … 事件混在同一流里。**回合数 ≠ 行数**，别按行数截取回合。
- **行序有意义**（事件按发生顺序落盘）。重排、按事件名分组都会破坏因果链。
- gzip 可以，但保留 `.jsonl.gz` 后缀与原始行序，并在 issue 里说明。

**禁止**：截断长字段、合并多行、给每行加队伍前缀或时间戳注解、脱敏 ID/坐标、
只保留 `round` 事件、只保留你分析里引用到的那几行。

我已经**自己**截断过的字段（不必再想办法还原，但分析时别把它们当全文）：

| 字段 | 上限 | 出现在 |
|---|---|---|
| `errorDescs[]` | 120 字符 | `round` |
| `phaseTask`、`lastCmdResult` | 160 字符 | `round` |
| `task_ended.bestAnswer` | 120 字符 | `task_ended` |
| `task_answer_found.answer` / `task_answer_sentinel.answer` / `task_answer_blocked.answer` | 120 / 60 / 60 字符 | 各自事件 |
| `prompt_sent.head`、`cmd_sent.head` | 300 字符 | 各自事件 |

**不截断的关键字段**：`task_answer_submit.answer` —— 交给判题器的**原始字节**，
v8 起全量记录（上限 4000 字符，正常答案几十到几百字符），另附 `chars` 记真实字符数。
判题器回 `MissingNamedInput` 时，缺的是哪个字段只能从这段原文里看出来，
所以这是任务 0 分唯一的一手证据。分析时若 `answer` 的实际字符数 < `chars`，
说明它被 4000 上限截断过（正常场次不会发生）。

---

## 3. 结果回执 `receipt.json`（本次交付的核心）

### 3.1 为什么必须是回执

决策器的输入全是**场内量**：双方基地与围墙血量、我方金币、当日召唤令计数、黄昏的
火力缺口估计。它能判断"该不该收火""该不该继续买令"，但**判断不了**这些调整换来了
什么。胜负、双方总分、分项得分、对手身份、base commit 这五样都在你的视野里，
不在我的视野里。没有回执，"这轮改动是不是变好了"只能靠猜。

### 3.2 示例

```json
{
  "match_id": "pk577716",
  "base_commit": "b03b1c8",
  "base_commit_time": "2026-09-13T08:30:00+08:00",
  "opponent": {"team_id": "4388", "team_name": "teamB_4388_道化黄泉路"},
  "result": "win",
  "result_source": "judger",
  "rounds": 256,
  "end_reason": "enemy_station_destroyed",
  "score": {"ours": 139, "enemy": 52},
  "score_breakdown": {
    "ours":  {"task": 91, "kill": 8,  "survival": 40},
    "enemy": {"task": 0,  "kill": 52, "survival": 0}
  },
  "station_hp_last": {"ours": 1500, "enemy": 0},
  "enemy_seen": {"station_level": 2, "towers": 3, "walls": 12},
  "tasks_ours": [
    {"session": 1, "task_type": "城市气候", "accepted_round": 12, "ended_round": 96,
     "reason": "completed", "submitted": true, "submissions": 2, "score": 47}
  ],
  "env": {"CG_LEARN": null, "CG_TUNE_CLEAR_GAP": null, "CG_TUNE_STATION_FOCUS": null},
  "coach": {
    "halves": 2,
    "moves": [
      {"round": 262, "day": 3, "switch": "harass", "from": "Rhythm", "to": "Off", "why": "summons_sterile"}
    ]
  }
}
```

### 3.3 字段表

| 字段 | 类型 | 来源 | 取不到时 |
|---|---|---|---|
| `match_id` | string | 你的对局 ID | 必填 |
| `base_commit` | string | 发起该场时仓库 HEAD 的短 hash | 必填（没有就不发这场） |
| `base_commit_time` | string | 该 commit 的提交时间，ISO8601 带时区 | `null` |
| `opponent.team_id` / `team_name` | string | 判题器对局详情 | `null` |
| `result` | `win`/`loss`/`draw`/`unknown` | 判题器判定 | 只此一处允许 `unknown` |
| `result_source` | string | `judger` / `series` / `inferred`，说明胜负从哪来 | 必填 |
| `rounds` | int | 该场最后一回合号 | `null` |
| `end_reason` | string | 判题器的结束原因原文 | `null` |
| `score.ours` / `score.enemy` | int | 双方总分 | **必填**（我唯一要不到的硬数据） |
| `score_breakdown` | object | 双方 task/kill/survival 分项 | 整块 `null`，别只填一半 |
| `station_hp_last` | object | 双方基地最后血量 | `null`（我也能从 `round.enemyStationHp` 自推） |
| `enemy_seen` | object | 对方最后回合的基地等级/塔数/墙数 | `null` |
| `tasks_ours[]` | array | **我方**每个任务 session 一行，见 §3.5 | 拿不到整块 `null` |
| `env` | object | 该场进程的 `CG_*` 原值（没有就是 `null`） | 必填（哪怕全 `null`） |
| `coach` | object | 从我的 JSONL 提取，配方见 §5 | 必填 |

### 3.4 三条硬规则

1. **取不到就填 `null`**：不要省略字段、不要猜、不要填 `0` / `""` / `"unknown"`。
   `0` 和 `null` 对我是两件相反的事——前者是"对方真的一分没得"，后者是"不知道"。
   用 `0` 冒充不知道，我会把它当成事实去归因。
2. **一个 match_id 一份回执**，不要合并多场；同一 commit 的多场也各写各的。
3. `base_commit` 必须是**实际跑这场时**的 HEAD。你的流程有拉代码 → 对战 → 抓日志 →
   分析的时延，我读到 issue 时 HEAD 通常已经变了：没有这个字段，分析里引用的行号与
   函数名可能已经不存在，我也无法 checkout 回去复现日志。
   （实例：issue #23 的分析生成于 08:53，而它描述的修复 `561640b` 提交于 08:47——
   6 分钟跑不完一场比赛，所以那份分析讲的是修复前的代码。）

### 3.5 `tasks_ours[]` 里每个 session 要有

| 字段 | 说明 |
|---|---|
| `session` | 我日志里的 `task.session`（`task_started` / `task_answer_submit` / `task_ended` 都带） |
| `task_type` | 任务类型（`task_started.head` 里有） |
| `accepted_round` / `ended_round` | 接取与结束回合；用于和我的日志对齐 |
| `reason` | `task_ended.reason`（`completed` / `timeout` / `wrong_answers` / …） |
| `submitted` | 是否提交过答案（`task_answer_submit` 是否出现过） |
| `submissions` | 提交次数 |
| `score` | **判题器给这个 session 的最终得分**——我这边看不到，只能靠 `totalScore` 反推 |

**为什么单列这张表**：任务超时结束时，判题器按"此前提交过的通过率最高的答案"结算
（任务书 `timeoutRounds` 条）。所以 `reason=timeout` **不等于 0 分**，把 timeout 一律
记成失败会误诊根因。我目前只能用 `totalScore - kill - survival` 反推出一个残差，
它把任务得分和估算误差混在一起，分不清"某个 session 真的 0 分"和"我算错了"。
逐 session 的 `score` 是唯一能把这两件事分开的数据。

---

## 4. issue 头部字段

| 字段 | 示例 | 说明 |
|---|---|---|
| 生成时间 | — | 保留 |
| 对战代码版本 | `pk575557: 8de90f3 (2026-09-13 08:26)` | 逐场列出；跨多个 commit 时逐行 |
| 本批次数 / 对手数 | `5 场 / 3 个对手` | 批规模 **≥5 场、≥2 对手**：3 场分不清信号与噪声 |
| 当前排名 | `12 / 64` | 全部参赛队中的名次 |
| 累计战绩 | `8 胜 5 负 1 平` | 上半场 + 下半场累计 |
| 胜率 | `57.1%` | 胜场 / 总场次，1 位小数 |
| 每场一行结果 | `pk577716 胜 139:52 (b03b1c8)` | 不翻 JSON 也能扫一眼 |
| 教练汇总 | 见 §5 | 格式固定，我会 grep |

排名与胜率是判断"改进是否真的有效"的唯一长期指标：单场胜负可能是运气（对手强弱、
出生点、地图随机），只有趋势能证伪。

---

## 5. 教练记录：从我的 JSONL 提取（jq 配方）

教练是决策器内置的在线自适应（`CoreGeek/src/brain/coach.rs`），事件有四种：
`coach_ready`（启动时的档位）、`coach_night`（每夜结算）、`coach_move`（**移动了一个
开关**）、`coach_half`（半场结束）。另外逐回合的 `round.data.policy` 记录**当时**档位。

```sh
# 1) 写进 receipt.coach.moves
grep '^{' ours.jsonl | jq -c 'select(.event=="coach_move")
  | .data | {round, day, switch, from, to, why}'

# 2) 写进 receipt.coach.halves（没有 coach_half 就用 coach_ready.halvesLearned）
grep '^{' ours.jsonl | jq -c 'select(.event=="coach_half") | .data.halves' | tail -1

# 3) issue 头部的教练汇总行
grep '^{' ours.jsonl | jq -r 'select(.event=="coach_move")
  | "\(.data.switch) \(.data.from)->\(.data.to) @\(.data.day)日 \(.data.why)"'
```

两个坑：`coach_move.from` / `to` 是**字符串**（布尔档位写作 `"true"` / `"false"`，
`harass` 是档位名 `Off` / `Rhythm` / `Rich`）；而 `round.data.policy.stationPressure`
是**布尔**。同名字段两种类型，别用一套解析。

issue 头部请给这一行（我直接 grep）：

```
教练：本批 N 场 | 移动 M 次（station_pressure a / gap_funding b / harass c） | 至少移动过一次的场次 X/N | 结果分布 胜 W / 负 L / 平 D
```

---

## 6. 交付前自检

每一场都跑一遍，任何一条不过就别发：

1. `jq . receipt.json > /dev/null` 通过；`jq -r 'keys[]' receipt.json` 包含 §3.3 的**全部**字段名。
2. `grep -c '"event":"round"' ours.jsonl` ≥ 该场回合数（行数只会更多，不会更少）。
3. `git cat-file -t <base_commit>` 返回 `commit`，且时间与该场吻合。
4. `receipt.coach.moves` 条数 == `grep -c '"event":"coach_move"' ours.jsonl`。
5. `receipt.coach.halves` == 最后一个 `coach_half` 的 `halves`（一场都没有则为 `0`）。
6. 没有把 `null` 写成 `0` / `""` / `"unknown"`；`result` 之外的字段出现 `"unknown"` 一律算错。
7. 对手日志拿不到时，在 issue 与回执里写明原因，而不是静默省略文件。

---

## 7. 反向反馈（三个问题，请在同一份 issue 里回答）

1. 分析时**最缺哪类信息**——哪些结论你只能猜？
2. 希望我新增哪些事件或字段（附一个你期望的 JSON 示例即可）？
3. 哪些现有事件是**噪音**，可以砍掉（我可以少写，降低体积与干扰）？

---

## 8. 机器可读指令块

```yaml
agent_request:
  target: workflow-driver
  repo: gamelife1314/Competition
  action: deliver_battle_data
  version: 8
  model: >
    We see the board (per-round state, both sides' units, our own score/gold) but never the
    result. Process data is ours to emit — transport it verbatim. Outcome, identity and
    version are yours to attach — fill them per battle.
  deliverables:
    per_battle:
      - path: workflow/logs/<match_id>/ours.jsonl
        content: our process stdout, verbatim JSONL
        rules: [no truncation, no rewriting, no merging, no redaction, preserve line order]
        note: first stdout line is not JSON ("listening on 0.0.0.0:<port>") — skip non-{ lines
      - path: workflow/logs/<match_id>/enemy.jsonl
        content: opponent stdout when obtainable
        if_unavailable: write workflow/logs/<match_id>/enemy.jsonl.missing stating why
      - path: workflow/logs/<match_id>/receipt.json
        content: outcome receipt (see receipt_fields)
        rules: [must parse with jq, every field present, unknown => null]
      - path: workflow/analysis/<match_id>.md
        content: full analysis report, untruncated
    per_batch:
      - issue_header_fields
      - one receipt.json per battle (never merged)
      - coach summary line
  receipt_fields:
    required: [match_id, base_commit, base_commit_time, opponent, result, result_source, rounds, end_reason, score, score_breakdown, station_hp_last, enemy_seen, tasks_ours, env, coach]
    score: {ours: int, enemy: int}                      # the one hard datum we cannot see
    score_breakdown: {ours: {task, kill, survival}, enemy: {task, kill, survival}}
    station_hp_last: {ours: int, enemy: int}
    enemy_seen: {station_level: int, towers: int, walls: int}
    tasks_ours:
      desc: one row per our task session; the judger's own per-session score
      fields: [session, task_type, accepted_round, ended_round, reason, submitted, submissions, score]
      why: >
        A timed-out task is settled on the best submission ever made, so reason=timeout does
        not mean zero. Our only proxy is totalScore - kill - survival, which mixes the task
        score with our estimation error and cannot separate a genuine zero from a misestimate.
    env:
      desc: the CG_* environment values the battle actually ran with (null when unset)
      keys: [CG_LEARN, CG_TUNE_CLEAR_GAP, CG_TUNE_STATION_FOCUS]
      why: an empty result here is itself the answer — it tells us no dial was forced.
    coach:
      source: our JSONL
      halves: last coach_half.halves (fallback coach_ready.halvesLearned, else 0)
      moves: every coach_move -> {round, day, switch, from, to, why}
  value_rules:
    null_not_zero: "0 and null mean opposite things (0 = really scored nothing, null = unknown)"
    one_receipt_per_match_id: true
    base_commit_per_battle: true
  issue_header_fields:
    - {name: 对战代码版本, per_battle: true, example: "pk575557: 8de90f3 (2026-09-13 08:26)"}
    - {name: 本批次数/对手数, requirement: ">= 5 battles, >= 2 opponents", example: "5 场 / 3 个对手"}
    - {name: 当前排名, example: "12 / 64"}
    - {name: 累计战绩, example: "8 胜 5 负 1 平"}
    - {name: 胜率, example: "57.1%"}
    - {name: 每场一行结果, example: "pk577716 胜 139:52 (b03b1c8)"}
    - {name: 教练汇总, format: "教练：本批 N 场 | 移动 M 次（station_pressure a / gap_funding b / harass c） | 至少移动过一次的场次 X/N | 结果分布 胜 W / 负 L / 平 D"}
  self_check:
    - jq . receipt.json succeeds and every field name in receipt_fields is present
    - grep -c '"event":"round"' ours.jsonl >= rounds in that battle
    - git cat-file -t <base_commit> == commit
    - receipt.coach.moves count == grep -c '"event":"coach_move"' ours.jsonl
    - no null replaced by 0 / "" / "unknown" (result is the sole exception)
    - a missing enemy log is explained, never silently omitted
  reverse_feedback:
    - Which information is the analysis missing (what can you only guess)?
    - Which new events/fields would you like (attach a JSON example)?
    - Which existing events are noise and can be dropped?
```

---

## 9. English summary

We see the board, never the result. Everything the per-round request carries is ours to log,
and we do: `/docs/WORKFLOW_REQUEST.md` v8 asks you to (1) transport our stdout JSONL
verbatim, (2) attach a per-battle outcome receipt, (3) attach the batch header fields.

1. **Transport, don't process** — `ours.jsonl` byte-for-byte: no truncation, no merging, no
   redaction, line order preserved. The first stdout line is not JSON. Rounds ≠ lines.
2. **Per-battle receipt** (`receipt.json`, one per `match_id`, never merged) — the fields we
   cannot observe from inside: winner, both totals, the task/kill/survival split, opponent
   identity, the actual `base_commit`, the per-session task scores, and the `CG_*` values the
   battle ran with. Unknown values must be `null`, never `0` — a fabricated zero is worse
   than a missing one, because we will attribute a change to it.
3. **Issue header** — per-battle commit, batch size (≥5 battles / ≥2 opponents), rank,
   cumulative record, win rate, one line per battle, and the fixed-format coach summary line.
4. **Self-check** (§6) — run it before publishing; it catches truncated files, missing
   fields, a wrong `base_commit`, and count mismatches against our own log.

The `coach` block is extracted from our JSONL with the jq recipes in §5; the coach itself
needs no environment variables and no control group from you.

---

*本文件由本地 agent 维护，作为向内部 workflow 下发数据交付指令的接口。*
