# 对自动对战 Workflow 的全部要求

> 载体：本文档 + git commit message（`git log` 可见完整请求）。
> 目标读者：驱动自动对战、下载日志、分析对局、生成「改进任务」issue 的内部 code agent / workflow。
> 版本：v6（本文档为唯一入口，取代原 `AGENT_REQUEST.md`）。
> v6 增量：请求六（任务健康度口径）、请求七（A/B 拨盘对战）、事件表更新（本轮新增事件）、
> 请求四备注（`errorDescs` 已由我方日志自带）。

---

## 请求一：issue 头部补充累计战绩字段

当前 issue 只含**单场**结果 + 本批次数（`分析场次: 1 (胜 0 / 负 1 / 平或未知 0)`），
缺少全局视角，无法判断改进是否让整体成绩上升。

请在「生成时间」下方增加：

| 字段 | 含义 | 示例 |
|---|---|---|
| 当前排名 | 全部参赛队中的名次 | `12 / 64` |
| 累计胜场 | 上半场+下半场累计获胜 | `8` |
| 累计负场 | 累计落败 | `5` |
| 累计平局 | 累计平局 | `1` |
| 胜率 | 胜场 / 总场次，保留 1 位小数 | `57.1%` |

**为什么**：单场失利可能是运气（对手强弱、出生点、地图随机）。只有排名与胜率的
长期趋势能说明改进是否有效，否则会出现"修了半天反而更差"却看不出来。

---

## 请求二：每轮发起的对战场数 3 → 5

**为什么**：3 场样本太小，无法区分信号与噪声（1 胜 2 负 vs 0 胜 3 负 统计上无意义），
3 场胜率在 33%/67% 间剧烈摆动，无法用于决策。5 场以上胜率才具参考价值，同时加快迭代循环。

---

## 请求三：记录每场对战所基于的代码 commit

请在「生成时间」下方增加：

| 字段 | 含义 | 示例 |
|---|---|---|
| 对战代码版本 | 发起该场对战时使用的 git commit（短 hash + 时间） | `b03b1c8 (2026-09-13 08:30)` |

一批对战跨越多个 commit 时，请逐场标记，或在批次头部列出：
```
历史对局代码版本（如涉及多个）：
  pk575557: 8de90f3 (2026-09-13 08:26)
  pk575412: b03b1c8 (2026-09-13 08:30)
```

**为什么**：workflow 拉代码 → 发起对战 → 下载日志 → 分析 → 生成 issue 存在**时间差**，
而我们本地持续在提交。读 issue 时我们的 HEAD 通常已经变了：

1. 分析里引用的**行号/函数名可能已不存在**（"照 issue 去改却找不到代码"）
2. 报告的问题**可能已被后续 commit 修掉**，白白浪费一轮改进
3. 没有 base commit **无法复现日志**（不能 checkout 到那个版本）
4. 无法把 commit 与胜率趋势关联（配合请求一）

**实例**：issue #23 的分析生成于 08:53，而 `561640b`（issue #22 修复）提交于 08:47——
6 分钟不可能跑完一场比赛，所以 #23 描述的是**修复前**的代码。若有 base commit 字段，
我们就能立刻判断"这条改进项是否已被修复"。

---

## 请求四：提供完整详细的日志（最重要）

### 现状问题

1. **issue 正文被截断**：每条改进项被截到 ~200 字符。例如 issue #23 里出现
   `北京任务答案 {"city"` 后**直接断掉**，看不到实际提交的完整答案，无法定位格式错误。
2. **原始日志不在仓库**：issue 引用的 `workflow/logs/pk577716/teamB_4388_道化黄泉路.log`
   和 `workflow/analysis/*.md` 在本仓库中**不存在**，我们拿不到逐回合原始数据。
3. **缺少对手视角**：只有我方日志时，无法理解对手为何能完成任务/守住基地
   （例如 issue #10 提到"对手靠 `MissingNamedInput` 拒绝描述重试 4 次成功"，
   但我们看不到对手的 LLM 交互细节）。

### 请提供（按重要性排序）

| # | 内容 | 形式 | 用途 |
|---|---|---|---|
| 1 | **我方完整原始 JSONL 日志** | 逐回合全量，不截断 | 复现决策链、定位代码位置 |
| 2 | **对手日志**（若可得） | 同上 | 学习对手成功的任务/防御策略 |
| 3 | **完整分析报告** | `workflow/analysis/*.md` 全文 | 我们的分析已被截断到无法使用 |
| 4 | **issue 正文不截断**（或阈值提到 ≥2000 字符） | — | 现在 200 字符切断关键信息 |
| 5 | **结构化关键事件摘要** | JSON，见下 | 便于自动比对与量化 |

### 建议的结构化事件摘要（每场）

```json
{
  "match_id": "pk577716",
  "base_commit": "561640b",
  "duration_rounds": 256,
  "result": {"series": "3:0", "single": "loss", "score": [52, 139]},
  "score_breakdown": {
    "ours":   {"task": 0, "kill": 5, "survival": 20},
    "enemy":  {"task": 139, "kill": 0, "survival": 0}
  },
  "tasks": {
    "ours":  [{"id": 1, "status": "failed", "reason": "schema_reject",
               "submitted": "<完整答案原文>", "error": "<errors[].description 全文>"}],
    "enemy": [{"id": 1, "status": "completed", "rounds": 16, "score": 91}]
  },
  "base_hp_timeline": [{"round": 1, "ours": 1500, "enemy": 1500}],
  "wall_hp_timeline":  [{"round": 60, "count": 20, "hp": 20000}],
  "robot_waves":       [{"night": 1, "spawned": 70, "killed": 45}],
  "towers":            [{"id": 20020, "level": 1, "fired_rounds": 12, "idle_rounds": 24}],
  "gold_curve":        [{"round": 1, "gold": 75}],
  "anomalies":         [{"round": 130, "event": "controller_withdrawn", "role": 20010}]
}
```

**为什么最关键**：`errors[].description` 是判题器返回的**唯一 schema 提示来源**
（我们的 `model.rs` 此前把它解析后丢弃了）。完整日志能让我们看到
"提交了什么 → 判题器说了什么 → 对手怎么改对的"这个完整闭环，
这是修复任务系统 0 分问题的核心证据。

> **v6 备注（我方侧已完成）**：自本版本对应 commit 起，我方 `round` 事件自带
> `errorDescs` 字段（每条判题器错误描述，截 120 字符）——你们可以直接从 JSONL 提取，
> 不必再从协议层另抓。若 120 字符不够，请告诉我们需要的截断长度。

---

## 请求五：明确告诉我们当前分析却什么信息

我们承诺持续优化自己的日志输出（stdout JSONL），但需要你们反馈：

1. **当前分析时最缺哪类信息**（哪些字段拿不到，只能猜）
2. **希望我们新增的事件名称与字段**（建议附 JSON 示例）
3. **哪些现有日志是噪音**（可以砍掉，降低体积与干扰）

我们当前已输出的事件：

| 事件 | 用途 |
|---|---|
| `round` | 每回合完整状态（cmds / failures / score / scoreAttr / scoreDelta / **errorDescs**） |
| `night_debug` | 夜间控制器–炮塔配对、距离、冷却、是否开火、静默原因 |
| `volley` | 齐射结果核对（fired/executed/damaged/kills/rejected） |
| `task_*` | 任务接取/开始/提交/结束、session、阶段；`task_answer_submit` 含**实际提交原文**；`task_answer_schema` 含缺/多字段；`task_fields` 含 FIELDS 侦察回显 |
| `sop_reuse` / `sop_evicted` / `sop_replay_skipped` | SOP 缓存复用、二次判错淘汰、拒重放同字节脚本 |
| `shopping` / `buy` | 采购意图、价格、可负担性、买家距离 |
| `tower_plan` / `build_blacklisted` | 建造计划与失败黑名单；`tower_plan.guard` 为第三塔资金守护 |
| `pair_recomputed` / `night_withdraw` | 夜间配对重算原因、控制器撤退；撤退滞回状态 |
| `door_reseal` / `wall_gate_*` | 门切开/黄昏重封/墙门封合 |
| `robotEvents` | 机器人出生（id + spawnHp） |
| `wall` | 墙数量与总血量变化 |

---

## 请求六：任务健康度的统计口径修正（重要）

**现状问题**：当前分析把 `task_ended reason=timeout` 直接记为「任务失败 / 0 分」。
但接口文档 `PlayerTask.timeoutRounds` 写明：**任务超时强制结束时，按「此前提交过的
通过率最高的答案」结算积分与金币**——提交是累加制。一个 `timeout` session 只要提交过
答案就有分（可能是几十分），把它记为 0 会误诊根因（v1 路线图 P1-5）。

**请把任务健康度改为**：

| 指标 | 定义 | 数据源（我方 JSONL） |
|---|---|---|
| 提交率 | `submittedRound` 非空的 session 比例 | `round.task.submittedRound` / `task_answer_submit` 事件 |
| 平均通过率 | 各 session 最高通过率均值（判分侧的近似） | `totalScore` 增量归因 `scoreAttr.residual` |
| 真零分 session | **从未提交**（cmdRounds=0 或 无 `task_answer_submit`） | 同上 |

「提交率」应作为任务系统改进的第一验收指标（v1 目标 ≥ 80%），而不是「timeout 次数」。

---

## 请求七：A/B 拨盘对战实验

自本版本对应 commit 起，决策器内置 `CG_TUNE_*` 环境变量拨盘（进程启动时读取，
默认行为与当前 commit 完全一致）。请按 A/B 方式发起对战，验证以下假设：

| 实验 | 环境变量 | A 组（默认） | B 组 | 观察指标（`round.scoreAttr`） |
|---|---|---|---|---|
| 清怪缺口驱动资金排序 | `CG_TUNE_CLEAR_GAP` | `0` | `1` | kill / survival / residual 三项与基地存活天数 |
| 敌基地持续压制 | `CG_TUNE_STATION_FOCUS` | `1` | `0` | `volley.enemyStationHp` 下降速度、先拆胜场数 |

**要求**：每组至少 5 场（请求二）、多对手、issue 中给出**分项得分对比**（不止总分），
并标注每场使用的拨盘值（配合请求三的 base commit 字段）。

---

## 机器可读请求块

```yaml
agent_request:
  target: workflow-driver
  repo: gamelife1314/Competition
  action: improve_workflow
  version: 6
  changes:
    - id: issue_header_stats
      desc: extend improvement-issue header with cumulative stats
      placement: after "生成时间" line
      add_fields:
        - {name: current_rank, desc: rank among all teams,     example: "12 / 64"}
        - {name: total_wins,   desc: cumulative match wins,    example: 8}
        - {name: total_losses, desc: cumulative match losses,  example: 5}
        - {name: total_draws,  desc: cumulative match draws,   example: 1}
        - {name: win_rate,     desc: wins / total, 1 decimal,  example: "57.1%"}
    - id: battles_per_round
      desc: raise the number of battles initiated per round
      from: 3
      to: 5
    - id: battle_base_commit
      desc: record the code commit each battle was fought on
      per_battle: true
      add_fields:
        - {name: battle_base_commit, desc: "git commit hash + time", example: "b03b1c8 (2026-09-13 08:30)"}
    - id: full_logs
      desc: provide complete, untruncated battle data
      priority: highest
      provide:
        - ours_raw_jsonl          # our full per-round log, untruncated
        - enemy_raw_jsonl         # opponent log when obtainable
        - analysis_report_md      # workflow/analysis/*.md in full
        - issue_body_untruncated  # raise the ~200-char cut to >=2000
        - structured_summary_json # see schema in the document above
      note: our round event now carries errorDescs (judger error text) — extract directly
    - id: log_requirements_feedback
      desc: tell us which log fields/events the analysis needs
      request: >
        List (a) which information is currently missing and forces guesswork,
        (b) desired new event names and fields (with a JSON example),
        (c) which existing events are noise and can be dropped.
    - id: task_health_metric
      desc: score task health by submission rate + average pass rate, not "timeout = failure"
      metrics:
        - {name: submission_rate, source: "task_answer_submit events / task.submittedRound", target: ">= 80% of sessions"}
        - {name: avg_pass_rate,   source: "scoreAttr.residual attribution"}
        - {name: true_zero_sessions, definition: "sessions with NO submission (cmdRounds=0 or no task_answer_submit)"}
    - id: ab_dial_experiments
      desc: run A/B battles with CG_TUNE_* env overrides
      experiments:
        - {env: CG_TUNE_CLEAR_GAP, a: "0", b: "1", watch: "scoreAttr kill/survival/residual + station survival days"}
        - {env: CG_TUNE_STATION_FOCUS, a: "1", b: "0", watch: "volley.enemyStationHp drop speed, first-kill wins"}
      requirements: ">= 5 battles per group, multiple opponents, per-objective score breakdown in the issue, dial value recorded per battle"
```

---

## English summary

Please improve the battle workflow in five ways:

1. **Issue header stats** — cumulative rank, wins, losses, draws, win rate.
2. **Battles per round 3 → 5** — three samples cannot separate signal from noise.
3. **Battle base commit** — record the commit each battle ran on; the workflow's
   multi-stage delay means our HEAD has usually moved on by the time we read the
   issue.
4. **Full logs (highest priority)** — stop truncating the issue body at ~200
   chars; provide our complete raw JSONL, the opponent's log when obtainable,
   the full analysis report, and a structured per-match summary. The judge's
   `errors[].description` is the only schema signal we get, and our parser
   currently discards it.
5. **Log requirements feedback** — tell us what your analysis is missing, what
   new events/fields you want, and which existing events are noise.

---

*本文件由本地 agent 维护，用于向内部 code agent / workflow 传递改进需求。*
