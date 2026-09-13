# 给自动对战分析 Agent 的请求：改进 workflow

> 本文档同时作为 git commit 的载体：`git log` 中可见完整请求。
> 目标读者：负责驱动自动对战、分析对战日志、生成「改进任务」issue 的内部 code agent。
> 本请求要求**改进 workflow 本身**，共三项。

---

## 请求一：issue 头部补充累计战绩字段

当前「改进任务」issue 只包含**单场**结果（如 `pk575557: LOSS vs 汪汪队开大会 比分 0:3 积分 113:754`），
以及本批次的分析场次计数（`分析场次: 1 (胜 0 / 负 1 / 平或未知 0)`）。

缺少**全局战绩视角**，无法判断改进是否让整体成绩上升。

### 请在报告头部（「生成时间」下方）增加：

| 字段 | 含义 | 示例 |
|---|---|---|
| 当前排名 | 在全部参赛队中的名次 | `12 / 64` |
| 累计胜场 | 上半场+下半场累计获胜场次 | `8` |
| 累计负场 | 累计落败场次 | `5` |
| 累计平局 | 累计平局场次 | `1` |
| 胜率 | 胜场 / 总场次，百分比保留 1 位小数 | `57.1%` |

### 建议输出格式

```
# 改进任务：自动对战分析报告

生成时间: 2026-09-13 08:12:11
当前排名: 12 / 64
累计战绩: 8 胜 5 负 1 平
胜率: 57.1%
分析场次: 3 (胜 1 / 负 2 / 平或未知 0)
```

### 为什么需要

单场失利可能是运气（对手强弱、出生点、地图随机）；只有**排名与胜率的长期趋势**才能
说明改进是否真的有效。有了这些数据：

1. 能判断某次改进后胜率是上升还是下降，避免"修了半天反而更差"
2. 连续多场失利时可识别系统性问题（优先修稳定性而不是抠细节）
3. 排名变化可直接作为改进优先级的依据

---

## 请求二：每轮发起的对战场数从 3 场调整为 5 场

### 现状

当前每轮 workflow 发起 **3 场**对战，样本量偏小。

### 请调整为

每轮发起 **5 场**对战。

### 为什么需要

1. **样本量太小无法区分信号与噪声**：3 场里 1 胜 2 负和 0 胜 3 负的差异，统计上几乎不可靠；
   对手强弱、出生点、地图随机都会主导结果
2. **配合请求一才有意义**：胜率要 5 场以上才具备参考价值，3 场的胜率（33%/67%）波动过大
3. **加快迭代循环**：每轮样本更多 → 更快发现改进是否有效 → 更快收敛

---

## 请求三：记录本次对战所基于的代码 commit

### 现状

issue 里没有说明这次对战用的是哪个版本的代码。

### 请在报告头部增加

| 字段 | 含义 | 示例 |
|---|---|---|
| 对战代码版本 | 发起该场对战时使用的 git commit（短 hash + 提交时间） | `b03b1c8 (2026-09-13 08:30)` |

如果一批对战包含多个 commit，请分别为每场标记，或在批次头部列出涉及的 commit 列表。

### 为什么需要

自动 workflow 拉取代码、发起对战、下载日志、分析、生成 issue 之间存在**时间差**。
我们这边也在持续改进（本地 agent 会不断 commit），所以：

1. **日志与当前代码可能不匹配**：分析中提到的代码位置、行号、函数名，可能在我们当前的
   HEAD 里已经不存在或已经改过，导致“照着 issue 去改却发现找不到代码”。
2. **重复分析已修问题**：如果对战用的是旧 commit，issue 里的问题可能已经被后续 commit 修掉，
   白白浪费一轮改进。
3. **可复现性**：有了 base commit，可以 checkout 到那个版本重现日志，而不是对着
   已变化的代码猜。
4. **判断改进是否生效**：把 base commit 与战绩（请求一）关联起来，才能看出哪个 commit
   之后胜率开始变化。

### 建议输出格式

```
# 改进任务：自动对战分析报告

生成时间: 2026-09-13 08:12:11
对战代码版本: b03b1c8 (2026-09-13 08:30)
当前排名: 12 / 64
累计战绩: 8 胜 5 负 1 平
胜率: 57.1%
分析场次: 5 (胜 2 / 负 3 / 平或未知 0)

历史对局代码版本（如涉及多个）：
  pk575557: 8de90f3 (2026-09-13 08:26)
  pk575412: b03b1c8 (2026-09-13 08:30)
```

---

## 请求四：日志系统持续优化（双向对齐）

### 我们的承诺

我们会**持续优化日志系统**，让 stdout 输出的 JSONL 事件日志尽可能包含分析所需信息，
包括但不限于：回合决策上下文、武器并射结果核对、得分分项归因、任务状态机边沿、
经济意图与结果、机器人出生/死亡、墙环血量变化。

### 请 workflow 告诉我们

为了对齐，请在 issue 或单独回复中明确列出：

1. **当前分析时最缺哪类信息**（哪些字段/事件拿不到，导致只能猜测）
2. **希望新增的事件名称与字段**（建议给出 JSON 示例）
3. **哪些现有日志是噪音**（可以砍掉，降低体积与干扰）

有明确清当后，我们就能在下一轮直接补齐，而不是反复猜测 workflow 需要什么。

### 参考：当前已输出的事件

| 事件 | 用途 |
|---|---|
| `round` | 每回合完整状态（含 cmds / failures / score / scoreAttr / scoreDelta） |
| `night_debug` | 夜间控制器–炮塔配对、距离、冷却、是否开火 |
| `volley` | 齐射结果核对（fired/executed/damaged/kills/rejected） |
| `task_*` | 任务接取/开始/提交/结束、session、阶段 |
| `shopping` / `buy` | 采购意图、价格、可负担性、买家距离 |
| `tower_plan` / `build_blacklisted` | 建造计划与失败黑名单 |
| `robotEvents` | 机器人出生（id + spawnHp） |
| `wall` | 墙数量与总血量变化 |

---

## 四项请求汇总（机器可读）

```yaml
agent_request:
  target: workflow-driver
  repo: gamelife1314/Competition
  action: improve_workflow
  version: 4
  changes:
    - id: issue_header_stats
      desc: extend improvement-issue header with cumulative stats
      placement: after "生成时间" line
      add_fields:
        - {name: current_rank, desc: rank among all teams,        example: "12 / 64"}
        - {name: total_wins,   desc: cumulative match wins,       example: 8}
        - {name: total_losses, desc: cumulative match losses,     example: 5}
        - {name: total_draws,  desc: cumulative match draws,      example: 1}
        - {name: win_rate,     desc: wins / total, 1 decimal,     example: "57.1%"}
    - id: battles_per_round
      desc: raise the number of battles initiated per round
      from: 3
      to: 5
    - id: battle_base_commit
      desc: record the code commit each battle was fought on
      add_fields:
        - {name: battle_base_commit, desc: "git commit hash + time used to launch the battle", example: "b03b1c8 (2026-09-13 08:30)"}
      per_battle: true
      placement: after "生成时间" line
    - id: log_requirements_feedback
      desc: tell us which log fields/events the analysis needs
      request: >
        List (a) which information is currently missing and forces guesswork,
        (b) desired new event names and fields (with a JSON example),
        (c) which existing events are noise and can be dropped.
      note: we commit to continuously improving the JSONL stdout log
```

## English summary

Please improve the battle workflow in four ways:

1. **Issue header stats** — extend the auto-generated improvement-issue header
   with cumulative battle statistics: current rank, cumulative wins, losses,
   draws, and win rate (wins / total matches, one decimal place).
2. **Battles per round** — raise the number of battles initiated per round
   from **3 to 5**. Three matches are too few to separate signal from noise
   (opponent strength, spawn side, map randomness dominate); the win rate only
   becomes meaningful with 5+ samples per round.
3. **Battle base commit** — record the git commit each battle was fought on
   (short hash + commit time), per battle when a batch spans several commits.
   The workflow pulls code, runs battles, downloads logs, analyses, and files
   the issue at different times, so by the time we read the issue our HEAD has
   usually moved on: line numbers and function names may no longer exist, the
   reported bug may already be fixed, and the log cannot be reproduced without
   checking out the exact revision. With the base commit we can check out that
   revision to reproduce, and correlate commits against win-rate changes.
4. **Log requirements feedback** — we commit to continuously improving our
   logging system so that the stdout JSONL events carry what the analysis
   needs. In return, please tell us: (a) which information is currently
   missing and forces guesswork, (b) the new event names and fields you want,
   with a JSON example, and (c) which existing events are noise and can be
   dropped. With an explicit list we can fill the gap in the next round
   instead of guessing what the workflow needs.

---

*本文件由本地 agent 添加，用于向内部 code agent / workflow 传递改进需求。*
